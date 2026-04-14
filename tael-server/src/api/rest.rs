use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::log_bus::LogBus;
use crate::span_bus::SpanBus;
use crate::storage::DuckDbStore;
use crate::storage::models::{LogQuery, MetricQuery, TraceQuery};

#[derive(Clone)]
struct AppState {
    store: Arc<DuckDbStore>,
    bus: Arc<SpanBus>,
    log_bus: Arc<LogBus>,
}

pub fn router(store: Arc<DuckDbStore>, bus: Arc<SpanBus>, log_bus: Arc<LogBus>) -> Router {
    let state = AppState { store, bus, log_bus };
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
        .route("/api/v1/write", post(prom_remote_write))
        .route("/healthz", get(healthz))
        .with_state(state)
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
) -> impl IntoResponse {
    let query = TraceQuery {
        service: params.service,
        operation: params.operation,
        min_duration_ms: params.min_duration_ms,
        max_duration_ms: params.max_duration_ms,
        status: params.status,
        last_seconds: params.last.as_deref().and_then(parse_duration_to_seconds),
        limit: params.limit,
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

    match crate::promql::evaluate(&state.store, &expr, lookback) {
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

async fn prom_remote_write(
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    crate::ingest::prom_remote_write::handle_write(state.store, body).await
}

async fn healthz() -> &'static str {
    "ok"
}
