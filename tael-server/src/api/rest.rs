use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, RawQuery, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::cluster::{ClusterCoordinator, EpochFencer};
use crate::log_bus::LogBus;
use crate::span_bus::SpanBus;
use crate::storage::models::{LogQuery, MetricQuery, TraceQuery};
use crate::storage::{BlobStore, Store, WAL_EPOCH_HEADER};

#[derive(Clone)]
struct AppState {
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    bus: Arc<SpanBus>,
    log_bus: Arc<LogBus>,
    /// Cluster coordinator: `Some` when this node runs in a coordinated cluster
    /// (backs the `/internal/cluster` status endpoint). `None` when off.
    cluster: Option<Arc<ClusterCoordinator>>,
    /// Standby-side epoch gate for WAL replication (the coordinator's fencer).
    /// `None` keeps replication unfenced (single leader / tests).
    wal_fencer: Option<Arc<EpochFencer>>,
}

pub fn router(
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    bus: Arc<SpanBus>,
    log_bus: Arc<LogBus>,
    cluster: Option<Arc<ClusterCoordinator>>,
) -> Router {
    let wal_fencer = cluster.as_ref().map(|c| c.fencer());
    let state = AppState {
        store,
        blobs,
        bus,
        log_bus,
        cluster,
        wal_fencer,
    };
    Router::new()
        .route("/api/v1/traces", get(query_traces))
        .route("/api/v1/traces/live", get(live_traces))
        .route("/api/v1/traces/{trace_id}", get(get_trace))
        .route("/api/v1/services", get(list_services))
        .route(
            "/api/v1/traces/{trace_id}/comments",
            get(get_comments).post(add_comment),
        )
        .route("/api/v1/logs", get(query_logs))
        .route("/api/v1/logs/live", get(live_logs))
        .route("/api/v1/metrics", get(query_metrics))
        .route("/api/v1/metrics/query", get(promql_query))
        .route("/api/v1/summary", get(query_summary))
        .route("/api/v1/anomalies", get(query_anomalies))
        .route("/api/v1/correlate", get(query_correlate))
        .route("/api/v1/sql", get(query_sql))
        .route("/api/v1/blobs/{sha256}", get(get_blob))
        .route("/api/v1/write", post(prom_remote_write))
        .route("/internal/wal/records", post(apply_wal_record))
        .route("/internal/cluster", get(cluster_status))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// Resolve a content-addressed payload (LLM prompt/completion, or an oversized
/// log body) by its sha256. Returns the raw bytes as `text/plain`.
async fn get_blob(
    State(state): State<AppState>,
    Path(sha256): Path<String>,
) -> impl IntoResponse {
    match state.blobs.get(&sha256) {
        Ok(Some(bytes)) => (StatusCode::OK, bytes).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "blob not found" })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "get_blob failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct TraceQueryParams {
    service: Option<String>,
    operation: Option<String>,
    min_duration_ms: Option<f64>,
    max_duration_ms: Option<f64>,
    status: Option<String>,
    last: Option<String>,
    limit: Option<u32>,
    text: Option<String>,
}

fn parse_duration_to_seconds(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('s') {
        rest.parse().ok()
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.parse::<i64>().ok().map(|v| v * 60)
    } else if let Some(rest) = s.strip_suffix('h') {
        rest.parse::<i64>().ok().map(|v| v * 3600)
    } else if let Some(rest) = s.strip_suffix('d') {
        rest.parse::<i64>().ok().map(|v| v * 86400)
    } else {
        s.parse().ok()
    }
}

async fn query_traces(
    State(state): State<AppState>,
    Query(params): Query<TraceQueryParams>,
    RawQuery(raw): RawQuery,
) -> impl IntoResponse {
    let query = TraceQuery {
        service: params.service,
        operation: params.operation,
        min_duration_ms: params.min_duration_ms,
        max_duration_ms: params.max_duration_ms,
        status: params.status,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: params.limit,
        attributes: parse_attribute_params(raw.as_deref()),
        text: params.text,
    };

    match state.store.query_traces(&query) {
        Ok(spans) => (StatusCode::OK, axum::Json(serde_json::json!({ "spans": spans }))),
        Err(e) => {
            tracing::error!(error = %e, "query_traces failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

/// Pull repeated `attribute=key=value` pairs out of a raw query string.
/// `serde_urlencoded` (axum's default Query parser) keeps only the last value
/// for duplicate keys, so we re-parse the raw string to collect all of them.
fn parse_attribute_params(raw: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    form_urlencoded::parse(raw.as_bytes())
        .filter(|(k, _)| k == "attribute")
        .filter_map(|(_, v)| {
            let (key, value) = v.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct LiveQueryParams {
    service: Option<String>,
    status: Option<String>,
}

async fn live_traces(
    State(state): State<AppState>,
    Query(params): Query<LiveQueryParams>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.bus.subscribe();
    let service_filter = params.service;
    let status_filter = params.status;

    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        let json = result.ok()?;
        let filtered = filter_span_batch(&json, service_filter.as_deref(), status_filter.as_deref());
        filtered.map(|data| Ok::<_, Infallible>(Event::default().data(data)))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn filter_span_batch(
    json: &str,
    service: Option<&str>,
    status: Option<&str>,
) -> Option<String> {
    if service.is_none() && status.is_none() {
        return Some(json.to_string());
    }

    let spans: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let filtered: Vec<&serde_json::Value> = spans
        .iter()
        .filter(|s| {
            if let Some(svc) = service {
                if s["service"].as_str() != Some(svc) {
                    return false;
                }
            }
            if let Some(st) = status {
                if s["status"].as_str() != Some(st) {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        return None;
    }

    serde_json::to_string(&filtered).ok()
}

async fn get_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get_trace(&trace_id) {
        Ok(spans) if spans.is_empty() => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "trace not found" })),
        ),
        Ok(spans) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "trace_id": trace_id,
                "span_count": spans.len(),
                "spans": spans,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn list_services(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.list_services() {
        Ok(services) => (StatusCode::OK, axum::Json(serde_json::json!({ "services": services }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct AddCommentBody {
    author: Option<String>,
    body: String,
    span_id: Option<String>,
}

async fn add_comment(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
    Json(payload): Json<AddCommentBody>,
) -> impl IntoResponse {
    let author = payload.author.as_deref().unwrap_or("anonymous");
    match state.store.add_comment(&trace_id, payload.span_id.as_deref(), author, &payload.body) {
        Ok(comment) => (StatusCode::CREATED, axum::Json(serde_json::json!({ "comment": comment }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn get_comments(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get_comments(&trace_id) {
        Ok(comments) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "comments": comments, "count": comments.len() })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

// ── Log endpoints ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct LogQueryParams {
    service: Option<String>,
    severity: Option<String>,
    body_contains: Option<String>,
    trace_id: Option<String>,
    last: Option<String>,
    limit: Option<u32>,
}

async fn query_logs(
    State(state): State<AppState>,
    Query(params): Query<LogQueryParams>,
) -> impl IntoResponse {
    let query = LogQuery {
        service: params.service,
        severity: params.severity,
        body_contains: params.body_contains,
        trace_id: params.trace_id,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: params.limit,
    };

    match state.store.query_logs(&query) {
        Ok(logs) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "logs": logs, "count": logs.len() })),
        ),
        Err(e) => {
            tracing::error!(error = %e, "query_logs failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct LiveLogParams {
    service: Option<String>,
    severity: Option<String>,
}

async fn live_logs(
    State(state): State<AppState>,
    Query(params): Query<LiveLogParams>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_bus.subscribe();
    let service_filter = params.service;
    let severity_filter = params.severity;

    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        let json = result.ok()?;
        let filtered =
            filter_log_batch(&json, service_filter.as_deref(), severity_filter.as_deref());
        filtered.map(|data| Ok::<_, Infallible>(Event::default().data(data)))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn filter_log_batch(
    json: &str,
    service: Option<&str>,
    severity: Option<&str>,
) -> Option<String> {
    if service.is_none() && severity.is_none() {
        return Some(json.to_string());
    }

    let logs: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let filtered: Vec<&serde_json::Value> = logs
        .iter()
        .filter(|l| {
            if let Some(svc) = service {
                if l["service"].as_str() != Some(svc) {
                    return false;
                }
            }
            if let Some(sev) = severity {
                if l["severity"].as_str() != Some(sev) {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        return None;
    }

    serde_json::to_string(&filtered).ok()
}

// ── Metric endpoints ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct MetricQueryParams {
    service: Option<String>,
    name: Option<String>,
    metric_type: Option<String>,
    last: Option<String>,
    limit: Option<u32>,
}

async fn query_metrics(
    State(state): State<AppState>,
    Query(params): Query<MetricQueryParams>,
) -> impl IntoResponse {
    let query = MetricQuery {
        service: params.service,
        name: params.name,
        metric_type: params.metric_type,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: params.limit,
    };

    match state.store.query_metrics(&query) {
        Ok(metrics) => (
            StatusCode::OK,
            axum::Json(
                serde_json::json!({ "metrics": metrics, "count": metrics.len() }),
            ),
        ),
        Err(e) => {
            tracing::error!(error = %e, "query_metrics failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct PromqlParams {
    query: String,
    last: Option<String>,
}

async fn promql_query(
    State(state): State<AppState>,
    Query(params): Query<PromqlParams>,
) -> impl IntoResponse {
    let lookback = params
        .last
        .as_deref()
        .and_then(parse_duration_to_seconds)
        .unwrap_or(300);

    let expr = match crate::promql::parse(&params.query) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": format!("parse error: {e}") })),
            );
        }
    };

    match crate::promql::evaluate(state.store.as_ref(), &expr, lookback) {
        Ok(series) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "query": params.query,
                "series": series,
                "count": series.len(),
            })),
        ),
        Err(e) => {
            tracing::error!(error = %e, "promql evaluate failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct SummaryParams {
    last: Option<String>,
    service: Option<String>,
}

async fn query_summary(
    State(state): State<AppState>,
    Query(params): Query<SummaryParams>,
) -> impl IntoResponse {
    let last_seconds = params
        .last
        .as_deref()
        .and_then(parse_duration_to_seconds)
        .unwrap_or(3600);

    match state.store.query_summary(last_seconds, params.service.as_deref()) {
        Ok(report) => (StatusCode::OK, axum::Json(serde_json::to_value(&report).unwrap())),
        Err(e) => {
            tracing::error!(error = %e, "query_summary failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnomalyParams {
    last: Option<String>,
    baseline: Option<String>,
    service: Option<String>,
}

async fn query_anomalies(
    State(state): State<AppState>,
    Query(params): Query<AnomalyParams>,
) -> impl IntoResponse {
    let current_seconds = params
        .last
        .as_deref()
        .and_then(parse_duration_to_seconds)
        .unwrap_or(3600);
    let baseline_seconds = params
        .baseline
        .as_deref()
        .and_then(parse_duration_to_seconds)
        .unwrap_or(current_seconds * 6);

    match state
        .store
        .query_anomalies(current_seconds, baseline_seconds, params.service.as_deref())
    {
        Ok(report) => (StatusCode::OK, axum::Json(serde_json::to_value(&report).unwrap())),
        Err(e) => {
            tracing::error!(error = %e, "query_anomalies failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct CorrelateParams {
    trace: String,
}

async fn query_correlate(
    State(state): State<AppState>,
    Query(params): Query<CorrelateParams>,
) -> impl IntoResponse {
    match state.store.query_correlate(&params.trace) {
        Ok(Some(report)) => (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&report).unwrap()),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "trace not found" })),
        ),
        Err(e) => {
            tracing::error!(error = %e, "query_correlate failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

#[derive(Debug, Deserialize)]
struct SqlParams {
    q: String,
}

async fn query_sql(
    State(state): State<AppState>,
    Query(params): Query<SqlParams>,
) -> impl IntoResponse {
    match state.store.query_sql(&params.q) {
        Ok(rows) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "rows": rows, "count": rows.len() })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn prom_remote_write(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    crate::ingest::prom_remote_write::handle_write(state.store, body).await
}

/// WAL replication ingress: a standby receives one framed WAL record
/// (`[version][tag][json]`) shipped from a leader and applies it to local state
/// (`docs/tael-server-scaling-ha.md` §5.1). Internal endpoint — firewall it to
/// the leader→standby network in production.
///
/// When cluster coordination is on, the `x-tael-wal-epoch` header carries the
/// leader's epoch and is checked against the standby's fencer: a record from a
/// deposed leader (stale epoch) is rejected with 409 so it can't corrupt state.
/// Returns 202 on apply, 409 if fenced, 422 if this store can't be a standby.
async fn apply_wal_record(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Some(fencer) = &state.wal_fencer {
        let epoch = headers
            .get(WAL_EPOCH_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        if !fencer.check_and_advance(epoch) {
            tracing::warn!(
                epoch,
                highest = fencer.highest(),
                "fenced stale-leader WAL record"
            );
            return (
                StatusCode::CONFLICT,
                axum::Json(serde_json::json!({
                    "error": "fenced: record epoch is older than the current leader",
                    "epoch": epoch,
                    "current": fencer.highest(),
                })),
            )
                .into_response();
        }
    }
    match state.store.apply_framed_wal(&body) {
        Ok(()) => (StatusCode::ACCEPTED, "applied").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "WAL replication apply failed");
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// Liveness probe: the process is up and the HTTP server is serving. Always
/// 200 — it does not touch the store (see `/readyz` for that).
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe: `200 ready` when the store can serve requests, else `503`.
/// For a local engine this is trivially ready once constructed; for a
/// `FanoutStore` query tier it reflects shard reachability
/// (`docs/tael-server-scaling-ha.md` §5.4). Wire k8s/LB readiness here so a
/// node that can't reach its dependencies is drained from rotation.
/// Cluster status: this node's id, whether it's the elected leader, and its
/// current epoch. `enabled: false` when coordination is off. Useful for
/// operating failover (which node leads, what epoch) — see §5.1.
async fn cluster_status(State(state): State<AppState>) -> impl IntoResponse {
    match &state.cluster {
        Some(c) => axum::Json(serde_json::json!({
            "enabled": true,
            "node_id": c.node_id(),
            "is_leader": c.is_leader(),
            "epoch": c.current_epoch(),
        })),
        None => axum::Json(serde_json::json!({ "enabled": false })),
    }
}

async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.health() {
        Ok(()) => (StatusCode::OK, "ready"),
        Err(e) => {
            tracing::warn!(error = %e, "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{
        AnomalyReport, CorrelateReport, LogRecord, MetricPoint, ServiceInfo, Span, SummaryReport,
        TraceComment,
    };

    /// Minimal store whose WAL apply always succeeds, so the test isolates the
    /// endpoint's fencing decision from the storage engine.
    struct OkApplyStore;
    impl Store for OkApplyStore {
        fn insert_spans(&self, _: &[Span]) -> anyhow::Result<()> {
            Ok(())
        }
        fn query_traces(&self, _: &TraceQuery) -> anyhow::Result<Vec<Span>> {
            Ok(vec![])
        }
        fn get_trace(&self, _: &str) -> anyhow::Result<Vec<Span>> {
            Ok(vec![])
        }
        fn list_services(&self) -> anyhow::Result<Vec<ServiceInfo>> {
            Ok(vec![])
        }
        fn add_comment(
            &self,
            _: &str,
            _: Option<&str>,
            _: &str,
            _: &str,
        ) -> anyhow::Result<TraceComment> {
            anyhow::bail!("unused")
        }
        fn get_comments(&self, _: &str) -> anyhow::Result<Vec<TraceComment>> {
            Ok(vec![])
        }
        fn insert_logs(&self, _: &[LogRecord]) -> anyhow::Result<()> {
            Ok(())
        }
        fn query_logs(&self, _: &LogQuery) -> anyhow::Result<Vec<LogRecord>> {
            Ok(vec![])
        }
        fn insert_metrics(&self, _: &[MetricPoint]) -> anyhow::Result<()> {
            Ok(())
        }
        fn query_metrics(&self, _: &MetricQuery) -> anyhow::Result<Vec<MetricPoint>> {
            Ok(vec![])
        }
        fn query_summary(&self, _: i64, _: Option<&str>) -> anyhow::Result<SummaryReport> {
            anyhow::bail!("unused")
        }
        fn query_anomalies(
            &self,
            _: i64,
            _: i64,
            _: Option<&str>,
        ) -> anyhow::Result<AnomalyReport> {
            anyhow::bail!("unused")
        }
        fn query_correlate(&self, _: &str) -> anyhow::Result<Option<CorrelateReport>> {
            Ok(None)
        }
        fn query_sql(&self, _: &str) -> anyhow::Result<Vec<serde_json::Value>> {
            Ok(vec![])
        }
        fn apply_framed_wal(&self, _: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn state_with_fencer(fencer: Arc<EpochFencer>) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        AppState {
            store: Arc::new(OkApplyStore),
            blobs: Arc::new(BlobStore::new(dir.path().to_str().unwrap()).unwrap()),
            bus: Arc::new(SpanBus::new().unwrap()),
            log_bus: Arc::new(LogBus::new().unwrap()),
            cluster: None,
            wal_fencer: Some(fencer),
        }
    }

    fn headers_with_epoch(epoch: u64) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert(WAL_EPOCH_HEADER, epoch.to_string().parse().unwrap());
        h
    }

    #[tokio::test]
    async fn wal_endpoint_fences_a_deposed_leaders_stale_epoch() {
        let fencer = Arc::new(EpochFencer::new());
        let state = state_with_fencer(Arc::clone(&fencer));

        // Current leader (epoch 5): accepted, advances the gate.
        let r = apply_wal_record(State(state.clone()), headers_with_epoch(5), Bytes::from_static(b"x"))
            .await
            .into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);

        // Deposed leader still shipping at epoch 3: fenced out.
        let r = apply_wal_record(State(state.clone()), headers_with_epoch(3), Bytes::from_static(b"x"))
            .await
            .into_response();
        assert_eq!(r.status(), StatusCode::CONFLICT);

        // The current leader's ongoing stream (epoch 5) keeps flowing.
        let r = apply_wal_record(State(state), headers_with_epoch(5), Bytes::from_static(b"x"))
            .await
            .into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }
}
