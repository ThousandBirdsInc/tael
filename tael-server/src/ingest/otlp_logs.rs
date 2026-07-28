use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse, logs_service_server::LogsService,
};
use tonic::{Request, Response, Status};

use crate::log_bus::LogBus;
use crate::storage::models::{LogRecord, LogSeverity};
use crate::storage::{BlobStore, PayloadIndexes, Store};

/// Log bodies larger than this are offloaded to the blob store (stack traces,
/// dumped payloads). Tuned against real corpora later (design Open Q #7).
const LOG_BODY_BLOB_THRESHOLD: usize = 8 * 1024;

pub struct OtlpLogsService {
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    /// Full-text index routing shared with span ingest, so one `--text`
    /// query reaches log bodies and LLM payloads alike.
    search: Arc<PayloadIndexes>,
    bus: Arc<LogBus>,
    /// Stamp every record with the writing principal's tenant
    /// (`TAEL_MULTI_TENANT`). See [`crate::tenancy::stamp`].
    multi_tenant: bool,
}

impl OtlpLogsService {
    pub fn new(
        store: Arc<dyn Store>,
        blobs: Arc<BlobStore>,
        search: Arc<PayloadIndexes>,
        bus: Arc<LogBus>,
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
}

/// Shared-handle wrapper so the gRPC and OTLP/HTTP listeners serve the same
/// logs service. See [`super::otlp::SharedTraceService`].
pub struct SharedLogsService(pub Arc<OtlpLogsService>);

#[tonic::async_trait]
impl LogsService for SharedLogsService {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        self.0.export(request).await
    }
}

#[tonic::async_trait]
impl LogsService for OtlpLogsService {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        let Some(_permit) = super::backpressure::try_acquire() else {
            super::stats::record_shed(super::stats::Pipeline::OtlpLogs);
            return Err(Status::resource_exhausted(
                "ingest at capacity; retry with backoff",
            ));
        };
        let principal = request
            .extensions()
            .get::<crate::auth::Principal>()
            .cloned();
        let tenant = crate::tenancy::write_tenant(self.multi_tenant, principal.as_ref());
        let search = self.search.for_tenant(&tenant);
        let req = request.into_inner();
        let mut logs = Vec::new();
        // Bodies moved to the blob store, kept here so the search index still
        // sees their text.
        let mut searchable: Vec<(Option<String>, String)> = Vec::new();

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

                    // Offload oversized bodies to the blob store, keeping only
                    // the hash inline. Dedups repeated stack traces for free.
                    // The full text is kept for the search index, which would
                    // otherwise never see the very bodies most worth searching.
                    let (body, body_sha256) = if body.len() > LOG_BODY_BLOB_THRESHOLD {
                        match self.blobs.put(body.as_bytes()) {
                            Ok(hash) => {
                                searchable.push((trace_id.clone(), body));
                                (String::new(), Some(hash))
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "failed to store log body blob");
                                (body, None)
                            }
                        }
                    } else {
                        (body, None)
                    };

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
                        body_sha256,
                    });
                }
            }
        }

        // Index bodies for full-text search. Only logs carrying a trace ID are
        // indexed: search resolves to traces, and a log with no trace has
        // nothing to resolve to.
        if let Some(ref idx) = search {
            let mut indexed_any = false;
            let inline = logs.iter().filter_map(|log| {
                let trace_id = log.trace_id.clone()?;
                (!log.body.trim().is_empty()).then(|| (trace_id, log.body.clone()))
            });
            for (trace_id, body) in inline.chain(
                searchable
                    .iter()
                    .filter_map(|(tid, body)| tid.clone().map(|t| (t, body.clone()))),
            ) {
                match idx.index_log_body(&trace_id, &body) {
                    Ok(()) => indexed_any = true,
                    Err(e) => tracing::warn!(error = %e, "failed to index log body"),
                }
            }
            if indexed_any && let Err(e) = idx.commit() {
                tracing::warn!(error = %e, "failed to commit log search index");
            }
        }

        // Stamp the writer's tenant last, so it overrides anything the client
        // sent — the attribute is an authorization boundary, not client data.
        crate::tenancy::stamp(
            self.multi_tenant,
            principal.as_ref(),
            logs.iter_mut().map(|l| &mut l.attributes),
        );

        let log_count = logs.len();
        if let Err(e) = self.store.insert_logs(&logs) {
            tracing::error!(error = %e, "failed to insert logs");
            super::stats::record_error(super::stats::Pipeline::OtlpLogs);
            return Err(Status::internal(format!("storage error: {e}")));
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpLogs, log_count);

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
