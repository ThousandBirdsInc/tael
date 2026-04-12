use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
    logs_service_server::LogsService,
};
use tonic::{Request, Response, Status};

use crate::log_bus::LogBus;
use crate::storage::DuckDbStore;
use crate::storage::models::{LogRecord, LogSeverity};

pub struct OtlpLogsService {
    store: Arc<DuckDbStore>,
    bus: Arc<LogBus>,
}

impl OtlpLogsService {
    pub fn new(store: Arc<DuckDbStore>, bus: Arc<LogBus>) -> Self {
        Self { store, bus }
    }
}

#[tonic::async_trait]
impl LogsService for OtlpLogsService {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        let req = request.into_inner();
        let mut logs = Vec::new();

        for resource_logs in &req.resource_logs {
            let service_name = resource_logs
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

            for scope_logs in &resource_logs.scope_logs {
                for otel_log in &scope_logs.log_records {
                    let trace_id = if otel_log.trace_id.is_empty() {
                        None
                    } else {
                        Some(hex::encode(&otel_log.trace_id))
                    };

                    let span_id = if otel_log.span_id.is_empty() {
                        None
                    } else {
                        Some(hex::encode(&otel_log.span_id))
                    };

                    let timestamp = timestamp_to_datetime(otel_log.time_unix_nano);
                    let observed_timestamp =
                        timestamp_to_datetime(otel_log.observed_time_unix_nano);

                    let severity = LogSeverity::from_severity_number(otel_log.severity_number);
                    let severity_text = if otel_log.severity_text.is_empty() {
                        severity.to_string().to_uppercase()
                    } else {
                        otel_log.severity_text.clone()
                    };

                    let body = otel_log
                        .body
                        .as_ref()
                        .and_then(|v| v.value.as_ref())
                        .map(|val| match val {
                            opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.clone(),
                            opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(i) => i.to_string(),
                            opentelemetry_proto::tonic::common::v1::any_value::Value::DoubleValue(d) => d.to_string(),
                            opentelemetry_proto::tonic::common::v1::any_value::Value::BoolValue(b) => b.to_string(),
                            _ => String::new(),
                        })
                        .unwrap_or_default();

                    let mut attributes = HashMap::new();
                    for attr in &otel_log.attributes {
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

                    logs.push(LogRecord {
                        timestamp,
                        observed_timestamp,
                        trace_id,
                        span_id,
                        severity,
                        severity_text,
                        body,
                        service: service_name.clone(),
                        attributes,
                    });
                }
            }
        }

        let log_count = logs.len();
        if let Err(e) = self.store.insert_logs(&logs) {
            tracing::error!(error = %e, "failed to insert logs");
            return Err(Status::internal(format!("storage error: {e}")));
        }

        if let Err(e) = self.bus.publish(&logs) {
            tracing::warn!(error = %e, "failed to publish logs to bus");
        }

        tracing::debug!(log_count, "ingested logs");

        Ok(Response::new(ExportLogsServiceResponse {
            partial_success: None,
        }))
    }
}

fn timestamp_to_datetime(nanos: u64) -> DateTime<Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let nsecs = (nanos % 1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or_default()
}
