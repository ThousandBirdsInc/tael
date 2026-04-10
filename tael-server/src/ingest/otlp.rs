use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::TraceService,
};
use tonic::{Request, Response, Status};

use crate::storage::DuckDbStore;
use crate::storage::models::{Span, SpanEvent, SpanStatus};

pub struct OtlpTraceService {
    store: Arc<DuckDbStore>,
}

impl OtlpTraceService {
    pub fn new(store: Arc<DuckDbStore>) -> Self {
        Self { store }
    }
}

#[tonic::async_trait]
impl TraceService for OtlpTraceService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let req = request.into_inner();
        let mut spans = Vec::new();

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
                        if let Some(ref value) = attr.value {
                            if let Some(ref val) = value.value {
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
                    }

                    let events: Vec<SpanEvent> = otel_span
                        .events
                        .iter()
                        .map(|e| {
                            let mut event_attrs = HashMap::new();
                            for attr in &e.attributes {
                                if let Some(ref value) = attr.value {
                                    if let Some(ref val) = value.value {
                                        let s = match val {
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
                                            opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => i.to_string(),
                                            _ => continue,
                                        };
                                        event_attrs.insert(attr.key.clone(), s);
                                    }
                                }
                            }
                            SpanEvent {
                                name: e.name.clone(),
                                timestamp: timestamp_to_datetime(e.time_unix_nano),
                                attributes: event_attrs,
                            }
                        })
                        .collect();

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
                    });
                }
            }
        }

        let span_count = spans.len();
        if let Err(e) = self.store.insert_spans(&spans) {
            tracing::error!(error = %e, "failed to insert spans");
            return Err(Status::internal(format!("storage error: {e}")));
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
