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
    routing::{get, post},
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
        .route("/api/v1/evals/runs", get(eval_runs))
        .route("/api/v1/evals/runs/{run_id}", get(eval_run))
        .route("/api/v1/evals/runs/{run_id}/cases", get(eval_cases))
        .route("/api/v1/evals/runs/{run_id}/scores", get(eval_scores))
        .route("/api/v1/evals/runs/{run_id}/compare", get(eval_compare))
        .route("/api/v1/evals/scores", post(eval_add_score))
        .route("/api/v1/evals/runner-spans", post(eval_add_runner_span))
        .route("/api/v1/blobs", post(put_blob))
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
        Ok(spans) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({ "spans": spans })),
        ),
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

fn filter_log_batch(json: &str, service: Option<&str>, severity: Option<&str>) -> Option<String> {
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
