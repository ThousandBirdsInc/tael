use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use serde::Deserialize;

use crate::storage::DuckDbStore;
use crate::storage::models::TraceQuery;

type AppState = Arc<DuckDbStore>;

pub fn router(store: AppState) -> Router {
    Router::new()
        .route("/api/v1/traces", get(query_traces))
        .route("/api/v1/traces/{trace_id}", get(get_trace))
        .route("/api/v1/services", get(list_services))
        .route(
            "/api/v1/traces/{trace_id}/comments",
            get(get_comments).post(add_comment),
        )
        .route("/healthz", get(healthz))
        .with_state(store)
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
    State(store): State<AppState>,
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

    match store.query_traces(&query) {
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

async fn get_trace(
    State(store): State<AppState>,
    Path(trace_id): Path<String>,
) -> impl IntoResponse {
    match store.get_trace(&trace_id) {
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

async fn list_services(State(store): State<AppState>) -> impl IntoResponse {
    match store.list_services() {
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
    State(store): State<AppState>,
    Path(trace_id): Path<String>,
    Json(payload): Json<AddCommentBody>,
) -> impl IntoResponse {
    let author = payload.author.as_deref().unwrap_or("anonymous");
    match store.add_comment(&trace_id, payload.span_id.as_deref(), author, &payload.body) {
        Ok(comment) => (StatusCode::CREATED, axum::Json(serde_json::json!({ "comment": comment }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn get_comments(
    State(store): State<AppState>,
    Path(trace_id): Path<String>,
) -> impl IntoResponse {
    match store.get_comments(&trace_id) {
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
