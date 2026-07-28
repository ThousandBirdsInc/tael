//! The ingest tier: OTLP-terminating forwarders for `TAEL_NODE_ROLE=ingest`
//! (`docs/tael-server-scaling-ha.md` §4 "Ingest tier", the impl plan's
//! "dedicated ingest-only process mode").
//!
//! An ingest-only node accepts OTLP on both transports (gRPC `:4317`,
//! HTTP/protobuf `:4318` + the REST listener), splits every export batch by
//! the **same shard key the query fan-out uses** — `hash(trace_id)` for spans
//! and logs, `hash(metric name)` for metrics (`storage::fanout::shard_index`)
//! — and re-posts one OTLP/HTTP request per shard. It opens no local engine:
//! decode → route → forward, nothing else.
//!
//! Splitting happens at the protobuf layer, *before* any payload extraction,
//! so LLM prompt/completion blobs and text indexing happen on the storage
//! shard that owns the trace — an ingest node holds no blobs and no index.
//!
//! Auth composes: the client's `Authorization` header is forwarded verbatim
//! (gRPC metadata or HTTP header), so the shard authenticates the *original*
//! principal and tenant stamping stays correct. Run all nodes against the
//! same keystore. `TAEL_INGEST_FORWARD_API_KEY` supplies a fallback key for
//! clients that authenticated some other way.
//!
//! Delivery is at-least-once: if any shard's slice fails, the whole batch
//! errors with a retryable status and the producer resends. Shards that
//! already accepted their slice apply the resend idempotently (hot-tier keys
//! are content-derived), so retries cannot duplicate.

use std::sync::Arc;

use anyhow::Result;
use opentelemetry_proto::tonic::collector::{
    logs::v1::{
        ExportLogsServiceRequest, ExportLogsServiceResponse, logs_service_server::LogsService,
    },
    metrics::v1::{
        ExportMetricsServiceRequest, ExportMetricsServiceResponse,
        metrics_service_server::MetricsService,
    },
    trace::v1::{
        ExportTraceServiceRequest, ExportTraceServiceResponse, trace_service_server::TraceService,
    },
};
use prost::Message;
use tonic::{Request, Response, Status};

use crate::storage::shard_index;

/// The client's raw `Authorization` header value, stashed on the request by
/// the OTLP/HTTP handlers so the forwarder can pass the original credentials
/// through. (The gRPC path reads it straight from request metadata.)
#[derive(Clone)]
pub struct ForwardAuth(pub String);

/// Splits OTLP export requests by shard and re-posts them over OTLP/HTTP.
pub struct OtlpForwarder {
    /// Shard base URLs (their REST listeners — the OTLP routes are always
    /// mounted there too, so no extra port needs to be reachable).
    shards: Vec<String>,
    client: reqwest::Client,
    /// Fallback `Authorization` value when the client sent none
    /// (`TAEL_INGEST_FORWARD_API_KEY`).
    default_auth: Option<String>,
}

impl OtlpForwarder {
    pub fn new(shards: Vec<String>) -> Result<Self> {
        anyhow::ensure!(
            !shards.is_empty(),
            "ingest forwarding requires at least one shard URL"
        );
        let shards = shards
            .into_iter()
            .map(|s| s.trim_end_matches('/').to_string())
            .collect();
        let default_auth = std::env::var("TAEL_INGEST_FORWARD_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .map(|k| format!("Bearer {}", k.trim()));
        Ok(Self {
            shards,
            client: reqwest::Client::new(),
            default_auth,
        })
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// POST one shard's slice; errors map onto OTLP statuses the exporter
    /// treats correctly (retryable vs. not).
    async fn post(
        &self,
        shard: usize,
        path: &str,
        body: Vec<u8>,
        auth: Option<&str>,
    ) -> Result<(), Status> {
        let url = format!("{}{}", self.shards[shard], path);
        let mut req = self
            .client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .body(body);
        if let Some(auth) = auth.or(self.default_auth.as_deref()) {
            req = req.header(reqwest::header::AUTHORIZATION, auth);
        }
        let resp = req.send().await.map_err(|e| {
            Status::unavailable(format!("shard {url} unreachable: {e}; retry with backoff"))
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(match status.as_u16() {
            401 => Status::unauthenticated(format!("shard {url}: {body}")),
            403 => Status::permission_denied(format!("shard {url}: {body}")),
            429 | 503 => {
                Status::resource_exhausted(format!("shard {url} at capacity: {body}; retry"))
            }
            _ => Status::internal(format!("shard {url} returned {status}: {body}")),
        })
    }

    async fn forward<M: Message>(
        &self,
        path: &str,
        slices: Vec<Option<M>>,
        auth: Option<&str>,
    ) -> Result<(), Status> {
        for (shard, slice) in slices.into_iter().enumerate() {
            if let Some(slice) = slice {
                self.post(shard, path, slice.encode_to_vec(), auth).await?;
            }
        }
        Ok(())
    }
}

/// The client's `Authorization` to forward: the stashed HTTP header, the gRPC
/// metadata value, or nothing (then the forwarder's fallback key applies).
fn forward_auth<T>(request: &Request<T>) -> Option<String> {
    if let Some(ForwardAuth(auth)) = request.extensions().get::<ForwardAuth>() {
        return Some(auth.clone());
    }
    request
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

// ── The three forwarding services (drop-in for the storing ones) ────

pub struct ForwardingTraceService(pub Arc<OtlpForwarder>);

#[tonic::async_trait]
impl TraceService for ForwardingTraceService {
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
        let auth = forward_auth(&request);
        let req = request.into_inner();
        let (slices, count) = split_traces(req, self.0.shard_count());
        if let Err(e) = self.0.forward("/v1/traces", slices, auth.as_deref()).await {
            super::stats::record_error(super::stats::Pipeline::OtlpSpans);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpSpans, count);
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

pub struct ForwardingLogsService(pub Arc<OtlpForwarder>);

#[tonic::async_trait]
impl LogsService for ForwardingLogsService {
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
        let auth = forward_auth(&request);
        let req = request.into_inner();
        let (slices, count) = split_logs(req, self.0.shard_count());
        if let Err(e) = self.0.forward("/v1/logs", slices, auth.as_deref()).await {
            super::stats::record_error(super::stats::Pipeline::OtlpLogs);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpLogs, count);
        Ok(Response::new(ExportLogsServiceResponse {
            partial_success: None,
        }))
    }
}

pub struct ForwardingMetricsService(pub Arc<OtlpForwarder>);

#[tonic::async_trait]
impl MetricsService for ForwardingMetricsService {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        let Some(_permit) = super::backpressure::try_acquire() else {
            super::stats::record_shed(super::stats::Pipeline::OtlpMetrics);
            return Err(Status::resource_exhausted(
                "ingest at capacity; retry with backoff",
            ));
        };
        let auth = forward_auth(&request);
        let req = request.into_inner();
        let (slices, count) = split_metrics(req, self.0.shard_count());
        if let Err(e) = self.0.forward("/v1/metrics", slices, auth.as_deref()).await {
            super::stats::record_error(super::stats::Pipeline::OtlpMetrics);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpMetrics, count);
        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: None,
        }))
    }
}

// ── Protobuf-level batch splitting ──────────────────────────────────

/// Split a trace export by `hash(trace_id)`, preserving each span's resource
/// and scope wrappers. Returns one request per shard (`None` = nothing for
/// that shard) plus the total span count.
pub(crate) fn split_traces(
    req: ExportTraceServiceRequest,
    n: usize,
) -> (Vec<Option<ExportTraceServiceRequest>>, usize) {
    let mut out: Vec<Option<ExportTraceServiceRequest>> = (0..n).map(|_| None).collect();
    let mut count = 0usize;
    for resource_spans in req.resource_spans {
        for scope_spans in &resource_spans.scope_spans {
            // Partition this scope's spans by their trace's owning shard.
            let mut per_shard: Vec<Vec<_>> = (0..n).map(|_| Vec::new()).collect();
            for span in &scope_spans.spans {
                count += 1;
                let shard = shard_index(&hex::encode(&span.trace_id), n);
                per_shard[shard].push(span.clone());
            }
            for (shard, spans) in per_shard.into_iter().enumerate() {
                if spans.is_empty() {
                    continue;
                }
                let target = out[shard].get_or_insert_with(Default::default);
                target
                    .resource_spans
                    .push(opentelemetry_proto::tonic::trace::v1::ResourceSpans {
                        resource: resource_spans.resource.clone(),
                        schema_url: resource_spans.schema_url.clone(),
                        scope_spans: vec![opentelemetry_proto::tonic::trace::v1::ScopeSpans {
                            scope: scope_spans.scope.clone(),
                            schema_url: scope_spans.schema_url.clone(),
                            spans,
                        }],
                    });
            }
        }
    }
    (out, count)
}

/// Split a logs export the way the read fan-out expects: by `hash(trace_id)`
/// when the record carries one, else by the resource's service name (matching
/// `FanoutStore::insert_logs`, which routes orphan logs by service).
pub(crate) fn split_logs(
    req: ExportLogsServiceRequest,
    n: usize,
) -> (Vec<Option<ExportLogsServiceRequest>>, usize) {
    let mut out: Vec<Option<ExportLogsServiceRequest>> = (0..n).map(|_| None).collect();
    let mut count = 0usize;
    for resource_logs in req.resource_logs {
        let service = resource_service_name(resource_logs.resource.as_ref());
        for scope_logs in &resource_logs.scope_logs {
            let mut per_shard: Vec<Vec<_>> = (0..n).map(|_| Vec::new()).collect();
            for record in &scope_logs.log_records {
                count += 1;
                let key = if record.trace_id.is_empty() {
                    service.clone()
                } else {
                    hex::encode(&record.trace_id)
                };
                per_shard[shard_index(&key, n)].push(record.clone());
            }
            for (shard, log_records) in per_shard.into_iter().enumerate() {
                if log_records.is_empty() {
                    continue;
                }
                let target = out[shard].get_or_insert_with(Default::default);
                target
                    .resource_logs
                    .push(opentelemetry_proto::tonic::logs::v1::ResourceLogs {
                        resource: resource_logs.resource.clone(),
                        schema_url: resource_logs.schema_url.clone(),
                        scope_logs: vec![opentelemetry_proto::tonic::logs::v1::ScopeLogs {
                            scope: scope_logs.scope.clone(),
                            schema_url: scope_logs.schema_url.clone(),
                            log_records,
                        }],
                    });
            }
        }
    }
    (out, count)
}

/// Split a metrics export by `hash(metric name)`, matching
/// `FanoutStore::insert_metrics` (a series stays whole on one shard, so
/// unique-name counts still merge by sum).
pub(crate) fn split_metrics(
    req: ExportMetricsServiceRequest,
    n: usize,
) -> (Vec<Option<ExportMetricsServiceRequest>>, usize) {
    let mut out: Vec<Option<ExportMetricsServiceRequest>> = (0..n).map(|_| None).collect();
    let mut count = 0usize;
    for resource_metrics in req.resource_metrics {
        for scope_metrics in &resource_metrics.scope_metrics {
            let mut per_shard: Vec<Vec<_>> = (0..n).map(|_| Vec::new()).collect();
            for metric in &scope_metrics.metrics {
                count += 1;
                per_shard[shard_index(&metric.name, n)].push(metric.clone());
            }
            for (shard, metrics) in per_shard.into_iter().enumerate() {
                if metrics.is_empty() {
                    continue;
                }
                let target = out[shard].get_or_insert_with(Default::default);
                target.resource_metrics.push(
                    opentelemetry_proto::tonic::metrics::v1::ResourceMetrics {
                        resource: resource_metrics.resource.clone(),
                        schema_url: resource_metrics.schema_url.clone(),
                        scope_metrics: vec![
                            opentelemetry_proto::tonic::metrics::v1::ScopeMetrics {
                                scope: scope_metrics.scope.clone(),
                                schema_url: scope_metrics.schema_url.clone(),
                                metrics,
                            },
                        ],
                    },
                );
            }
        }
    }
    (out, count)
}

/// `service.name` from a proto resource, with the same `unknown` default the
/// storing ingest path normalizes to — the shard keys must agree.
fn resource_service_name(
    resource: Option<&opentelemetry_proto::tonic::resource::v1::Resource>,
) -> String {
    resource
        .and_then(|r| {
            r.attributes.iter().find_map(|attr| {
                if attr.key != "service.name" {
                    return None;
                }
                attr.value.as_ref().and_then(|v| {
                    v.value.as_ref().and_then(|val| match val {
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            s,
                        ) => Some(s.clone()),
                        _ => None,
                    })
                })
            })
        })
        .unwrap_or_else(|| "unknown".to_string())
}

// ── The ingest-only node's Store stub ───────────────────────────────

use crate::storage::models::{
    AnomalyReport, CorrelateReport, LogQuery, LogRecord, MetricPoint, MetricQuery, ServiceInfo,
    Span, SummaryReport, TraceComment, TraceQuery,
};
use crate::storage::{RemoteStore, Store};

const INGEST_ONLY: &str = "this node is ingest-only (TAEL_NODE_ROLE=ingest): it accepts and \
     forwards OTLP but serves no queries and stores nothing. Query a storage shard or a \
     query-tier node (TAEL_QUERY_SHARDS) instead; point dd-trace and Prometheus remote-write \
     directly at a storage node.";

/// The `Store` an ingest-only node mounts behind its REST router: every
/// storage operation is refused with an explanation, and readiness reflects
/// shard reachability so a load balancer drops an ingest node whose shards
/// are all gone (it could only shed traffic anyway). With no shards
/// configured (the Kafka-producer edge, whose downstream is a broker rather
/// than HTTP shards), readiness is unconditional.
pub struct IngestOnlyStore {
    shards: Vec<RemoteStore>,
    message: &'static str,
}

impl IngestOnlyStore {
    pub fn new(shard_urls: &[String]) -> Result<Self> {
        Self::with_message(shard_urls, INGEST_ONLY)
    }

    /// Same refusal behavior with a mode-specific explanation.
    pub fn with_message(shard_urls: &[String], message: &'static str) -> Result<Self> {
        let shards = shard_urls
            .iter()
            .map(RemoteStore::new)
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { shards, message })
    }
}

macro_rules! refuse {
    ($self:ident) => {
        anyhow::bail!($self.message)
    };
}

impl Store for IngestOnlyStore {
    fn insert_spans(&self, _spans: &[Span]) -> Result<()> {
        refuse!(self)
    }
    fn query_traces(&self, _query: &TraceQuery) -> Result<Vec<Span>> {
        refuse!(self)
    }
    fn get_trace(&self, _trace_id: &str) -> Result<Vec<Span>> {
        refuse!(self)
    }
    fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        refuse!(self)
    }
    fn add_comment(
        &self,
        _trace_id: &str,
        _span_id: Option<&str>,
        _author: &str,
        _body: &str,
    ) -> Result<TraceComment> {
        refuse!(self)
    }
    fn get_comments(&self, _trace_id: &str) -> Result<Vec<TraceComment>> {
        refuse!(self)
    }
    fn insert_logs(&self, _logs: &[LogRecord]) -> Result<()> {
        refuse!(self)
    }
    fn query_logs(&self, _query: &LogQuery) -> Result<Vec<LogRecord>> {
        refuse!(self)
    }
    fn insert_metrics(&self, _metrics: &[MetricPoint]) -> Result<()> {
        refuse!(self)
    }
    fn query_metrics(&self, _query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        refuse!(self)
    }
    fn query_summary(&self, _last_seconds: i64, _service: Option<&str>) -> Result<SummaryReport> {
        refuse!(self)
    }
    fn query_anomalies(
        &self,
        _current_seconds: i64,
        _baseline_seconds: i64,
        _service: Option<&str>,
    ) -> Result<AnomalyReport> {
        refuse!(self)
    }
    fn query_correlate(&self, _trace_id: &str) -> Result<Option<CorrelateReport>> {
        refuse!(self)
    }
    fn query_sql(&self, _sql: &str) -> Result<Vec<serde_json::Value>> {
        refuse!(self)
    }
    fn health(&self) -> Result<()> {
        // Ready while at least one shard is reachable — with none configured
        // (the Kafka edge), always ready; with all configured shards down,
        // ingest can only shed, so the LB should route around this node.
        if self.shards.is_empty() {
            return Ok(());
        }
        let mut healthy = 0usize;
        for (i, shard) in self.shards.iter().enumerate() {
            match shard.health() {
                Ok(()) => healthy += 1,
                Err(e) => tracing::warn!(shard = i, error = %e, "ingest shard unhealthy"),
            }
        }
        anyhow::ensure!(
            healthy > 0,
            "no reachable ingest shards ({} configured)",
            self.shards.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
    use opentelemetry_proto::tonic::resource::v1::Resource;

    fn kv(k: &str, v: &str) -> KeyValue {
        KeyValue {
            key: k.into(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(v.into())),
            }),
        }
    }

    fn trace_request(traces: &[&[u8; 16]]) -> ExportTraceServiceRequest {
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", "api")],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: traces
                        .iter()
                        .enumerate()
                        .map(|(i, tid)| Span {
                            trace_id: tid.to_vec(),
                            span_id: vec![i as u8 + 1; 8],
                            name: format!("op-{i}"),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn split_traces_keeps_whole_traces_on_one_shard_and_loses_none() {
        // Many distinct traces, two spans each (adjacent duplicates), split
        // across 3 shards: every trace's spans stay together, the shard
        // choice matches the query fan-out's hash, and the union is complete.
        let mut ids: Vec<[u8; 16]> = Vec::new();
        for i in 0..32u8 {
            ids.push([i; 16]);
        }
        let doubled: Vec<&[u8; 16]> = ids.iter().flat_map(|t| [t, t]).collect();
        let (slices, count) = split_traces(trace_request(&doubled), 3);
        assert_eq!(count, 64);

        let mut total = 0usize;
        for (shard, slice) in slices.iter().enumerate() {
            let Some(slice) = slice else { continue };
            for rs in &slice.resource_spans {
                // Wrappers survive the split.
                assert!(rs.resource.is_some(), "resource wrapper must be preserved");
                for ss in &rs.scope_spans {
                    for span in &ss.spans {
                        total += 1;
                        assert_eq!(
                            shard_index(&hex::encode(&span.trace_id), 3),
                            shard,
                            "span landed on a shard the query fan-out would not look at first"
                        );
                    }
                }
            }
        }
        assert_eq!(
            total, 64,
            "no span may be dropped or duplicated by the split"
        );
    }

    #[test]
    fn split_logs_routes_by_trace_then_service() {
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        let req = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", "api")],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![
                        LogRecord {
                            trace_id: vec![9u8; 16],
                            ..Default::default()
                        },
                        LogRecord::default(), // orphan: routes by service
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let (slices, count) = split_logs(req, 4);
        assert_eq!(count, 2);
        let with_trace = shard_index(&hex::encode([9u8; 16]), 4);
        let orphan = shard_index("api", 4);
        for (shard, slice) in slices.iter().enumerate() {
            let records: usize = slice
                .iter()
                .flat_map(|s| &s.resource_logs)
                .flat_map(|r| &r.scope_logs)
                .map(|s| s.log_records.len())
                .sum();
            let expected = usize::from(shard == with_trace) + usize::from(shard == orphan);
            assert_eq!(records, expected, "shard {shard}");
        }
    }

    #[test]
    fn split_metrics_keeps_a_series_on_one_shard() {
        use opentelemetry_proto::tonic::metrics::v1::{Metric, ResourceMetrics, ScopeMetrics};
        let metric = |name: &str| Metric {
            name: name.into(),
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![metric("cpu"), metric("mem"), metric("cpu")],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let (slices, count) = split_metrics(req, 5);
        assert_eq!(count, 3);
        let cpu_shard = shard_index("cpu", 5);
        let cpu_count: usize = slices[cpu_shard]
            .iter()
            .flat_map(|s| &s.resource_metrics)
            .flat_map(|r| &r.scope_metrics)
            .flat_map(|s| &s.metrics)
            .filter(|m| m.name == "cpu")
            .count();
        assert_eq!(cpu_count, 2, "both cpu points must land on cpu's shard");
    }

    /// End to end: a forwarder in front of two real storage shards (REST
    /// servers with the OTLP routes mounted, exactly as `tael serve` mounts
    /// them). Spans shard by trace, each trace stays whole, and the query
    /// fan-out over the same shards sees everything.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forwarder_shards_otlp_traffic_across_real_shards() {
        use crate::storage::{FanoutStore, RemoteStore, TaelBackend};
        use std::net::SocketAddr;

        struct WalKeyGuard(String);
        impl Drop for WalKeyGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.0));
            }
        }

        /// A storage shard: real REST router + OTLP routes over a real engine.
        fn serve_shard(key: String) -> (SocketAddr, Arc<TaelBackend>) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_str().unwrap().to_string();
            let backend = Arc::new(TaelBackend::with_wal_key(&path, &key).unwrap());
            let store: Arc<dyn Store> = Arc::clone(&backend) as Arc<dyn Store>;
            let (tx, rx) = std::sync::mpsc::channel();
            let served = Arc::clone(&backend);
            std::thread::spawn(move || {
                let _dir = dir;
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    let blobs = Arc::new(crate::storage::BlobStore::new(&path).unwrap());
                    let bus = Arc::new(crate::span_bus::SpanBus::new().unwrap());
                    let log_bus = Arc::new(crate::log_bus::LogBus::new().unwrap());
                    let otlp = crate::ingest::otlp_http::OtlpHttpState {
                        traces: Arc::new(crate::ingest::otlp::OtlpTraceService::new(
                            Arc::clone(&store),
                            Arc::clone(&blobs),
                            Arc::new(crate::storage::PayloadIndexes::Single(
                                served.search_index(),
                            )),
                            Arc::clone(&bus),
                            false,
                        )),
                        logs: Arc::new(crate::ingest::otlp_logs::OtlpLogsService::new(
                            Arc::clone(&store),
                            Arc::clone(&blobs),
                            Arc::new(crate::storage::PayloadIndexes::Single(
                                served.search_index(),
                            )),
                            Arc::clone(&log_bus),
                            false,
                        )),
                        metrics: Arc::new(crate::ingest::otlp_metrics::OtlpMetricsService::new(
                            Arc::clone(&store),
                            false,
                        )),
                    };
                    let alerts = Arc::new(crate::alerts::AlertStore::open(&path).unwrap());
                    let scores = Arc::new(crate::scoring::ScoreRuleStore::open(&path).unwrap());
                    let suites = Arc::new(crate::suites::SuiteStore::open(&path).unwrap());
                    let app = crate::api::rest::router(
                        store, blobs, bus, log_bus, None, alerts, scores, suites, path, false,
                    )
                    .merge(crate::ingest::otlp_http::router(otlp));
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    tx.send(listener.local_addr().unwrap()).unwrap();
                    axum::serve(listener, app).await.unwrap();
                });
            });
            (rx.recv().unwrap(), backend)
        }

        let key0 = format!("tael-test-fwd0-{}", uuid::Uuid::new_v4());
        let key1 = format!("tael-test-fwd1-{}", uuid::Uuid::new_v4());
        let _g0 = WalKeyGuard(key0.clone());
        let _g1 = WalKeyGuard(key1.clone());
        let (addr0, shard0) = serve_shard(key0);
        let (addr1, shard1) = serve_shard(key1);

        let forwarder = Arc::new(
            OtlpForwarder::new(vec![format!("http://{addr0}"), format!("http://{addr1}")]).unwrap(),
        );
        let service = ForwardingTraceService(Arc::clone(&forwarder));

        // 16 traces, 2 spans each — enough that both shards get traffic.
        let ids: Vec<[u8; 16]> = (0..16u8).map(|i| [i + 1; 16]).collect();
        let doubled: Vec<&[u8; 16]> = ids.iter().flat_map(|t| [t, t]).collect();
        service
            .export(Request::new(trace_request(&doubled)))
            .await
            .expect("forwarding should succeed");

        // Every trace's spans landed whole on exactly one shard...
        let mut on0 = 0usize;
        let mut on1 = 0usize;
        for tid in &ids {
            let hexid = hex::encode(tid);
            let s0 = shard0.get_trace(&hexid).unwrap();
            let s1 = shard1.get_trace(&hexid).unwrap();
            match (s0.len(), s1.len()) {
                (2, 0) => on0 += 1,
                (0, 2) => on1 += 1,
                other => panic!("trace {hexid} split or lost: {other:?}"),
            }
        }
        assert!(
            on0 > 0 && on1 > 0,
            "the split must actually shard: {on0}/{on1}"
        );

        // ...and the query fan-out over the same shard set sees all of them.
        // (RemoteStore is blocking HTTP, so it runs off the async test thread.)
        let all = tokio::task::spawn_blocking(move || {
            let fanout = FanoutStore::new(vec![
                Arc::new(RemoteStore::new(format!("http://{addr0}")).unwrap()) as Arc<dyn Store>,
                Arc::new(RemoteStore::new(format!("http://{addr1}")).unwrap()) as Arc<dyn Store>,
            ])
            .unwrap();
            fanout
                .query_traces(&TraceQuery {
                    limit: Some(100),
                    ..Default::default()
                })
                .unwrap()
        })
        .await
        .unwrap();
        assert_eq!(all.len(), 32);
    }
}
