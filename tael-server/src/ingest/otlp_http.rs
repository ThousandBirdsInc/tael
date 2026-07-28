//! OTLP/HTTP receiver (default `:4318`).
//!
//! Many OTel SDKs ship with `http/protobuf` as their default exporter
//! protocol, so a gRPC-only server forces per-language configuration on every
//! service being onboarded. This module mounts `/v1/traces`, `/v1/logs`, and
//! `/v1/metrics` on a plain HTTP listener and forwards each decoded request
//! into the *same* [`TraceService`]/[`LogsService`]/[`MetricsService`]
//! implementations the gRPC listener uses — there is no second ingest path to
//! keep in sync, only a second transport.
//!
//! Wire format follows the OTLP/HTTP spec: request and response bodies are
//! binary protobuf (`application/x-protobuf`), optionally gzip-encoded, and a
//! success returns the matching `Export*ServiceResponse` (an empty message,
//! meaning "all records accepted"). OTLP/JSON is not implemented; those
//! requests get a 415 that names the supported content type rather than a
//! confusing decode failure.

use std::io::Read;
use std::sync::Arc;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::post,
};

use crate::auth::Principal;
use opentelemetry_proto::tonic::collector::{
    logs::v1::{ExportLogsServiceRequest, logs_service_server::LogsService},
    metrics::v1::{ExportMetricsServiceRequest, metrics_service_server::MetricsService},
    trace::v1::{ExportTraceServiceRequest, trace_service_server::TraceService},
};
use prost::Message;

/// The three OTLP services, shared with the gRPC listener. Trait objects, so
/// the same transports serve either the storing services (`otlp*`) or the
/// ingest-tier forwarding services (`ingest::forward`).
#[derive(Clone)]
pub struct OtlpHttpState {
    pub traces: Arc<dyn TraceService>,
    pub logs: Arc<dyn LogsService>,
    pub metrics: Arc<dyn MetricsService>,
}

/// The OTLP/HTTP route set. Mounted on its own listener (`:4318` by default)
/// and reachable through the REST listener too, so a single port is enough
/// when that is all a deployment can expose.
pub fn routes() -> Router<OtlpHttpState> {
    Router::new()
        .route("/v1/traces", post(export_traces))
        .route("/v1/logs", post(export_logs))
        .route("/v1/metrics", post(export_metrics))
}

/// The OTLP/HTTP routes with state applied. Used both for the dedicated `:4318`
/// listener and merged into the REST listener, so a deployment that can only
/// expose one port still accepts OTLP/HTTP.
pub fn router(state: OtlpHttpState) -> Router {
    routes().with_state(state)
}

/// Cap on a decompressed OTLP body (64 MiB). A gzip bomb would otherwise let an
/// unauthenticated sender allocate without bound; real OTLP batches are orders
/// of magnitude smaller than this.
const MAX_DECOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;

/// Decode a request body into an OTLP protobuf message, transparently
/// gunzipping when the sender set `Content-Encoding: gzip`.
fn decode<T: Message + Default>(headers: &HeaderMap, body: Bytes) -> Result<T, ErrorResponse> {
    if let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        && ct.starts_with("application/json")
    {
        return Err(ErrorResponse::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "OTLP/JSON is not supported; export with application/x-protobuf \
             (OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf)",
        ));
    }

    let raw = match headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
    {
        Some(enc) if enc.eq_ignore_ascii_case("gzip") => {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(&body[..])
                .take(MAX_DECOMPRESSED_BYTES)
                .read_to_end(&mut out)
                .map_err(|e| {
                    ErrorResponse::new(StatusCode::BAD_REQUEST, format!("gzip decode failed: {e}"))
                })?;
            Bytes::from(out)
        }
        // `identity` and absent both mean "as-is". Anything else is a codec we
        // never advertised, so say so instead of failing to parse protobuf.
        Some(enc) if !enc.is_empty() && !enc.eq_ignore_ascii_case("identity") => {
            return Err(ErrorResponse::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("unsupported Content-Encoding `{enc}`; use gzip or identity"),
            ));
        }
        _ => body,
    };

    T::decode(&raw[..]).map_err(|e| {
        ErrorResponse::new(
            StatusCode::BAD_REQUEST,
            format!("protobuf decode failed: {e}"),
        )
    })
}

/// Encode a successful export response as protobuf, per the OTLP/HTTP spec.
fn ok_response<T: Message>(message: T) -> axum::response::Response {
    let mut buf = Vec::with_capacity(message.encoded_len());
    // Encoding into a Vec only fails when the buffer can't grow, which would
    // already have aborted on allocation.
    let _ = message.encode(&mut buf);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        buf,
    )
        .into_response()
}

/// A failed export. OTLP/HTTP wants a `Status` protobuf here; agents and
/// humans debugging an exporter are far better served by a readable JSON
/// message, and every SDK surfaces the response body on failure either way.
struct ErrorResponse {
    status: StatusCode,
    message: String,
}

impl ErrorResponse {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            axum::Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

/// Map a service-level failure (storage down, WAL full) onto an OTLP retryable
/// status so the exporter backs off and resends rather than dropping the batch.
fn export_failed(err: tonic::Status) -> ErrorResponse {
    let status = match err.code() {
        tonic::Code::InvalidArgument => StatusCode::BAD_REQUEST,
        tonic::Code::Unauthenticated => StatusCode::UNAUTHORIZED,
        tonic::Code::PermissionDenied => StatusCode::FORBIDDEN,
        tonic::Code::ResourceExhausted | tonic::Code::Unavailable => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ErrorResponse::new(status, err.message().to_string())
}

async fn export_traces(
    State(state): State<OtlpHttpState>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let req: ExportTraceServiceRequest = match decode(&headers, body) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    // Carry the auth layer's principal into the shared service, which stamps
    // each record with the writer's tenant — same as the gRPC interceptor.
    let mut req = tonic::Request::new(req);
    if let Some(Extension(p)) = principal {
        req.extensions_mut().insert(p);
    }
    match state.traces.export(req).await {
        Ok(resp) => ok_response(resp.into_inner()),
        Err(e) => export_failed(e).into_response(),
    }
}

async fn export_logs(
    State(state): State<OtlpHttpState>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let req: ExportLogsServiceRequest = match decode(&headers, body) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    // Carry the auth layer's principal into the shared service, which stamps
    // each record with the writer's tenant — same as the gRPC interceptor.
    let mut req = tonic::Request::new(req);
    if let Some(Extension(p)) = principal {
        req.extensions_mut().insert(p);
    }
    match state.logs.export(req).await {
        Ok(resp) => ok_response(resp.into_inner()),
        Err(e) => export_failed(e).into_response(),
    }
}

async fn export_metrics(
    State(state): State<OtlpHttpState>,
    principal: Option<Extension<Principal>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let req: ExportMetricsServiceRequest = match decode(&headers, body) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    // Carry the auth layer's principal into the shared service, which stamps
    // each record with the writer's tenant — same as the gRPC interceptor.
    let mut req = tonic::Request::new(req);
    if let Some(Extension(p)) = principal {
        req.extensions_mut().insert(p);
    }
    match state.metrics.export(req).await {
        Ok(resp) => ok_response(resp.into_inner()),
        Err(e) => export_failed(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use axum::body::Body;
    use axum::http::Request;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
    use tower::ServiceExt;

    use super::*;
    use crate::ingest::{
        otlp::OtlpTraceService, otlp_logs::OtlpLogsService, otlp_metrics::OtlpMetricsService,
    };
    use crate::log_bus::LogBus;
    use crate::span_bus::SpanBus;
    use crate::storage::testing::TestBackend;

    fn test_state(engine: &TestBackend) -> OtlpHttpState {
        let store = engine.store();
        let blobs = Arc::clone(&engine.blobs);
        OtlpHttpState {
            traces: Arc::new(OtlpTraceService::new(
                Arc::clone(&store),
                Arc::clone(&blobs),
                Arc::new(crate::storage::PayloadIndexes::Single(
                    engine.backend.search_index(),
                )),
                Arc::new(SpanBus::new().unwrap()),
                false,
            )),
            logs: Arc::new(OtlpLogsService::new(
                Arc::clone(&store),
                blobs,
                Arc::new(crate::storage::PayloadIndexes::Single(
                    engine.backend.search_index(),
                )),
                Arc::new(LogBus::new().unwrap()),
                false,
            )),
            metrics: Arc::new(OtlpMetricsService::new(store, false)),
        }
    }

    /// A minimal one-span export batch, matching what an SDK sends.
    fn trace_request(service: &str) -> ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".into(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(service.into())),
                        }),
                    }],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![1u8; 16],
                        span_id: vec![2u8; 8],
                        name: "GET /healthz".into(),
                        start_time_unix_nano: 1_700_000_000_000_000_000,
                        end_time_unix_nano: 1_700_000_000_010_000_000,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[tokio::test]
    async fn protobuf_traces_are_ingested_and_acked() {
        let engine = TestBackend::new();
        let app = router(test_state(&engine));

        let body = trace_request("checkout").encode_to_vec();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/traces")
                    .header("content-type", "application/x-protobuf")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        // An empty partial_success means every record was accepted.
        let decoded = ExportTraceServiceResponse::decode(&bytes[..]).unwrap();
        assert!(decoded.partial_success.is_none());
    }

    #[tokio::test]
    async fn gzip_encoded_bodies_are_decompressed() {
        let engine = TestBackend::new();
        let app = router(test_state(&engine));

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&trace_request("gzipped").encode_to_vec())
            .unwrap();
        let body = encoder.finish().unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/traces")
                    .header("content-type", "application/x-protobuf")
                    .header("content-encoding", "gzip")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn json_requests_report_the_supported_content_type() {
        let engine = TestBackend::new();
        let app = router(test_state(&engine));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/traces")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("x-protobuf"),
            "error should name the supported content type: {body}"
        );
    }

    #[tokio::test]
    async fn malformed_protobuf_is_a_client_error() {
        let engine = TestBackend::new();
        let app = router(test_state(&engine));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/metrics")
                    .header("content-type", "application/x-protobuf")
                    .body(Body::from(vec![0xff, 0xff, 0xff, 0xff]))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
