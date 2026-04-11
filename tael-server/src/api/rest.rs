use std::convert::Infallible;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::Deserialize;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::span_bus::SpanBus;
use crate::storage::DuckDbStore;
use crate::storage::models::TraceQuery;

#[derive(Clone)]
struct AppState {
    store: Arc<DuckDbStore>,
    bus: Arc<SpanBus>,
}

pub fn router(store: Arc<DuckDbStore>, bus: Arc<SpanBus>) -> Router {
    let state = AppState { store, bus };
    Router::new()
        .route("/api/v1/traces", get(query_traces))
        .route("/api/v1/traces/live", get(live_traces))
        .route("/api/v1/traces/{trace_id}", get(get_trace))
        .route("/api/v1/services", get(list_services))
        .route(
            "/api/v1/traces/{trace_id}/comments",
            get(get_comments).post(add_comment),
        )
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

async fn healthz() -> &'static str {
    "ok"
}
