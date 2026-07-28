use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse, trace_service_server::TraceService,
};
use tonic::{Request, Response, Status};

use crate::span_bus::SpanBus;
use crate::storage::models::{LlmOperation, LlmSpan, Span, SpanEvent, SpanKind, SpanStatus};
use crate::storage::{BlobStore, PayloadIndexes, Store};

pub struct OtlpTraceService {
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    /// Full-text index routing over LLM payloads (`None` for backends without
    /// a text index).
    search: Arc<PayloadIndexes>,
    bus: Arc<SpanBus>,
    /// Stamp every record with the writing principal's tenant
    /// (`TAEL_MULTI_TENANT`). See [`crate::tenancy::stamp`].
    multi_tenant: bool,
}

impl OtlpTraceService {
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<BlobStore>,
        search: Arc<PayloadIndexes>,
        bus: Arc<SpanBus>,
        multi_tenant: bool,
    ) -> Self {
        Self {
            store,
            blobs,
            search,
            bus,
            multi_tenant,
        }
    }

    /// Store a payload value in the blob store, returning its content hash.
    /// A blob failure is logged and treated as "no payload" rather than failing
    /// ingestion.
    fn blob_value(&self, value: Option<&str>) -> Option<String> {
        let value = value?;
        if value.is_empty() {
            return None;
        }
        match self.blobs.put(value.as_bytes()) {
            Ok(hash) => Some(hash),
            Err(e) => {
                tracing::warn!(error = %e, "failed to store payload blob");
                None
            }
        }
    }
}

/// Newtype that lets the gRPC listener serve a trace service shared (via `Arc`)
/// with the OTLP/HTTP listener. `tonic`'s generated server takes ownership of
/// its service, and the orphan rule blocks implementing [`TraceService`] on
/// `Arc<OtlpTraceService>` directly, so the wrapper carries the shared handle.
pub struct SharedTraceService(pub Arc<dyn TraceService>);

#[tonic::async_trait]
impl TraceService for SharedTraceService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        self.0.export(request).await
    }
}

#[tonic::async_trait]
impl TraceService for OtlpTraceService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let Some(_permit) = super::backpressure::try_acquire() else {
            super::stats::record_shed(super::stats::Pipeline::OtlpSpans);
            return Err(Status::resource_exhausted(
                "ingest at capacity; retry with backoff",
            ));
        };
        // The authenticated principal (attached by the auth layer on both
        // transports) determines the tenant this batch is stamped with and,
        // under tenant isolation, which engine's text index receives it.
        let principal = request
            .extensions()
            .get::<crate::auth::Principal>()
            .cloned();
        let tenant = crate::tenancy::write_tenant(self.multi_tenant, principal.as_ref());
        let search = self.search.for_tenant(&tenant);
        let req = request.into_inner();
        let mut spans = Vec::new();
        let mut indexed_any = false;

        for resource_spans in &req.resource_spans {
            let service_name = resource_spans
                .resource
                .as_ref()
                .and_then(|r| {
                    r.attributes.iter().find_map(|attr| {
                        if attr.key == "service.name" {
                            attr.value.as_ref().and_then(|v| {
                                v.value.as_ref().map(|val| match val {
                                    opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
                                    _ => String::new(),
                                })
                            })
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| "unknown".to_string());

            for scope_spans in &resource_spans.scope_spans {
                for otel_span in &scope_spans.spans {
                    let trace_id = hex::encode(&otel_span.trace_id);
                    let span_id = hex::encode(&otel_span.span_id);
                    let parent_span_id = if otel_span.parent_span_id.is_empty() {
                        None
                    } else {
                        Some(hex::encode(&otel_span.parent_span_id))
                    };

                    let start_time = timestamp_to_datetime(otel_span.start_time_unix_nano);
                    let end_time = timestamp_to_datetime(otel_span.end_time_unix_nano);
                    let duration_ms = (otel_span.end_time_unix_nano as f64
                        - otel_span.start_time_unix_nano as f64)
                        / 1_000_000.0;

                    let status = match otel_span.status.as_ref() {
                        Some(s) => match s.code() {
                            opentelemetry_proto::tonic::trace::v1::status::StatusCode::Ok => {
                                SpanStatus::Ok
                            }
                            opentelemetry_proto::tonic::trace::v1::status::StatusCode::Error => {
                                SpanStatus::Error
                            }
                            _ => SpanStatus::Unset,
                        },
                        None => SpanStatus::Unset,
                    };

                    let mut attributes = HashMap::new();
                    for attr in &otel_span.attributes {
                        if let Some(ref value) = attr.value
                            && let Some(ref val) = value.value
                        {
                            let s = match val {
                                    opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
                                    opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => i.to_string(),
                                    opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(d) => d.to_string(),
                                    opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => b.to_string(),
                                    _ => continue,
                                };
                            attributes.insert(attr.key.clone(), s);
                        }
                    }

                    let events: Vec<SpanEvent> = otel_span
                        .events
                        .iter()
                        .map(|e| {
                            let mut event_attrs = HashMap::new();
                            for attr in &e.attributes {
                                if let Some(ref value) = attr.value
                                    && let Some(ref val) = value.value {
                                        let s = match val {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => i.to_string(),
                                            _ => continue,
                                        };
                                        event_attrs.insert(attr.key.clone(), s);
                                    }
                            }
                            SpanEvent {
                                name: e.name.clone(),
                                timestamp: timestamp_to_datetime(e.time_unix_nano),
                                attributes: event_attrs,
                            }
                        })
                        .collect();

                    // Promote GenAI attributes into a typed LLM extension. When
                    // present, the span kind is marked `Llm`; otherwise map the
                    // OTel span kind faithfully.
                    let mut llm = extract_llm_span(&attributes);
                    if let Some(ref mut l) = llm {
                        // Capture payload text, then move it out of the columnar
                        // attributes into the content-addressed blob store
                        // (keeping only hashes; dedups shared system prompts).
                        let prompt = attributes.remove("gen_ai.prompt");
                        let completion = attributes.remove("gen_ai.completion");

                        // Index the payload text for full-text search before it
                        // leaves memory (only the hashes survive on the span).
                        if let Some(ref idx) = search {
                            let mut text = String::new();
                            if let Some(p) = &prompt {
                                text.push_str(p);
                                text.push(' ');
                            }
                            if let Some(c) = &completion {
                                text.push_str(c);
                            }
                            if !text.trim().is_empty() {
                                if let Err(e) = idx.index_span(&trace_id, &span_id, &text) {
                                    tracing::warn!(error = %e, "failed to index payload text");
                                } else {
                                    indexed_any = true;
                                }
                            }
                        }

                        l.prompt_sha256 = self.blob_value(prompt.as_deref());
                        l.completion_sha256 = self.blob_value(completion.as_deref());
                    }
                    let kind = if llm.is_some() {
                        SpanKind::Llm
                    } else {
                        map_span_kind(otel_span.kind())
                    };

                    // Attribute values are indexed too, because the structured
                    // filters are exact-match: an agent that doesn't already
                    // know a URL or model string can't filter for it, but can
                    // search for it.
                    if let Some(ref idx) = search
                        && let Err(e) = idx.index_span_attributes(&trace_id, &span_id, &attributes)
                    {
                        tracing::warn!(error = %e, "failed to index span attributes");
                    } else if search.is_some() && !attributes.is_empty() {
                        indexed_any = true;
                    }

                    spans.push(Span {
                        trace_id,
                        span_id,
                        parent_span_id,
                        service: service_name.clone(),
                        operation: otel_span.name.clone(),
                        start_time,
                        end_time,
                        duration_ms,
                        status,
                        attributes,
                        events,
                        kind,
                        llm,
                    });
                }
            }
        }

        // Stamp the writer's tenant last, so it overrides anything the client
        // sent — the attribute is an authorization boundary, not client data.
        crate::tenancy::stamp(
            self.multi_tenant,
            principal.as_ref(),
            spans.iter_mut().map(|s| &mut s.attributes),
        );

        let span_count = spans.len();
        if let Err(e) = self.store.insert_spans(&spans) {
            tracing::error!(error = %e, "failed to insert spans");
            super::stats::record_error(super::stats::Pipeline::OtlpSpans);
            return Err(Status::internal(format!("storage error: {e}")));
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpSpans, span_count);

        // Make any newly indexed payload text searchable.
        if indexed_any
            && let Some(ref idx) = search
            && let Err(e) = idx.commit()
        {
            tracing::warn!(error = %e, "failed to commit search index");
        }

        if let Err(e) = self.bus.publish(&spans) {
            tracing::warn!(error = %e, "failed to publish spans to bus");
        }

        tracing::debug!(span_count, "ingested spans");

        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

fn timestamp_to_datetime(nanos: u64) -> DateTime<Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let nsecs = (nanos % 1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or_default()
}

/// Map the OpenTelemetry proto span kind to our [`SpanKind`].
fn map_span_kind(kind: opentelemetry_proto::tonic::trace::v1::span::SpanKind) -> SpanKind {
    use opentelemetry_proto::tonic::trace::v1::span::SpanKind as K;
    match kind {
        K::Server => SpanKind::Server,
        K::Client => SpanKind::Client,
        K::Producer => SpanKind::Producer,
        K::Consumer => SpanKind::Consumer,
        // Unspecified and Internal both fall through to Internal.
        _ => SpanKind::Internal,
    }
}

/// Build a typed [`LlmSpan`] from OpenTelemetry GenAI semantic-convention
/// attributes (`gen_ai.*`). Returns `None` when the span is not an LLM call.
/// Promoted keys are left in the attribute map so existing attribute filters
/// keep working; only the well-known fields are additionally typed here.
pub(crate) fn extract_llm_span(attrs: &HashMap<String, String>) -> Option<LlmSpan> {
    let provider = attrs.get("gen_ai.system").cloned();
    let model = attrs
        .get("gen_ai.request.model")
        .or_else(|| attrs.get("gen_ai.response.model"))
        .cloned();

    // Require at least a provider or model to call this an LLM span.
    if provider.is_none() && model.is_none() {
        return None;
    }

    let input_tokens = attrs
        .get("gen_ai.usage.input_tokens")
        .or_else(|| attrs.get("gen_ai.usage.prompt_tokens"))
        .and_then(|s| s.parse::<u32>().ok());
    let output_tokens = attrs
        .get("gen_ai.usage.output_tokens")
        .or_else(|| attrs.get("gen_ai.usage.completion_tokens"))
        .and_then(|s| s.parse::<u32>().ok());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        _ => attrs
            .get("gen_ai.usage.total_tokens")
            .and_then(|s| s.parse::<u32>().ok()),
    };

    let cost_usd = attrs
        .get("gen_ai.usage.cost")
        .or_else(|| attrs.get("gen_ai.usage.cost_usd"))
        .and_then(|s| s.parse::<f64>().ok());

    let operation = attrs
        .get("gen_ai.operation.name")
        .map(|s| LlmOperation::from_str(s))
        .unwrap_or_default();

    let temperature = attrs
        .get("gen_ai.request.temperature")
        .and_then(|s| s.parse::<f64>().ok());

    let finish_reason = attrs
        .get("gen_ai.response.finish_reasons")
        .or_else(|| attrs.get("gen_ai.response.finish_reason"))
        .cloned();

    let ttft_ms = attrs
        .get("gen_ai.response.time_to_first_token_ms")
        .and_then(|s| s.parse::<f64>().ok());

    Some(LlmSpan {
        provider: provider.unwrap_or_default(),
        model: model.unwrap_or_default(),
        operation,
        input_tokens,
        output_tokens,
        total_tokens,
        cost_usd,
        ttft_ms,
        inter_token_ms: None,
        // Payload hashes are populated when the blob store lands (Phase 2).
        prompt_sha256: None,
        completion_sha256: None,
        finish_reason,
        temperature,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn non_llm_span_has_no_extension() {
        let a = attrs(&[("http.method", "GET")]);
        assert!(extract_llm_span(&a).is_none());
    }

    #[test]
    fn maps_genai_attributes_to_typed_fields() {
        let a = attrs(&[
            ("gen_ai.system", "anthropic"),
            ("gen_ai.request.model", "claude-opus-4-7"),
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.usage.input_tokens", "1200"),
            ("gen_ai.usage.output_tokens", "340"),
            ("gen_ai.usage.cost", "0.0185"),
            ("gen_ai.request.temperature", "0.7"),
            ("gen_ai.response.finish_reasons", "end_turn"),
        ]);
        let llm = extract_llm_span(&a).expect("should detect LLM span");
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.model, "claude-opus-4-7");
        assert_eq!(llm.operation, LlmOperation::Chat);
        assert_eq!(llm.input_tokens, Some(1200));
        assert_eq!(llm.output_tokens, Some(340));
        assert_eq!(llm.total_tokens, Some(1540));
        assert_eq!(llm.cost_usd, Some(0.0185));
        assert_eq!(llm.temperature, Some(0.7));
        assert_eq!(llm.finish_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn detects_llm_span_from_model_alone() {
        let a = attrs(&[("gen_ai.response.model", "gpt-4o")]);
        let llm = extract_llm_span(&a).expect("model alone is enough");
        assert_eq!(llm.model, "gpt-4o");
        assert!(llm.provider.is_empty());
    }

    /// A one-span OTLP export whose span carries `extra` client attributes.
    fn export_request(
        extra: &[(&str, &str)],
    ) -> opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span as PSpan};

        let kv = |k: &str, v: &str| KeyValue {
            key: k.into(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(v.into())),
            }),
        };
        opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", "api")],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![PSpan {
                        trace_id: vec![7u8; 16],
                        span_id: vec![8u8; 8],
                        name: "op".into(),
                        start_time_unix_nano: 1_700_000_000_000_000_000,
                        end_time_unix_nano: 1_700_000_000_001_000_000,
                        attributes: extra.iter().map(|(k, v)| kv(k, v)).collect(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[tokio::test]
    async fn the_server_stamp_overrides_a_forged_client_tenant() {
        use crate::auth::{Principal, Role};
        use crate::storage::models::TraceQuery;
        use crate::storage::testing::TestBackend;
        use crate::tenancy::TENANT_ATTRIBUTE;

        let engine = TestBackend::new();
        let service = OtlpTraceService::new(
            engine.store(),
            Arc::clone(&engine.blobs),
            Arc::new(crate::storage::PayloadIndexes::None),
            Arc::new(crate::span_bus::SpanBus::new().unwrap()),
            true, // multi-tenant: stamping on
        );

        // The client claims to be team-b; the authenticated key is team-a's.
        let mut request = Request::new(export_request(&[(TENANT_ATTRIBUTE, "team-b")]));
        request.extensions_mut().insert(Principal {
            key_id: "k1".into(),
            name: "svc-key".into(),
            role: Role::Writer,
            tenant: "team-a".into(),
        });
        service.export(request).await.unwrap();

        let spans = engine.backend.query_traces(&TraceQuery::default()).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0]
                .attributes
                .get(TENANT_ATTRIBUTE)
                .map(String::as_str),
            Some("team-a"),
            "the writer's tenant must override the forged attribute"
        );

        // With tenancy off, nothing is stamped and client attributes pass
        // through untouched — the single-tenant path stays exactly as it was.
        let service_off = OtlpTraceService::new(
            engine.store(),
            Arc::clone(&engine.blobs),
            Arc::new(crate::storage::PayloadIndexes::None),
            Arc::new(crate::span_bus::SpanBus::new().unwrap()),
            false,
        );
        let mut request = Request::new(export_request(&[]));
        request.extensions_mut().insert(Principal {
            key_id: "k1".into(),
            name: "svc-key".into(),
            role: Role::Writer,
            tenant: "team-a".into(),
        });
        // Distinct span id so both survive.
        service_off.export(request).await.unwrap();
        let spans = engine.backend.query_traces(&TraceQuery::default()).unwrap();
        assert!(
            spans
                .iter()
                .any(|s| !s.attributes.contains_key(TENANT_ATTRIBUTE)),
            "tenancy off must not stamp"
        );
    }
}
