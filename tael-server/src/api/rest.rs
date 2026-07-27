use std::collections::{BTreeMap, HashMap, HashSet};
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
    routing::{any, get, post},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::cluster::{ClusterCoordinator, EpochFencer};
use crate::log_bus::LogBus;
use crate::span_bus::SpanBus;
use crate::storage::models::{
    LogQuery, MetricPoint, MetricQuery, MetricType, Span, SpanStatus, TraceComment, TraceQuery,
};
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
    /// Alert rules and their live event feed.
    alerts: Arc<crate::alerts::AlertStore>,
    /// Online scoring rules.
    scores: Arc<crate::scoring::ScoreRuleStore>,
    /// Server-managed eval case suites.
    suites: Arc<crate::suites::SuiteStore>,
    /// Data directory, for the stores that read files directly.
    data_dir: String,
    /// Whether reads and writes are scoped by the caller's tenant.
    multi_tenant: bool,
}

pub fn router(
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    bus: Arc<SpanBus>,
    log_bus: Arc<LogBus>,
    cluster: Option<Arc<ClusterCoordinator>>,
    alerts: Arc<crate::alerts::AlertStore>,
    scores: Arc<crate::scoring::ScoreRuleStore>,
    suites: Arc<crate::suites::SuiteStore>,
    data_dir: String,
    multi_tenant: bool,
) -> Router {
    let wal_fencer = cluster.as_ref().map(|c| c.fencer());
    let state = AppState {
        store,
        blobs,
        bus,
        log_bus,
        cluster,
        wal_fencer,
        alerts,
        scores,
        suites,
        data_dir,
        multi_tenant,
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
        .route("/api/v1/comments", get(list_comments))
        .route("/api/v1/logs", get(query_logs))
        .route("/api/v1/logs/live", get(live_logs))
        .route("/api/v1/metrics", get(query_metrics))
        .route("/api/v1/metrics/query", get(promql_query))
        .route("/api/v1/metrics/rollups", get(query_rollups))
        .route("/api/v1/summary", get(query_summary))
        .route("/api/v1/anomalies", get(query_anomalies))
        .route("/api/v1/correlate", get(query_correlate))
        .route("/api/v1/topology", get(query_topology))
        .route("/api/v1/similar/{trace_id}", get(similar_traces))
        .route("/api/v1/cluster", get(cluster_traces))
        .route("/api/v1/embed", post(build_embeddings))
        .route("/api/v1/diff", get(query_diff))
        .route("/api/v1/metrics/{name}", get(get_metric))
        .route("/api/v1/sql", get(query_sql))
        .route("/api/v1/alerts", get(list_alerts).post(create_alert))
        .route("/api/v1/alerts/{name}", axum::routing::delete(delete_alert))
        .route("/api/v1/alerts/events", get(alert_events))
        .route("/api/v1/alerts/live", get(live_alerts))
        .route(
            "/api/v1/scores/rules",
            get(list_score_rules).post(create_score_rule),
        )
        .route(
            "/api/v1/scores/rules/{name}",
            axum::routing::delete(delete_score_rule),
        )
        .route("/api/v1/evals/runs", get(eval_runs))
        .route("/api/v1/evals/runs/{run_id}", get(eval_run))
        .route("/api/v1/evals/runs/{run_id}/cases", get(eval_cases))
        .route("/api/v1/evals/runs/{run_id}/scores", get(eval_scores))
        .route("/api/v1/evals/runs/{run_id}/compare", get(eval_compare))
        .route("/api/v1/evals/suites", get(list_suites))
        .route(
            "/api/v1/evals/suites/{name}",
            get(get_suite).post(push_suite),
        )
        .route(
            "/api/v1/evals/suites/{name}/snapshots",
            post(snapshot_suite),
        )
        .route("/api/v1/evals/suites/diff", get(diff_suites))
        .route("/api/v1/evals/scores", post(eval_add_score))
        .route("/api/v1/evals/runner-spans", post(eval_add_runner_span))
        .route("/api/v1/blobs", post(put_blob))
        .route("/api/v1/blobs/{sha256}", get(get_blob))
        .route("/api/v1/write", post(prom_remote_write))
        // Datadog trace-agent (dd-trace) intake, also usable through this
        // listener via DD_TRACE_AGENT_URL. See `ingest::datadog`.
        .merge(dd_routes())
        .route("/internal/wal/records", post(apply_wal_record))
        .route("/internal/cluster", get(cluster_status))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// The Datadog trace-agent endpoint set (see `ingest::datadog`). Mounted on
/// the main REST router and, via [`dd_router`], on the dedicated agent-port
/// listener.
fn dd_routes() -> Router<AppState> {
    Router::new()
        .route("/info", get(dd_info))
        .route("/v0.3/traces", post(dd_traces_v04).put(dd_traces_v04))
        .route("/v0.4/traces", post(dd_traces_v04).put(dd_traces_v04))
        .route("/v0.5/traces", post(dd_traces_v05).put(dd_traces_v05))
        .route("/v0.6/stats", post(dd_discard).put(dd_discard))
        .route("/telemetry/proxy/{*path}", any(dd_discard))
}

/// Standalone router for the dedicated Datadog agent-port listener (default
/// `127.0.0.1:8126`), so dd-trace clients work with zero configuration. Serves
/// only the trace-agent surface (plus `/healthz`), not the query API.
pub fn dd_router(
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    bus: Arc<SpanBus>,
    log_bus: Arc<LogBus>,
) -> Router {
    let state = AppState {
        store,
        blobs,
        bus,
        log_bus,
        cluster: None,
        wal_fencer: None,
        // The dd-trace listener serves ingest only; it never reads alerts, but
        // shares AppState, so it gets an empty in-memory store.
        alerts: Arc::new(
            crate::alerts::AlertStore::open("")
                .unwrap_or_else(|_| unreachable!("empty-path alert store cannot fail to open")),
        ),
        scores: Arc::new(
            crate::scoring::ScoreRuleStore::open("")
                .unwrap_or_else(|_| unreachable!("empty-path score store cannot fail to open")),
        ),
        suites: Arc::new(
            crate::suites::SuiteStore::open("")
                .unwrap_or_else(|_| unreachable!("empty-path suite store cannot fail to open")),
        ),
        data_dir: String::new(),
        multi_tenant: false,
    };
    dd_routes()
        .route("/healthz", get(healthz))
        .with_state(state)
}

/// Resolve a content-addressed payload (LLM prompt/completion, or an oversized
/// log body) by its sha256. Returns the raw bytes as `text/plain`.
async fn get_blob(State(state): State<AppState>, Path(sha256): Path<String>) -> impl IntoResponse {
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

async fn put_blob(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    match state.blobs.put(&body) {
        Ok(sha256) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "sha256": sha256, "size": body.len() })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "put_blob failed");
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
    /// When true, the response also carries an `explain` object describing how
    /// the query executed. Off by default so the common path stays one scan.
    explain: Option<bool>,
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
    principal: Option<axum::Extension<crate::auth::Principal>>,
) -> impl IntoResponse {
    let principal = principal.map(|axum::Extension(p)| p);
    let attribute_filters = match parse_attribute_params(raw.as_deref()) {
        Ok(f) => f,
        // A malformed regex is the caller's mistake and must say so; silently
        // matching nothing would look like "no such traces".
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };
    let query = TraceQuery {
        service: params.service,
        operation: params.operation,
        min_duration_ms: params.min_duration_ms,
        max_duration_ms: params.max_duration_ms,
        status: params.status,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: params.limit,
        attributes: attribute_filters.exact,
        attributes_contains: attribute_filters.contains,
        attributes_regex: attribute_filters.regex,
        text: params.text,
        tenant: crate::tenancy::read_scope(state.multi_tenant, principal.as_ref()),
    };

    match state.store.query_traces(&query) {
        Ok(spans) => {
            let mut body = serde_json::json!({ "spans": spans });
            if params.explain.unwrap_or(false) {
                // An explain failure must not fail the query itself — the
                // caller still wants its results.
                let explain = state
                    .store
                    .explain_traces(&query)
                    .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
                body["explain"] = explain;
            }
            (StatusCode::OK, axum::Json(body))
        }
        Err(e) => {
            tracing::error!(error = %e, "query_traces failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "error": e.to_string() })),
            )
        }
    }
}

/// Span attribute filters, split by match kind.
#[derive(Debug, Default)]
struct AttributeFilters {
    exact: Vec<(String, String)>,
    contains: Vec<(String, String)>,
    regex: Vec<(String, String)>,
}

/// Pull repeated `attribute=key<op>value` pairs out of a raw query string.
///
/// `serde_urlencoded` (axum's default Query parser) keeps only the last value
/// for duplicate keys, so the raw string is re-parsed to collect all of them.
/// Three operators are recognized, longest first so `~=` is not read as `=`:
/// `k~=v` (contains), `k=~v` (regex), `k=v` (exact).
fn parse_attribute_params(raw: Option<&str>) -> anyhow::Result<AttributeFilters> {
    let mut filters = AttributeFilters::default();
    let Some(raw) = raw else {
        return Ok(filters);
    };
    for (_, spec) in form_urlencoded::parse(raw.as_bytes()).filter(|(k, _)| k == "attribute") {
        // `=~` must be tried before `=`, and `~=` before both, or the operator
        // character ends up inside the key or value.
        let parsed = if let Some((k, v)) = spec.split_once("~=") {
            Some((k, v, 'c'))
        } else if let Some((k, v)) = spec.split_once("=~") {
            Some((k, v, 'r'))
        } else {
            spec.split_once('=').map(|(k, v)| (k, v, 'e'))
        };
        let Some((key, value, kind)) = parsed else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        match kind {
            'c' => filters.contains.push((key.to_string(), value.to_string())),
            'r' => {
                regex::Regex::new(value)
                    .map_err(|e| anyhow::anyhow!("invalid regex for attribute `{key}`: {e}"))?;
                filters.regex.push((key.to_string(), value.to_string()));
            }
            _ => filters.exact.push((key.to_string(), value.to_string())),
        }
    }
    Ok(filters)
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
        let filtered =
            filter_span_batch(&json, service_filter.as_deref(), status_filter.as_deref());
        filtered.map(|data| Ok::<_, Infallible>(Event::default().data(data)))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn filter_span_batch(json: &str, service: Option<&str>, status: Option<&str>) -> Option<String> {
    if service.is_none() && status.is_none() {
        return Some(json.to_string());
    }

    let spans: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let filtered: Vec<&serde_json::Value> = spans
        .iter()
        .filter(|s| {
            if let Some(svc) = service
                && s["service"].as_str() != Some(svc)
            {
                return false;
            }
            if let Some(st) = status
                && s["status"].as_str() != Some(st)
            {
                return false;
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
        Ok(services) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "services": services })),
        ),
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
    match state
        .store
        .add_comment(&trace_id, payload.span_id.as_deref(), author, &payload.body)
    {
        Ok(comment) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "comment": comment })),
        ),
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

#[derive(serde::Deserialize)]
struct ListCommentsParams {
    limit: Option<usize>,
}

/// The most recent comments across ALL traces, newest first. Powers the CLI's
/// reliability-loop scanners (`tael issue list`, `signal trend`, `eval suite
/// inspect`) on storage backends without the SQL layer.
async fn list_comments(
    State(state): State<AppState>,
    Query(params): Query<ListCommentsParams>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(1000).min(100_000);
    match state.store.list_comments(limit) {
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
        tenant: None,
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

fn filter_log_batch(json: &str, service: Option<&str>, severity: Option<&str>) -> Option<String> {
    if service.is_none() && severity.is_none() {
        return Some(json.to_string());
    }

    let logs: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let filtered: Vec<&serde_json::Value> = logs
        .iter()
        .filter(|l| {
            if let Some(svc) = service
                && l["service"].as_str() != Some(svc)
            {
                return false;
            }
            if let Some(sev) = severity
                && l["severity"].as_str() != Some(sev)
            {
                return false;
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
        tenant: None,
    };

    match state.store.query_metrics(&query) {
        Ok(metrics) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "metrics": metrics, "count": metrics.len() })),
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

    match state
        .store
        .query_summary(last_seconds, params.service.as_deref())
    {
        Ok(report) => (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&report).unwrap()),
        ),
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
        Ok(report) => (
            StatusCode::OK,
            axum::Json(serde_json::to_value(&report).unwrap()),
        ),
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
    principal: Option<axum::Extension<crate::auth::Principal>>,
) -> impl IntoResponse {
    let principal = principal.map(|axum::Extension(p)| p);
    // SQL runs against tables the query layer cannot filter per row without
    // rewriting arbitrary user queries, so under tenancy it is admin-only.
    // Refusing beats silently handing every tenant's rows to a `SELECT *`.
    if !crate::tenancy::may_use_sql(state.multi_tenant, principal.as_ref()) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "SQL is restricted to admin keys while multi-tenancy is enabled,                           because a SQL query cannot be scoped to one tenant. Use the                           structured query commands, which are scoped.",
            })),
        );
    }
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

// ── Eval endpoints ─────────────────────────────────────────────────

const EVAL_SCORE_METRIC: &str = "tael_eval_score";
const EVAL_QUERY_LIMIT: u32 = 50_000;

#[derive(Debug, Clone, Serialize)]
struct EvalRunSummary {
    run_id: String,
    suite_id: Option<String>,
    code_version: Option<String>,
    status: String,
    case_count: Option<usize>,
    observed_cases: usize,
    scored_cases: usize,
    passed_cases: usize,
    failed_cases: usize,
    pending_cases: Option<usize>,
    avg_scores: BTreeMap<String, f64>,
    cost_usd: f64,
    started_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalCaseSummary {
    run_id: String,
    suite_id: Option<String>,
    case_id: String,
    trace_id: Option<String>,
    status: String,
    started_at: Option<String>,
    updated_at: Option<String>,
    duration_ms: Option<f64>,
    scores: BTreeMap<String, f64>,
    labels: BTreeMap<String, String>,
    cost_usd: f64,
    comments: Vec<TraceComment>,
    #[serde(skip)]
    span_error: bool,
}

#[derive(Debug, Clone, Serialize)]
struct EvalScoreView {
    timestamp: String,
    run_id: String,
    suite_id: Option<String>,
    case_id: String,
    trace_id: Option<String>,
    span_id: Option<String>,
    metric: String,
    scorer: Option<String>,
    label: Option<String>,
    value: f64,
}

#[derive(Debug, Clone, Serialize)]
struct EvalCompareCase {
    case_id: String,
    metric: String,
    current_value: Option<f64>,
    baseline_value: Option<f64>,
    delta: Option<f64>,
    current_trace_id: Option<String>,
    baseline_trace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct EvalCompareReport {
    current_run_id: String,
    baseline_run_id: String,
    cases: Vec<EvalCompareCase>,
}

#[derive(Debug, Deserialize)]
struct EvalCompareParams {
    baseline: String,
}

#[derive(Debug, Deserialize)]
struct AddEvalScoreBody {
    suite_id: Option<String>,
    run_id: String,
    case_id: String,
    trace_id: Option<String>,
    span_id: Option<String>,
    metric: String,
    value: f64,
    scorer: Option<String>,
    label: Option<String>,
    rationale_sha256: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AddEvalRunnerSpanBody {
    suite_id: String,
    run_id: String,
    case_id: String,
    trace_id: String,
    span_id: String,
    case_index: Option<usize>,
    case_count: Option<usize>,
    code_version: Option<String>,
    status: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
    duration_ms: Option<f64>,
}

async fn eval_runs(State(state): State<AppState>) -> impl IntoResponse {
    match build_eval_snapshot(state.store.as_ref()) {
        Ok(snapshot) => {
            let mut runs: Vec<EvalRunSummary> = snapshot.runs.into_values().collect();
            runs.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "runs": runs, "count": runs.len() })),
            )
        }
        Err(e) => eval_error("eval_runs failed", e),
    }
}

async fn eval_run(State(state): State<AppState>, Path(run_id): Path<String>) -> impl IntoResponse {
    match build_eval_snapshot(state.store.as_ref()) {
        Ok(snapshot) => match snapshot.runs.get(&run_id) {
            Some(run) => (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "run": run })),
            ),
            None => (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({ "error": "eval run not found" })),
            ),
        },
        Err(e) => eval_error("eval_run failed", e),
    }
}

async fn eval_cases(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match build_eval_snapshot(state.store.as_ref()) {
        Ok(snapshot) => {
            let mut cases: Vec<EvalCaseSummary> = snapshot
                .cases
                .into_values()
                .filter(|c| c.run_id == run_id)
                .collect();
            cases.sort_by(|a, b| a.case_id.cmp(&b.case_id));
            let count = cases.len();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({ "run_id": run_id, "cases": cases, "count": count })),
            )
        }
        Err(e) => eval_error("eval_cases failed", e),
    }
}

async fn eval_scores(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match load_eval_scores(state.store.as_ref()) {
        Ok(scores) => {
            let scores: Vec<EvalScoreView> =
                scores.into_iter().filter(|s| s.run_id == run_id).collect();
            (
                StatusCode::OK,
                axum::Json(
                    serde_json::json!({ "run_id": run_id, "scores": scores, "count": scores.len() }),
                ),
            )
        }
        Err(e) => eval_error("eval_scores failed", e),
    }
}

async fn eval_compare(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
    Query(params): Query<EvalCompareParams>,
) -> impl IntoResponse {
    match build_eval_snapshot(state.store.as_ref()) {
        Ok(snapshot) => {
            let current = cases_by_metric(&snapshot, &run_id);
            let baseline = cases_by_metric(&snapshot, &params.baseline);
            let mut keys: HashSet<(String, String)> = current.keys().cloned().collect();
            keys.extend(baseline.keys().cloned());

            let mut cases = Vec::new();
            for (case_id, metric) in keys {
                let cur = current.get(&(case_id.clone(), metric.clone()));
                let base = baseline.get(&(case_id.clone(), metric.clone()));
                cases.push(EvalCompareCase {
                    case_id,
                    metric,
                    current_value: cur.map(|c| c.0),
                    baseline_value: base.map(|c| c.0),
                    delta: match (cur, base) {
                        (Some(c), Some(b)) => Some(c.0 - b.0),
                        _ => None,
                    },
                    current_trace_id: cur.and_then(|c| c.1.clone()),
                    baseline_trace_id: base.and_then(|c| c.1.clone()),
                });
            }
            cases.sort_by(|a, b| {
                a.case_id
                    .cmp(&b.case_id)
                    .then_with(|| a.metric.cmp(&b.metric))
            });

            let report = EvalCompareReport {
                current_run_id: run_id,
                baseline_run_id: params.baseline,
                cases,
            };
            (
                StatusCode::OK,
                axum::Json(serde_json::to_value(report).unwrap()),
            )
        }
        Err(e) => eval_error("eval_compare failed", e),
    }
}

async fn eval_add_score(
    State(state): State<AppState>,
    Json(payload): Json<AddEvalScoreBody>,
) -> impl IntoResponse {
    if payload.run_id.trim().is_empty()
        || payload.case_id.trim().is_empty()
        || payload.metric.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "run_id, case_id, and metric are required"
            })),
        );
    }

    let mut attrs = HashMap::new();
    if let Some(v) = payload.suite_id.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("suite_id".to_string(), v.to_string());
    }
    attrs.insert("run_id".to_string(), payload.run_id.clone());
    attrs.insert("case_id".to_string(), payload.case_id.clone());
    attrs.insert("metric".to_string(), payload.metric.clone());
    if let Some(v) = payload.trace_id.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("trace_id".to_string(), v.to_string());
    }
    if let Some(v) = payload.span_id.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("span_id".to_string(), v.to_string());
    }
    if let Some(v) = payload.scorer.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("scorer".to_string(), v.to_string());
    }
    if let Some(v) = payload.label.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("label".to_string(), v.to_string());
    }
    if let Some(v) = payload
        .rationale_sha256
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        attrs.insert("rationale_sha256".to_string(), v.to_string());
    }
    if let Some(v) = payload.source.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("source".to_string(), v.to_string());
    }

    let point = MetricPoint {
        timestamp: Utc::now(),
        service: "tael-eval".to_string(),
        name: EVAL_SCORE_METRIC.to_string(),
        metric_type: MetricType::Gauge,
        value: payload.value,
        unit: "score".to_string(),
        attributes: attrs,
        histogram: None,
    };

    match state.store.insert_metrics(std::slice::from_ref(&point)) {
        Ok(()) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "score": metric_to_eval_score(&point) })),
        ),
        Err(e) => eval_error("eval_add_score failed", e),
    }
}

async fn eval_add_runner_span(
    State(state): State<AppState>,
    Json(payload): Json<AddEvalRunnerSpanBody>,
) -> impl IntoResponse {
    if payload.suite_id.trim().is_empty()
        || payload.run_id.trim().is_empty()
        || payload.case_id.trim().is_empty()
        || payload.trace_id.trim().is_empty()
        || payload.span_id.trim().is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "suite_id, run_id, case_id, trace_id, and span_id are required"
            })),
        );
    }

    let start_time = payload
        .start_time
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let end_time = payload
        .end_time
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(start_time);
    let duration_ms = payload.duration_ms.unwrap_or_else(|| {
        end_time
            .signed_duration_since(start_time)
            .num_microseconds()
            .map(|us| us as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0)
    });

    let mut attrs = HashMap::new();
    attrs.insert("tael.eval.suite_id".to_string(), payload.suite_id.clone());
    attrs.insert("tael.eval.run_id".to_string(), payload.run_id.clone());
    attrs.insert("tael.eval.case_id".to_string(), payload.case_id.clone());
    attrs.insert("tael.eval.role".to_string(), "runner".to_string());
    if let Some(index) = payload.case_index {
        attrs.insert("tael.eval.case_index".to_string(), index.to_string());
    }
    if let Some(count) = payload.case_count {
        attrs.insert("tael.eval.case_count".to_string(), count.to_string());
    }
    if let Some(version) = payload.code_version.as_deref().filter(|s| !s.is_empty()) {
        attrs.insert("tael.eval.code_version".to_string(), version.to_string());
    }

    let span = Span {
        trace_id: payload.trace_id,
        span_id: payload.span_id,
        parent_span_id: None,
        service: "tael-eval-runner".to_string(),
        operation: "tael eval case".to_string(),
        start_time,
        end_time,
        duration_ms,
        status: payload
            .status
            .as_deref()
            .map(SpanStatus::from_str)
            .unwrap_or(SpanStatus::Unset),
        attributes: attrs,
        events: Vec::new(),
        kind: Default::default(),
        llm: None,
    };

    match state.store.insert_spans(std::slice::from_ref(&span)) {
        Ok(()) => {
            if let Err(e) = state.bus.publish(std::slice::from_ref(&span)) {
                tracing::warn!(error = %e, "failed to publish eval runner span to bus");
            }
            (
                StatusCode::CREATED,
                axum::Json(serde_json::json!({ "span": span })),
            )
        }
        Err(e) => eval_error("eval_add_runner_span failed", e),
    }
}

struct EvalSnapshot {
    runs: BTreeMap<String, EvalRunSummary>,
    cases: BTreeMap<(String, String), EvalCaseSummary>,
}

fn eval_error(
    context: &'static str,
    e: anyhow::Error,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    tracing::error!(error = %e, "{context}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": e.to_string() })),
    )
}

fn build_eval_snapshot(store: &dyn Store) -> anyhow::Result<EvalSnapshot> {
    let spans = store.query_traces(&TraceQuery {
        limit: Some(EVAL_QUERY_LIMIT),
        ..TraceQuery::default()
    })?;
    let scores = load_eval_scores(store)?;

    let mut cases: BTreeMap<(String, String), EvalCaseSummary> = BTreeMap::new();
    let mut run_case_counts: HashMap<String, usize> = HashMap::new();
    let mut run_code_versions: HashMap<String, String> = HashMap::new();

    for span in spans {
        let Some(run_id) = span.attributes.get("tael.eval.run_id").cloned() else {
            continue;
        };
        let Some(case_id) = span.attributes.get("tael.eval.case_id").cloned() else {
            continue;
        };
        let key = (run_id.clone(), case_id.clone());
        let suite_id = span.attributes.get("tael.eval.suite_id").cloned();
        if let Some(count) = span
            .attributes
            .get("tael.eval.case_count")
            .and_then(|v| v.parse::<usize>().ok())
        {
            run_case_counts
                .entry(run_id.clone())
                .and_modify(|n| *n = (*n).max(count))
                .or_insert(count);
        }
        if let Some(version) = span.attributes.get("tael.eval.code_version") {
            run_code_versions
                .entry(run_id.clone())
                .or_insert_with(|| version.clone());
        }

        let entry = cases.entry(key).or_insert_with(|| EvalCaseSummary {
            run_id: run_id.clone(),
            suite_id: suite_id.clone(),
            case_id: case_id.clone(),
            trace_id: Some(span.trace_id.clone()),
            status: "running".to_string(),
            started_at: Some(span.start_time.to_rfc3339()),
            updated_at: Some(span.end_time.to_rfc3339()),
            duration_ms: Some(span.duration_ms),
            scores: BTreeMap::new(),
            labels: BTreeMap::new(),
            cost_usd: 0.0,
            comments: Vec::new(),
            span_error: false,
        });

        if entry.suite_id.is_none() {
            entry.suite_id = suite_id;
        }
        if entry.trace_id.is_none() {
            entry.trace_id = Some(span.trace_id.clone());
        }
        merge_span_window(entry, &span);
    }

    for score in &scores {
        let key = (score.run_id.clone(), score.case_id.clone());
        let entry = cases.entry(key).or_insert_with(|| EvalCaseSummary {
            run_id: score.run_id.clone(),
            suite_id: score.suite_id.clone(),
            case_id: score.case_id.clone(),
            trace_id: score.trace_id.clone(),
            status: "scored".to_string(),
            started_at: Some(score.timestamp.clone()),
            updated_at: Some(score.timestamp.clone()),
            duration_ms: None,
            scores: BTreeMap::new(),
            labels: BTreeMap::new(),
            cost_usd: 0.0,
            comments: Vec::new(),
            span_error: false,
        });
        if entry.suite_id.is_none() {
            entry.suite_id = score.suite_id.clone();
        }
        if entry.trace_id.is_none() {
            entry.trace_id = score.trace_id.clone();
        }
        entry.scores.insert(score.metric.clone(), score.value);
        if let Some(label) = &score.label {
            entry.labels.insert(score.metric.clone(), label.clone());
        }
        if score.metric == "cost_usd" {
            entry.cost_usd += score.value;
        }
        entry.updated_at = max_string_time(entry.updated_at.take(), Some(score.timestamp.clone()));
    }

    for case in cases.values_mut() {
        case.status = infer_case_status(case);
        if let Some(trace_id) = &case.trace_id {
            case.comments = store.get_comments(trace_id).unwrap_or_default();
        }
    }

    let mut runs: BTreeMap<String, EvalRunSummary> = BTreeMap::new();
    for case in cases.values() {
        let run = runs
            .entry(case.run_id.clone())
            .or_insert_with(|| EvalRunSummary {
                run_id: case.run_id.clone(),
                suite_id: case.suite_id.clone(),
                code_version: run_code_versions.get(&case.run_id).cloned(),
                status: "unknown".to_string(),
                case_count: run_case_counts.get(&case.run_id).copied(),
                observed_cases: 0,
                scored_cases: 0,
                passed_cases: 0,
                failed_cases: 0,
                pending_cases: None,
                avg_scores: BTreeMap::new(),
                cost_usd: 0.0,
                started_at: case.started_at.clone(),
                updated_at: case.updated_at.clone(),
            });
        if run.suite_id.is_none() {
            run.suite_id = case.suite_id.clone();
        }
        run.observed_cases += usize::from(case.trace_id.is_some());
        run.scored_cases += usize::from(!case.scores.is_empty());
        run.passed_cases += usize::from(case.status == "pass");
        run.failed_cases += usize::from(case.status == "fail");
        run.cost_usd += case.cost_usd;
        run.started_at = min_string_time(run.started_at.take(), case.started_at.clone());
        run.updated_at = max_string_time(run.updated_at.take(), case.updated_at.clone());
    }

    let mut score_sums: HashMap<String, HashMap<String, (f64, usize)>> = HashMap::new();
    for case in cases.values() {
        let entry = score_sums.entry(case.run_id.clone()).or_default();
        for (metric, value) in &case.scores {
            if metric == "cost_usd" {
                continue;
            }
            entry
                .entry(metric.clone())
                .and_modify(|(sum, n)| {
                    *sum += value;
                    *n += 1;
                })
                .or_insert((*value, 1));
        }
    }
    for (run_id, metrics) in score_sums {
        if let Some(run) = runs.get_mut(&run_id) {
            for (metric, (sum, n)) in metrics {
                run.avg_scores.insert(metric, sum / n as f64);
            }
        }
    }

    for run in runs.values_mut() {
        run.pending_cases = run
            .case_count
            .map(|n| n.saturating_sub(run.observed_cases.max(run.scored_cases)));
        run.status = infer_run_status(run);
    }

    Ok(EvalSnapshot { runs, cases })
}

fn load_eval_scores(store: &dyn Store) -> anyhow::Result<Vec<EvalScoreView>> {
    let metrics = store.query_metrics(&MetricQuery {
        name: Some(EVAL_SCORE_METRIC.to_string()),
        limit: Some(EVAL_QUERY_LIMIT),
        ..MetricQuery::default()
    })?;
    Ok(metrics.iter().filter_map(metric_to_eval_score).collect())
}

fn metric_to_eval_score(point: &MetricPoint) -> Option<EvalScoreView> {
    let run_id = point.attributes.get("run_id")?.clone();
    let case_id = point.attributes.get("case_id")?.clone();
    let metric = point.attributes.get("metric")?.clone();
    Some(EvalScoreView {
        timestamp: point.timestamp.to_rfc3339(),
        run_id,
        suite_id: point.attributes.get("suite_id").cloned(),
        case_id,
        trace_id: point.attributes.get("trace_id").cloned(),
        span_id: point.attributes.get("span_id").cloned(),
        metric,
        scorer: point.attributes.get("scorer").cloned(),
        label: point.attributes.get("label").cloned(),
        value: point.value,
    })
}

fn merge_span_window(case: &mut EvalCaseSummary, span: &Span) {
    let start = span.start_time.to_rfc3339();
    let end = span.end_time.to_rfc3339();
    case.started_at = min_string_time(case.started_at.take(), Some(start));
    case.updated_at = max_string_time(case.updated_at.take(), Some(end));
    case.duration_ms = Some(case.duration_ms.unwrap_or(0.0).max(span.duration_ms));
    if span.status == SpanStatus::Error {
        case.span_error = true;
    }
}

fn min_string_time(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn max_string_time(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn infer_case_status(case: &EvalCaseSummary) -> String {
    if case.span_error {
        return "fail".to_string();
    }
    if case
        .labels
        .values()
        .any(|v| matches!(v.as_str(), "fail" | "failed" | "error"))
    {
        return "fail".to_string();
    }
    if case
        .labels
        .values()
        .any(|v| matches!(v.as_str(), "pass" | "passed" | "ok"))
    {
        return "pass".to_string();
    }
    if let Some(pass) = case.scores.get("pass") {
        return if *pass >= 1.0 { "pass" } else { "fail" }.to_string();
    }
    if let Some(correctness) = case.scores.get("correctness") {
        return if *correctness >= 1.0 { "pass" } else { "fail" }.to_string();
    }
    if case.trace_id.is_some() {
        "running".to_string()
    } else {
        "pending".to_string()
    }
}

fn infer_run_status(run: &EvalRunSummary) -> String {
    if run.failed_cases > 0 {
        return "failed".to_string();
    }
    if let Some(total) = run.case_count {
        if total > 0 && (run.scored_cases >= total || run.observed_cases >= total) {
            return "complete".to_string();
        }
        if run.observed_cases > 0 || run.scored_cases > 0 {
            return "running".to_string();
        }
    } else if run.observed_cases > 0 || run.scored_cases > 0 {
        return "running".to_string();
    }
    "unknown".to_string()
}

fn cases_by_metric(
    snapshot: &EvalSnapshot,
    run_id: &str,
) -> HashMap<(String, String), (f64, Option<String>)> {
    let mut out = HashMap::new();
    for case in snapshot.cases.values().filter(|c| c.run_id == run_id) {
        for (metric, value) in &case.scores {
            out.insert(
                (case.case_id.clone(), metric.clone()),
                (*value, case.trace_id.clone()),
            );
        }
    }
    out
}

async fn prom_remote_write(State(state): State<AppState>, body: Bytes) -> impl IntoResponse {
    crate::ingest::prom_remote_write::handle_write(state.store, body).await
}

// ── Datadog trace-agent (dd-trace) intake ───────────────────────────

async fn dd_info() -> impl IntoResponse {
    crate::ingest::datadog::handle_info()
}

async fn dd_traces_v04(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    crate::ingest::datadog::handle_traces(
        state.store,
        state.blobs,
        state.bus,
        crate::ingest::datadog::TracesVersion::V04,
        headers,
        body,
    )
    .await
}

async fn dd_traces_v05(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    crate::ingest::datadog::handle_traces(
        state.store,
        state.blobs,
        state.bus,
        crate::ingest::datadog::TracesVersion::V05,
        headers,
        body,
    )
    .await
}

/// Accept-and-discard for dd-trace background traffic (client stats,
/// instrumentation telemetry) so client loops don't log errors. The data
/// itself is derivable from the traces we already store.
async fn dd_discard() -> impl IntoResponse {
    StatusCode::OK
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
    use crate::storage::TaelBackend;
    use crate::storage::models::{
        AnomalyReport, CorrelateReport, LogRecord, MetricPoint, ServiceInfo, Span, SpanStatus,
        SummaryReport, TraceComment,
    };

    /// Minimal store that records inserted spans and whose WAL apply always
    /// succeeds, so tests isolate endpoint behavior from the storage engine.
    #[derive(Default)]
    struct OkApplyStore {
        inserted: std::sync::Mutex<Vec<Span>>,
    }
    impl Store for OkApplyStore {
        fn insert_spans(&self, spans: &[Span]) -> anyhow::Result<()> {
            self.inserted.lock().unwrap().extend_from_slice(spans);
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

    fn test_state(store: Arc<OkApplyStore>, fencer: Option<Arc<EpochFencer>>) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        AppState {
            store,
            blobs: Arc::new(BlobStore::new(path).unwrap()),
            bus: Arc::new(SpanBus::new().unwrap()),
            log_bus: Arc::new(LogBus::new().unwrap()),
            cluster: None,
            wal_fencer: fencer,
            alerts: Arc::new(crate::alerts::AlertStore::open(path).unwrap()),
            scores: Arc::new(crate::scoring::ScoreRuleStore::open(path).unwrap()),
            suites: Arc::new(crate::suites::SuiteStore::open(path).unwrap()),
            data_dir: path.to_string(),
            multi_tenant: false,
        }
    }

    fn state_with_fencer(fencer: Arc<EpochFencer>) -> AppState {
        test_state(Arc::new(OkApplyStore::default()), Some(fencer))
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
        let r = apply_wal_record(
            State(state.clone()),
            headers_with_epoch(5),
            Bytes::from_static(b"x"),
        )
        .await
        .into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);

        // Deposed leader still shipping at epoch 3: fenced out.
        let r = apply_wal_record(
            State(state.clone()),
            headers_with_epoch(3),
            Bytes::from_static(b"x"),
        )
        .await
        .into_response();
        assert_eq!(r.status(), StatusCode::CONFLICT);

        // The current leader's ongoing stream (epoch 5) keeps flowing.
        let r = apply_wal_record(
            State(state),
            headers_with_epoch(5),
            Bytes::from_static(b"x"),
        )
        .await
        .into_response();
        assert_eq!(r.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn datadog_v04_traces_are_ingested_and_acked() {
        let store = Arc::new(OkApplyStore::default());
        let state = test_state(Arc::clone(&store), None);

        // A minimal dd-trace v0.4 msgpack payload: one trace with one span.
        let span = rmpv::Value::Map(vec![
            (rmpv::Value::from("service"), rmpv::Value::from("billing")),
            (rmpv::Value::from("name"), rmpv::Value::from("http.request")),
            (rmpv::Value::from("trace_id"), rmpv::Value::from(42u64)),
            (rmpv::Value::from("span_id"), rmpv::Value::from(7u64)),
            (
                rmpv::Value::from("start"),
                rmpv::Value::from(1_700_000_000_000_000_000_i64),
            ),
            (
                rmpv::Value::from("duration"),
                rmpv::Value::from(5_000_000_i64),
            ),
        ]);
        let payload = rmpv::Value::Array(vec![rmpv::Value::Array(vec![span])]);
        let mut body = Vec::new();
        rmpv::encode::write_value(&mut body, &payload).unwrap();

        let r = dd_traces_v04(
            State(state),
            axum::http::HeaderMap::new(),
            Bytes::from(body),
        )
        .await
        .into_response();
        assert_eq!(r.status(), StatusCode::OK);
        let ack = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let ack: serde_json::Value = serde_json::from_slice(&ack).unwrap();
        assert!(ack.get("rate_by_service").is_some());

        let inserted = store.inserted.lock().unwrap();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].service, "billing");
        assert_eq!(inserted[0].operation, "http.request");
        assert_eq!(inserted[0].trace_id, format!("{:016x}{:016x}", 0, 42));
    }

    #[tokio::test]
    async fn datadog_info_advertises_supported_trace_endpoints() {
        let r = dd_info().await.into_response();
        assert_eq!(r.status(), StatusCode::OK);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let endpoints = info["endpoints"].as_array().unwrap();
        assert!(endpoints.contains(&serde_json::json!("/v0.4/traces")));
        assert!(endpoints.contains(&serde_json::json!("/v0.5/traces")));
        assert_eq!(info["client_drop_p0s"], serde_json::json!(false));
    }

    #[test]
    fn eval_snapshot_derives_runs_cases_and_scores_from_existing_signals() {
        let dir = tempfile::tempdir().unwrap();
        let store = TaelBackend::with_wal_key(
            dir.path().to_str().unwrap(),
            &format!("tael-test-eval-{}", uuid::Uuid::new_v4()),
        )
        .unwrap();
        let now = Utc::now();
        let mut attrs = HashMap::new();
        attrs.insert("tael.eval.suite_id".to_string(), "suite-a".to_string());
        attrs.insert("tael.eval.run_id".to_string(), "run-a".to_string());
        attrs.insert("tael.eval.case_id".to_string(), "case-1".to_string());
        attrs.insert("tael.eval.case_count".to_string(), "2".to_string());

        store
            .insert_spans(&[Span {
                trace_id: "trace-a".to_string(),
                span_id: "span-a".to_string(),
                parent_span_id: None,
                service: "agent".to_string(),
                operation: "eval case".to_string(),
                start_time: now,
                end_time: now + chrono::Duration::milliseconds(25),
                duration_ms: 25.0,
                status: SpanStatus::Ok,
                attributes: attrs,
                events: Vec::new(),
                kind: Default::default(),
                llm: None,
            }])
            .unwrap();

        let mut score_attrs = HashMap::new();
        score_attrs.insert("suite_id".to_string(), "suite-a".to_string());
        score_attrs.insert("run_id".to_string(), "run-a".to_string());
        score_attrs.insert("case_id".to_string(), "case-1".to_string());
        score_attrs.insert("trace_id".to_string(), "trace-a".to_string());
        score_attrs.insert("metric".to_string(), "correctness".to_string());
        score_attrs.insert("label".to_string(), "pass".to_string());
        store
            .insert_metrics(&[MetricPoint {
                timestamp: now + chrono::Duration::milliseconds(30),
                service: "tael-eval".to_string(),
                name: EVAL_SCORE_METRIC.to_string(),
                metric_type: MetricType::Gauge,
                value: 1.0,
                unit: "score".to_string(),
                attributes: score_attrs,
                histogram: None,
            }])
            .unwrap();

        let snapshot = build_eval_snapshot(&store).unwrap();
        let run = snapshot.runs.get("run-a").unwrap();
        assert_eq!(run.suite_id.as_deref(), Some("suite-a"));
        assert_eq!(run.case_count, Some(2));
        assert_eq!(run.observed_cases, 1);
        assert_eq!(run.scored_cases, 1);
        assert_eq!(run.passed_cases, 1);
        assert_eq!(run.pending_cases, Some(1));
        assert_eq!(run.avg_scores.get("correctness"), Some(&1.0));

        let case = snapshot
            .cases
            .get(&("run-a".to_string(), "case-1".to_string()))
            .unwrap();
        assert_eq!(case.status, "pass");
        assert_eq!(case.trace_id.as_deref(), Some("trace-a"));
    }
}

// ── Alerts ──────────────────────────────────────────────────────────

/// Rules and their current state. This is the closest thing tael has to a
/// dashboard, and it is JSON on purpose: the intended consumer polls or
/// follows it, rather than looking at it.
async fn list_alerts(State(state): State<AppState>) -> impl IntoResponse {
    let states = state.alerts.states();
    let rules: Vec<serde_json::Value> = state
        .alerts
        .list()
        .into_iter()
        .map(|rule| {
            let current = states
                .get(&rule.name)
                .copied()
                .unwrap_or(crate::alerts::AlertState::Ok);
            serde_json::json!({
                "name": rule.name,
                "query": rule.query,
                "for_seconds": rule.for_seconds,
                "window_seconds": rule.window_seconds,
                "sinks": rule.sinks,
                "description": rule.description,
                "created_at": rule.created_at,
                "state": current,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "alerts": rules, "count": rules.len() })),
    )
}

#[derive(Debug, Deserialize)]
struct CreateAlertPayload {
    name: String,
    query: String,
    #[serde(default)]
    for_seconds: i64,
    #[serde(default)]
    window_seconds: Option<i64>,
    #[serde(default)]
    sinks: Vec<crate::alerts::Sink>,
    #[serde(default)]
    description: Option<String>,
}

async fn create_alert(
    State(state): State<AppState>,
    Json(payload): Json<CreateAlertPayload>,
) -> impl IntoResponse {
    let rule = crate::alerts::AlertRule {
        name: payload.name,
        query: payload.query,
        for_seconds: payload.for_seconds,
        window_seconds: payload.window_seconds.unwrap_or(300),
        sinks: payload.sinks,
        description: payload.description,
        created_at: Utc::now(),
    };
    match state.alerts.create(rule.clone()) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "created": rule.name, "query": rule.query })),
        ),
        // A rejected rule is the caller's mistake (bad query, duplicate name),
        // not a server fault.
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn delete_alert(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match state.alerts.delete(&name) {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({ "deleted": name }))),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("no alert named `{name}`") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct AlertEventParams {
    limit: Option<usize>,
}

async fn alert_events(
    State(state): State<AppState>,
    Query(params): Query<AlertEventParams>,
) -> impl IntoResponse {
    let events = state.alerts.recent_events(params.limit.unwrap_or(50));
    (
        StatusCode::OK,
        Json(serde_json::json!({ "events": events, "count": events.len() })),
    )
}

/// Live alert feed. This is the long-poll primitive a babysitting agent blocks
/// on: connect once and be woken when something changes, instead of polling.
async fn live_alerts(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.alerts.subscribe();
    let stream = BroadcastStream::new(rx)
        .filter_map(|result| result.ok().map(|json| Ok(Event::default().data(json))));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ── Online scoring ──────────────────────────────────────────────────

/// Score rules with their progress. `traces_seen` versus `traces_sampled`
/// makes the effective sample rate visible, and `failures`/`last_error` mean a
/// silently broken judge is diagnosable rather than just absent from the data.
async fn list_score_rules(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.scores.status();
    let rules: Vec<serde_json::Value> = state
        .scores
        .list()
        .into_iter()
        .map(|rule| {
            let progress = status.get(&rule.name).cloned().unwrap_or_default();
            serde_json::json!({
                "name": rule.name,
                "sample": rule.sample,
                "matcher": rule.matcher,
                "command": rule.command,
                "description": rule.description,
                "created_at": rule.created_at,
                "status": progress,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "rules": rules, "count": rules.len() })),
    )
}

#[derive(Debug, Deserialize)]
struct CreateScoreRulePayload {
    name: String,
    #[serde(default)]
    sample: Option<f64>,
    #[serde(default)]
    matcher: crate::scoring::TraceMatcher,
    command: String,
    #[serde(default)]
    description: Option<String>,
}

async fn create_score_rule(
    State(state): State<AppState>,
    Json(payload): Json<CreateScoreRulePayload>,
) -> impl IntoResponse {
    let rule = crate::scoring::ScoreRule {
        name: payload.name,
        // A rule with no explicit rate scores a small slice rather than
        // everything: the expensive default is the wrong one for a judge that
        // may call a model per trace.
        sample: payload.sample.unwrap_or(0.05),
        matcher: payload.matcher,
        command: payload.command,
        description: payload.description,
        created_at: Utc::now(),
    };
    match state.scores.create(rule.clone()) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({ "created": rule.name, "sample": rule.sample })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn delete_score_rule(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    match state.scores.delete(&name) {
        Ok(true) => (StatusCode::OK, Json(serde_json::json!({ "deleted": name }))),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("no score rule named `{name}`") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

// ── Topology, diff, and metric inspection (DESIGN.md M3) ────────────

#[derive(Debug, Deserialize)]
struct TopologyParams {
    last: Option<String>,
    limit: Option<u32>,
}

/// Service dependency graph derived from span parent/child edges.
///
/// An edge exists when a span in service A is the parent of a span in service
/// B. This is reconstructed from the spans themselves rather than declared
/// anywhere, so it reflects what the system actually did rather than what an
/// architecture diagram claims.
async fn query_topology(
    State(state): State<AppState>,
    Query(params): Query<TopologyParams>,
) -> impl IntoResponse {
    let query = TraceQuery {
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: Some(params.limit.unwrap_or(50_000)),
        ..Default::default()
    };

    let spans = match state.store.query_traces(&query) {
        Ok(spans) => spans,
        Err(e) => {
            tracing::error!(error = %e, "topology query failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    // span_id -> (service, is_error) so a child can find its parent's service.
    let by_id: HashMap<&str, &Span> = spans.iter().map(|s| (s.span_id.as_str(), s)).collect();

    #[derive(Default)]
    struct Edge {
        calls: i64,
        errors: i64,
        total_ms: f64,
    }
    let mut edges: BTreeMap<(String, String), Edge> = BTreeMap::new();
    let mut nodes: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    // Spans whose parent is outside the queried window, which would otherwise
    // look like roots and overstate entry points.
    let mut dangling = 0usize;

    for span in &spans {
        let node = nodes.entry(span.service.clone()).or_insert((0, 0));
        node.0 += 1;
        if span.status == SpanStatus::Error {
            node.1 += 1;
        }

        let Some(parent_id) = span.parent_span_id.as_deref() else {
            continue;
        };
        let Some(parent) = by_id.get(parent_id) else {
            dangling += 1;
            continue;
        };
        // Only cross-service edges are dependencies; a span calling another
        // span inside the same service is internal structure.
        if parent.service == span.service {
            continue;
        }
        let edge = edges
            .entry((parent.service.clone(), span.service.clone()))
            .or_default();
        edge.calls += 1;
        edge.total_ms += span.duration_ms;
        if span.status == SpanStatus::Error {
            edge.errors += 1;
        }
    }

    let node_list: Vec<serde_json::Value> = nodes
        .iter()
        .map(|(name, (span_count, error_count))| {
            serde_json::json!({
                "service": name,
                "span_count": span_count,
                "error_count": error_count,
                "error_rate": if *span_count > 0 {
                    *error_count as f64 / *span_count as f64
                } else {
                    0.0
                },
            })
        })
        .collect();

    let edge_list: Vec<serde_json::Value> = edges
        .iter()
        .map(|((from, to), e)| {
            serde_json::json!({
                "from": from,
                "to": to,
                "calls": e.calls,
                "errors": e.errors,
                "error_rate": if e.calls > 0 { e.errors as f64 / e.calls as f64 } else { 0.0 },
                "avg_duration_ms": if e.calls > 0 { e.total_ms / e.calls as f64 } else { 0.0 },
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "services": node_list,
            "edges": edge_list,
            "spans_examined": spans.len(),
            "spans_with_parent_outside_window": dangling,
        })),
    )
}

#[derive(Debug, Deserialize)]
struct DiffParams {
    last: Option<String>,
    baseline: Option<String>,
    service: Option<String>,
}

/// Compare two windows across every summary metric.
///
/// `anomalies` is the opinionated version of this — it applies fixed thresholds
/// and reports only what it judges regressed. `diff` reports every delta and
/// leaves the judgement to the caller, which is what an agent investigating a
/// specific change actually wants.
async fn query_diff(
    State(state): State<AppState>,
    Query(params): Query<DiffParams>,
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

    let service = params.service.as_deref();
    let current = match state.store.query_summary(current_seconds, service) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };
    let baseline = match state.store.query_summary(baseline_seconds, service) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    // The baseline window contains the current one, so raw counts are not
    // comparable — rates are. Counts are still reported, labeled as totals.
    let per_second = |count: i64, seconds: i64| {
        if seconds > 0 {
            count as f64 / seconds as f64
        } else {
            0.0
        }
    };
    let delta = |current: f64, baseline: f64| {
        serde_json::json!({
            "current": current,
            "baseline": baseline,
            "delta": current - baseline,
            "ratio": if baseline.abs() > f64::EPSILON { current / baseline } else { f64::NAN },
        })
    };

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "current_window_seconds": current_seconds,
            "baseline_window_seconds": baseline_seconds,
            "service_filter": service,
            "note": "the baseline window contains the current one, so rates \
                     compare directly but totals do not",
            "spans_per_second": delta(
                per_second(current.traces.span_count, current_seconds),
                per_second(baseline.traces.span_count, baseline_seconds),
            ),
            "error_rate": delta(current.traces.error_rate, baseline.traces.error_rate),
            "p50_ms": delta(current.traces.p50_ms, baseline.traces.p50_ms),
            "p95_ms": delta(current.traces.p95_ms, baseline.traces.p95_ms),
            "p99_ms": delta(current.traces.p99_ms, baseline.traces.p99_ms),
            "avg_ms": delta(current.traces.avg_ms, baseline.traces.avg_ms),
            "log_errors_per_second": delta(
                per_second(current.logs.error, current_seconds),
                per_second(baseline.logs.error, baseline_seconds),
            ),
            "totals": {
                "current_span_count": current.traces.span_count,
                "baseline_span_count": baseline.traces.span_count,
                "current_error_count": current.traces.error_count,
                "baseline_error_count": baseline.traces.error_count,
            },
        })),
    )
}

#[derive(Debug, Deserialize)]
struct MetricDetailParams {
    last: Option<String>,
    limit: Option<u32>,
}

/// Everything known about one metric: its type, unit, label keys, series
/// count, value range, and recent points. Answers "what is this metric and can
/// I query it" without guessing at a filter that returns nothing.
async fn get_metric(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<MetricDetailParams>,
) -> impl IntoResponse {
    let query = MetricQuery {
        service: None,
        name: Some(name.clone()),
        metric_type: None,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: Some(params.limit.unwrap_or(500)),
        tenant: None,
    };

    let points = match state.store.query_metrics(&query) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    if points.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no points found for metric `{name}`"),
                "metric": name,
            })),
        );
    }

    let mut label_keys: HashSet<&str> = HashSet::new();
    let mut services: HashSet<&str> = HashSet::new();
    let mut series: HashSet<String> = HashSet::new();
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut with_histogram = 0usize;

    for p in &points {
        services.insert(p.service.as_str());
        for k in p.attributes.keys() {
            label_keys.insert(k.as_str());
        }
        let mut key: Vec<_> = p.attributes.iter().collect();
        key.sort();
        series.insert(format!("{}|{:?}", p.service, key));
        min = min.min(p.value);
        max = max.max(p.value);
        if p.histogram.is_some() {
            with_histogram += 1;
        }
    }

    let mut sorted_labels: Vec<&str> = label_keys.into_iter().collect();
    sorted_labels.sort_unstable();
    let mut sorted_services: Vec<&str> = services.into_iter().collect();
    sorted_services.sort_unstable();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "metric": name,
            "type": points[0].metric_type.to_string(),
            "unit": points[0].unit,
            "point_count": points.len(),
            "series_count": series.len(),
            "services": sorted_services,
            "label_keys": sorted_labels,
            "value_min": min,
            "value_max": max,
            "latest_timestamp": points.first().map(|p| p.timestamp),
            // Quantiles are only answerable for points that retained buckets.
            "points_with_histogram_buckets": with_histogram,
            "histogram_quantile_available": with_histogram > 0,
            "recent_points": points.iter().take(20).collect::<Vec<_>>(),
        })),
    )
}

// ── Eval case suites ────────────────────────────────────────────────

async fn list_suites(State(state): State<AppState>) -> impl IntoResponse {
    let suites: Vec<serde_json::Value> = state
        .suites
        .list()
        .into_iter()
        .map(|(name, suite)| {
            serde_json::json!({
                "name": name,
                "case_count": suite.cases.len(),
                "snapshot_count": suite.snapshots.len(),
                "updated_at": suite.updated_at,
                "latest_snapshot": suite.snapshots.last().map(|s| s.id.clone()),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "suites": suites, "count": suites.len() })),
    )
}

#[derive(Debug, Deserialize)]
struct SuiteReadParams {
    /// Optional snapshot id; absent reads the working set.
    snapshot: Option<String>,
}

async fn get_suite(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(params): Query<SuiteReadParams>,
) -> impl IntoResponse {
    let reference = match &params.snapshot {
        Some(id) => format!("{name}@{id}"),
        None => name.clone(),
    };
    match state.suites.resolve(&reference) {
        Ok((suite, snapshot, cases)) => {
            let snapshots = state
                .suites
                .get(&suite)
                .map(|s| s.snapshots)
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "suite": suite,
                    "snapshot": snapshot,
                    "cases": cases,
                    "count": cases.len(),
                    "snapshots": snapshots,
                })),
            )
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct PushSuiteBody {
    cases: Vec<crate::suites::CaseRef>,
}

async fn push_suite(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PushSuiteBody>,
) -> impl IntoResponse {
    match state.suites.push(&name, body.cases) {
        Ok(count) => (
            StatusCode::OK,
            Json(serde_json::json!({ "suite": name, "case_count": count })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize, Default)]
struct SnapshotBody {
    #[serde(default)]
    note: Option<String>,
}

async fn snapshot_suite(
    State(state): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<SnapshotBody>>,
) -> impl IntoResponse {
    let note = body.and_then(|Json(b)| b.note);
    match state.suites.snapshot(&name, note) {
        Ok(snapshot) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "suite": name,
                "snapshot": snapshot.id,
                "case_count": snapshot.case_count(),
                "created_at": snapshot.created_at,
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct SuiteDiffParams {
    from: String,
    to: String,
}

async fn diff_suites(
    State(state): State<AppState>,
    Query(params): Query<SuiteDiffParams>,
) -> impl IntoResponse {
    match state.suites.diff(&params.from, &params.to) {
        Ok(diff) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "from": params.from,
                "to": params.to,
                "added": diff.added,
                "removed": diff.removed,
                "changed": diff.changed,
                "unchanged": diff.unchanged,
            })),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct RollupParams {
    name: Option<String>,
    service: Option<String>,
    last: Option<String>,
    limit: Option<usize>,
}

/// 5-minute downsampled metric aggregates.
///
/// Rollups are kept on a much longer clock than raw points, so this is the
/// only surface that can answer a year-scale trend question. Each bucket
/// carries min/max/avg/sum/count rather than a single value, because a
/// downsample that kept only the mean would hide exactly the spikes a trend
/// question is usually about.
async fn query_rollups(
    State(state): State<AppState>,
    Query(params): Query<RollupParams>,
) -> impl IntoResponse {
    let last_seconds = params.last.as_deref().and_then(parse_duration_to_seconds);
    match state.store.query_metric_rollups(
        params.name.as_deref(),
        params.service.as_deref(),
        last_seconds,
        params.limit.unwrap_or(1000),
    ) {
        Ok(rollups) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "rollups": rollups,
                "count": rollups.len(),
                "bucket_seconds": 300,
            })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

// ── Similarity and clustering ───────────────────────────────────────

/// The text an embedding represents for one trace.
///
/// Built from the operations, error types, and LLM payload hashes rather than
/// raw attributes: a trace's identity for "have I seen this before" is what it
/// tried to do and how it failed, not the request ids and timestamps that make
/// every trace superficially unique.
fn trace_signature(spans: &[Span]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for span in spans {
        parts.push(format!("{} {}", span.service, span.operation));
        if span.status == SpanStatus::Error {
            parts.push(format!("error {}", span.operation));
        }
        for key in ["error.type", "exception.type", "rpc.grpc.status_code"] {
            if let Some(v) = span.attributes.get(key) {
                parts.push(format!("{key}={v}"));
            }
        }
        if let Some(llm) = &span.llm {
            parts.push(format!("llm {} {}", llm.provider, llm.model));
            if let Some(reason) = &llm.finish_reason {
                parts.push(format!("finish={reason}"));
            }
        }
    }
    parts.sort();
    parts.dedup();
    parts.join(" ")
}

#[derive(Debug, Deserialize)]
struct EmbedBody {
    /// Command that reads text on stdin and prints a JSON array of numbers.
    embed_cmd: String,
    #[serde(default)]
    last: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Build embeddings for recent traces.
///
/// Explicit rather than automatic on ingest: embedding costs money per trace
/// and most deployments will never want it, so it happens when asked.
async fn build_embeddings(
    State(state): State<AppState>,
    Json(body): Json<EmbedBody>,
) -> impl IntoResponse {
    let query = TraceQuery {
        last_seconds: body.last.as_deref().and_then(parse_duration_to_seconds),
        limit: Some(body.limit.unwrap_or(1000)),
        ..Default::default()
    };
    let spans = match state.store.query_traces(&query) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    let mut by_trace: BTreeMap<String, Vec<Span>> = BTreeMap::new();
    for span in spans {
        by_trace
            .entry(span.trace_id.clone())
            .or_default()
            .push(span);
    }

    let mut store = match crate::similarity::EmbeddingStore::load(&state.data_dir) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };

    let (mut embedded, mut skipped, mut failed) = (0usize, 0usize, 0usize);
    let mut last_error: Option<String> = None;
    for (trace_id, spans) in &by_trace {
        // Already embedded traces are skipped: a trace is immutable once
        // written, so re-embedding it would only spend money to get the same
        // vector back.
        if store.embeddings.contains_key(trace_id) {
            skipped += 1;
            continue;
        }
        let signature = trace_signature(spans);
        if signature.trim().is_empty() {
            skipped += 1;
            continue;
        }
        match crate::similarity::embed(&body.embed_cmd, &signature).await {
            Ok(vector) => {
                store.embeddings.insert(trace_id.clone(), vector);
                embedded += 1;
            }
            Err(e) => {
                failed += 1;
                last_error = Some(e.to_string());
            }
        }
    }

    if let Err(e) = store.save(&state.data_dir) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "embedded": embedded,
            "skipped": skipped,
            "failed": failed,
            "total_embeddings": store.embeddings.len(),
            "last_error": last_error,
        })),
    )
}

#[derive(Debug, Deserialize)]
struct SimilarParams {
    limit: Option<usize>,
    min_similarity: Option<f32>,
}

async fn similar_traces(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
    Query(params): Query<SimilarParams>,
) -> impl IntoResponse {
    let store = match crate::similarity::EmbeddingStore::load(&state.data_dir) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };
    let Some(query) = store.get(&trace_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("trace `{trace_id}` has no embedding"),
                "hint": "run `tael embed --cmd <embedder>` first",
            })),
        );
    };

    let neighbors = crate::similarity::nearest(
        &query,
        &store.to_vec(),
        params.limit.unwrap_or(10),
        params.min_similarity.unwrap_or(0.0),
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "trace_id": trace_id,
            "neighbors": neighbors,
            "count": neighbors.len(),
            "corpus_size": store.embeddings.len(),
        })),
    )
}

#[derive(Debug, Deserialize)]
struct ClusterParams {
    k: Option<usize>,
}

async fn cluster_traces(
    State(state): State<AppState>,
    Query(params): Query<ClusterParams>,
) -> impl IntoResponse {
    let store = match crate::similarity::EmbeddingStore::load(&state.data_dir) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            );
        }
    };
    let embeddings = store.to_vec();
    match crate::similarity::cluster(&embeddings, params.k.unwrap_or(5), 50) {
        Ok(clusters) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "clusters": clusters,
                "count": clusters.len(),
                "corpus_size": embeddings.len(),
                "note": "cohesion below ~0.7 means the grouping is weak; read the exemplars before acting on it",
            })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
