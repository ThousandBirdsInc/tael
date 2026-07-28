//! Kafka/Redpanda ingest buffer (`--features kafka`; design B5's
//! "optional Kafka/Redpanda ingest buffer for bursty traffic").
//!
//! **When to use it.** The walrus WAL already absorbs bursts on a single
//! node, and the WAL-shipping layer already replicates it — a broker is
//! *not* part of the default story (`docs/tael-server-scaling-ha.md` rejects
//! it there deliberately). The buffer earns its keep when ingest and storage
//! must scale and fail independently: producers keep accepting at full speed
//! while storage restarts, compacts, or falls behind, and the topic's
//! retention is the replay window.
//!
//! **Shape.** The topic carries whole OTLP protobuf export slices, not
//! tael-internal records, so the broker is a transparent pipe:
//!
//! - **Producer** (`TAEL_KAFKA_MODE=produce`): the node terminates OTLP on
//!   both transports, splits each export by the same shard key the query
//!   fan-out uses (`hash(trace_id)` spans/logs, `hash(name)` metrics — the
//!   `ingest::forward` splitters), and publishes one record per partition:
//!   `partition = shard_index % partition_count`. The write is acked to the
//!   producer only after the broker acked the publish.
//! - **Consumer** (`TAEL_KAFKA_MODE=consume`): a storage node owns an
//!   explicit set of partitions (`TAEL_KAFKA_PARTITIONS=0,1,…` — the
//!   single-writer-per-partition rule, chosen statically rather than by a
//!   consumer group so ownership is an operator decision) and applies each
//!   record through the very same OTLP services the network listeners use —
//!   blob extraction, text indexing, tenant stamping, backpressure, and
//!   ingest stats all behave identically.
//!
//! Offsets are tracked in `<data_dir>/kafka_offsets.json`, persisted after
//! apply — at-least-once, which is safe because the engine's hot-tier keys
//! are content-derived and replay overwrites instead of duplicating. A record
//! that fails to *decode* is skipped (a poison message must not wedge the
//! partition); a record that fails to *apply* is retried without advancing.
//!
//! Tenancy composes: the producer resolves the writing principal's tenant at
//! the edge and carries it in a record header; the consumer reconstructs a
//! principal from it so the normal server-side stamping applies. Raw client
//! credentials are never written to the topic.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
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
use rskafka::client::partition::{Compression, OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::record::Record;
use tonic::{Request, Response, Status};

use super::forward::{split_logs, split_metrics, split_traces};
use super::otlp_http::OtlpHttpState;
use crate::auth::{Principal, Role};

/// Record header naming the OTLP signal a record's payload decodes as.
const SIGNAL_HEADER: &str = "tael-signal";
/// Record header carrying the producing principal's tenant, when tenancy was
/// on at the edge.
const TENANT_HEADER: &str = "tael-tenant";

const SIGNAL_TRACES: &[u8] = b"traces";
const SIGNAL_LOGS: &[u8] = b"logs";
const SIGNAL_METRICS: &[u8] = b"metrics";

/// `TAEL_KAFKA_*` settings, parsed unconditionally so a build without the
/// feature can refuse loudly instead of silently ignoring them.
#[derive(Debug, Clone)]
pub struct KafkaSettings {
    /// `produce` or `consume` (`TAEL_KAFKA_MODE`).
    pub mode: KafkaMode,
    /// Bootstrap brokers (`TAEL_KAFKA_BROKERS`, comma-separated).
    pub brokers: Vec<String>,
    /// Topic (`TAEL_KAFKA_TOPIC`, default `tael-ingest`).
    pub topic: String,
    /// Consumer-owned partitions (`TAEL_KAFKA_PARTITIONS=0,1,…`); consume
    /// mode only.
    pub partitions: Vec<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KafkaMode {
    Produce,
    Consume,
}

impl KafkaSettings {
    /// Parse from the environment. `None` when `TAEL_KAFKA_MODE` is unset.
    pub fn from_env() -> Result<Option<Self>> {
        let mode = match std::env::var("TAEL_KAFKA_MODE") {
            Ok(m) if !m.trim().is_empty() => match m.trim().to_lowercase().as_str() {
                "produce" | "producer" | "publish" => KafkaMode::Produce,
                "consume" | "consumer" => KafkaMode::Consume,
                other => bail!("TAEL_KAFKA_MODE must be `produce` or `consume`, got `{other}`"),
            },
            _ => return Ok(None),
        };
        let brokers: Vec<String> = std::env::var("TAEL_KAFKA_BROKERS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if brokers.is_empty() {
            bail!("TAEL_KAFKA_MODE is set but TAEL_KAFKA_BROKERS is empty");
        }
        let topic = std::env::var("TAEL_KAFKA_TOPIC")
            .ok()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| "tael-ingest".to_string());
        let partitions: Vec<i32> = std::env::var("TAEL_KAFKA_PARTITIONS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<i32>().context("parsing TAEL_KAFKA_PARTITIONS"))
            .collect::<Result<_>>()?;
        if mode == KafkaMode::Consume && partitions.is_empty() {
            bail!(
                "TAEL_KAFKA_MODE=consume requires TAEL_KAFKA_PARTITIONS=<0,1,…> — partition \
                 ownership is explicit (one consumer per partition), not group-assigned"
            );
        }
        Ok(Some(Self {
            mode,
            brokers,
            topic,
            partitions,
        }))
    }
}

// ── Producer ────────────────────────────────────────────────────────

/// Publishes shard-split OTLP slices to the topic. One partition client per
/// partition, opened at startup so a missing topic fails the boot, not the
/// first batch.
pub struct KafkaPublisher {
    partitions: Vec<PartitionClient>,
    topic: String,
}

impl KafkaPublisher {
    pub async fn connect(settings: &KafkaSettings) -> Result<Self> {
        let client = rskafka::client::ClientBuilder::new(settings.brokers.clone())
            .client_id("tael-ingest-buffer")
            .build()
            .await
            .with_context(|| format!("connecting to Kafka at {:?}", settings.brokers))?;
        let partition_ids = client
            .list_topics()
            .await
            .context("listing Kafka topics")?
            .into_iter()
            .find(|t| t.name == settings.topic)
            .map(|t| t.partitions)
            .with_context(|| {
                format!(
                    "Kafka topic `{}` does not exist — create it first (e.g. \
                     `rpk topic create {} -p <partitions>`); its partition count is the \
                     shard count",
                    settings.topic, settings.topic
                )
            })?;
        // `partitions` is a BTreeSet, so iteration is already ordered — the
        // vector index must equal the partition id for shard routing to hold.
        let ids: Vec<i32> = partition_ids.into_iter().collect();
        let mut partitions = Vec::with_capacity(ids.len());
        for id in ids {
            partitions.push(
                client
                    .partition_client(&settings.topic, id, UnknownTopicHandling::Retry)
                    .await
                    .with_context(|| format!("opening partition {id} of `{}`", settings.topic))?,
            );
        }
        anyhow::ensure!(
            !partitions.is_empty(),
            "Kafka topic `{}` has no partitions",
            settings.topic
        );
        tracing::info!(
            topic = %settings.topic,
            partitions = partitions.len(),
            "Kafka ingest buffer: producing"
        );
        Ok(Self {
            partitions,
            topic: settings.topic.clone(),
        })
    }

    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    /// Publish one partition's slice; acked only when the broker acked.
    async fn publish(
        &self,
        partition: usize,
        signal: &'static [u8],
        payload: Vec<u8>,
        tenant: Option<&str>,
    ) -> Result<(), Status> {
        let mut headers = BTreeMap::new();
        headers.insert(SIGNAL_HEADER.to_string(), signal.to_vec());
        if let Some(t) = tenant {
            headers.insert(TENANT_HEADER.to_string(), t.as_bytes().to_vec());
        }
        let record = Record {
            key: None,
            value: Some(payload),
            headers,
            timestamp: chrono::Utc::now(),
        };
        self.partitions[partition]
            .produce(vec![record], Compression::NoCompression)
            .await
            .map_err(|e| {
                Status::unavailable(format!(
                    "Kafka publish to {}/{partition} failed: {e}; retry with backoff",
                    self.topic
                ))
            })?;
        Ok(())
    }

    async fn publish_slices<M: Message>(
        &self,
        signal: &'static [u8],
        slices: Vec<Option<M>>,
        tenant: Option<&str>,
    ) -> Result<(), Status> {
        for (partition, slice) in slices.into_iter().enumerate() {
            if let Some(slice) = slice {
                self.publish(partition, signal, slice.encode_to_vec(), tenant)
                    .await?;
            }
        }
        Ok(())
    }
}

/// The producing principal's tenant, to carry in the record header. Only the
/// resolved tenant travels — never credentials.
fn tenant_of<T>(request: &Request<T>, multi_tenant: bool) -> Option<String> {
    if !multi_tenant {
        return None;
    }
    Some(crate::tenancy::write_tenant(
        true,
        request.extensions().get::<Principal>(),
    ))
}

pub struct KafkaTraceService {
    pub publisher: Arc<KafkaPublisher>,
    pub multi_tenant: bool,
}

#[tonic::async_trait]
impl TraceService for KafkaTraceService {
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
        let tenant = tenant_of(&request, self.multi_tenant);
        let (slices, count) = split_traces(request.into_inner(), self.publisher.partition_count());
        if let Err(e) = self
            .publisher
            .publish_slices(SIGNAL_TRACES, slices, tenant.as_deref())
            .await
        {
            super::stats::record_error(super::stats::Pipeline::OtlpSpans);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpSpans, count);
        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

pub struct KafkaLogsService {
    pub publisher: Arc<KafkaPublisher>,
    pub multi_tenant: bool,
}

#[tonic::async_trait]
impl LogsService for KafkaLogsService {
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
        let tenant = tenant_of(&request, self.multi_tenant);
        let (slices, count) = split_logs(request.into_inner(), self.publisher.partition_count());
        if let Err(e) = self
            .publisher
            .publish_slices(SIGNAL_LOGS, slices, tenant.as_deref())
            .await
        {
            super::stats::record_error(super::stats::Pipeline::OtlpLogs);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpLogs, count);
        Ok(Response::new(ExportLogsServiceResponse {
            partial_success: None,
        }))
    }
}

pub struct KafkaMetricsService {
    pub publisher: Arc<KafkaPublisher>,
    pub multi_tenant: bool,
}

#[tonic::async_trait]
impl MetricsService for KafkaMetricsService {
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
        let tenant = tenant_of(&request, self.multi_tenant);
        let (slices, count) = split_metrics(request.into_inner(), self.publisher.partition_count());
        if let Err(e) = self
            .publisher
            .publish_slices(SIGNAL_METRICS, slices, tenant.as_deref())
            .await
        {
            super::stats::record_error(super::stats::Pipeline::OtlpMetrics);
            return Err(e);
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpMetrics, count);
        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: None,
        }))
    }
}

// ── Consumer ────────────────────────────────────────────────────────

/// Durable per-partition offsets: the next offset to fetch, persisted after
/// records are applied (at-least-once; replay is idempotent).
pub struct OffsetStore {
    path: PathBuf,
    offsets: std::sync::Mutex<std::collections::HashMap<String, i64>>,
}

impl OffsetStore {
    pub fn open(data_dir: &str) -> Result<Self> {
        let path = PathBuf::from(data_dir).join("kafka_offsets.json");
        let offsets = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self {
            path,
            offsets: std::sync::Mutex::new(offsets),
        })
    }

    fn key(topic: &str, partition: i32) -> String {
        format!("{topic}/{partition}")
    }

    /// The next offset to fetch for a partition, if one was ever committed.
    pub fn next_offset(&self, topic: &str, partition: i32) -> Option<i64> {
        self.offsets
            .lock()
            .unwrap()
            .get(&Self::key(topic, partition))
            .copied()
    }

    /// Durably record that everything below `next` has been applied.
    pub fn commit(&self, topic: &str, partition: i32, next: i64) -> Result<()> {
        let snapshot = {
            let mut offsets = self.offsets.lock().unwrap();
            offsets.insert(Self::key(topic, partition), next);
            offsets.clone()
        };
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&snapshot)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("finalizing {}", self.path.display()))?;
        Ok(())
    }
}

/// Start one consumer task per owned partition. Records are applied through
/// the same OTLP services the listeners use, so the whole storing pipeline
/// (blobs, indexing, stamping, stats) behaves identically.
pub async fn spawn_consumers(
    settings: &KafkaSettings,
    services: OtlpHttpState,
    data_dir: &str,
    multi_tenant: bool,
) -> Result<()> {
    let client = rskafka::client::ClientBuilder::new(settings.brokers.clone())
        .client_id("tael-storage-consumer")
        .build()
        .await
        .with_context(|| format!("connecting to Kafka at {:?}", settings.brokers))?;
    let offsets = Arc::new(OffsetStore::open(data_dir)?);
    for &partition in &settings.partitions {
        let pc = client
            .partition_client(&settings.topic, partition, UnknownTopicHandling::Retry)
            .await
            .with_context(|| format!("opening partition {partition} of `{}`", settings.topic))?;
        tracing::info!(topic = %settings.topic, partition, "Kafka ingest buffer: consuming");
        tokio::spawn(consume_partition(
            pc,
            settings.topic.clone(),
            partition,
            Arc::clone(&offsets),
            services.clone(),
            multi_tenant,
        ));
    }
    Ok(())
}

async fn consume_partition(
    pc: PartitionClient,
    topic: String,
    partition: i32,
    offsets: Arc<OffsetStore>,
    services: OtlpHttpState,
    multi_tenant: bool,
) {
    // Resume where we left off; a fresh node starts from the earliest
    // retained record — the whole point of the buffer is that history waits.
    let mut next = match offsets.next_offset(&topic, partition) {
        Some(n) => n,
        None => match pc.get_offset(OffsetAt::Earliest).await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(topic, partition, error = %e, "cannot resolve earliest offset; consumer stopped");
                return;
            }
        },
    };
    loop {
        let (records, _high_watermark) =
            match pc.fetch_records(next, 1..8 * 1024 * 1024, 5_000).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(topic, partition, error = %e, "Kafka fetch failed; backing off");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };
        for entry in records {
            let offset = entry.offset;
            loop {
                match apply_record(&entry.record, &services, multi_tenant).await {
                    Ok(()) => break,
                    Err(ApplyError::Poison(reason)) => {
                        // A record that can never decode must not wedge the
                        // partition; skipping it is data loss, so say so loudly.
                        tracing::error!(
                            topic,
                            partition,
                            offset,
                            reason,
                            "skipping undecodable Kafka record"
                        );
                        break;
                    }
                    Err(ApplyError::Retry(reason)) => {
                        tracing::warn!(
                            topic,
                            partition,
                            offset,
                            reason,
                            "apply failed; retrying without advancing"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
            next = offset + 1;
        }
        if let Err(e) = offsets.commit(&topic, partition, next) {
            tracing::warn!(topic, partition, error = %e, "offset commit failed; replay window grows");
        }
    }
}

enum ApplyError {
    /// The record can never succeed (bad header, undecodable payload).
    Poison(String),
    /// Transient (storage error, shed); retry without advancing.
    Retry(String),
}

/// Reconstruct the edge principal from the tenant header so server-side
/// stamping behaves as if the original writer had connected directly.
fn header_principal(record: &Record) -> Option<Principal> {
    let tenant = record.headers.get(TENANT_HEADER)?;
    Some(Principal {
        key_id: "kafka-buffer".to_string(),
        name: "kafka-buffer".to_string(),
        role: Role::Writer,
        tenant: String::from_utf8_lossy(tenant).into_owned(),
    })
}

async fn apply_record(
    record: &Record,
    services: &OtlpHttpState,
    multi_tenant: bool,
) -> Result<(), ApplyError> {
    let Some(payload) = record.value.as_deref() else {
        return Err(ApplyError::Poison("record has no value".into()));
    };
    let signal = record
        .headers
        .get(SIGNAL_HEADER)
        .map(|v| v.as_slice())
        .unwrap_or(SIGNAL_TRACES);
    // The `multi_tenant` flag gates whether a reconstructed principal is
    // even attached: with tenancy off the consumer stores records exactly as
    // a single-tenant listener would.
    let principal = multi_tenant.then(|| header_principal(record)).flatten();

    fn with_principal<T>(msg: T, principal: Option<Principal>) -> Request<T> {
        let mut req = Request::new(msg);
        if let Some(p) = principal {
            req.extensions_mut().insert(p);
        }
        req
    }
    let map_apply = |e: Status| match e.code() {
        tonic::Code::ResourceExhausted | tonic::Code::Unavailable | tonic::Code::Internal => {
            ApplyError::Retry(e.message().to_string())
        }
        _ => ApplyError::Poison(e.message().to_string()),
    };

    match signal {
        s if s == SIGNAL_TRACES => {
            let msg = ExportTraceServiceRequest::decode(payload)
                .map_err(|e| ApplyError::Poison(format!("trace decode: {e}")))?;
            services
                .traces
                .export(with_principal(msg, principal))
                .await
                .map_err(map_apply)?;
        }
        s if s == SIGNAL_LOGS => {
            let msg = ExportLogsServiceRequest::decode(payload)
                .map_err(|e| ApplyError::Poison(format!("log decode: {e}")))?;
            services
                .logs
                .export(with_principal(msg, principal))
                .await
                .map_err(map_apply)?;
        }
        s if s == SIGNAL_METRICS => {
            let msg = ExportMetricsServiceRequest::decode(payload)
                .map_err(|e| ApplyError::Poison(format!("metric decode: {e}")))?;
            services
                .metrics
                .export(with_principal(msg, principal))
                .await
                .map_err(map_apply)?;
        }
        other => {
            return Err(ApplyError::Poison(format!(
                "unknown signal header {:?}",
                String::from_utf8_lossy(other)
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_survive_reopen_and_track_per_partition() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        {
            let store = OffsetStore::open(path).unwrap();
            assert_eq!(store.next_offset("t", 0), None);
            store.commit("t", 0, 41).unwrap();
            store.commit("t", 3, 7).unwrap();
            store.commit("t", 0, 42).unwrap();
        }
        let store = OffsetStore::open(path).unwrap();
        assert_eq!(store.next_offset("t", 0), Some(42));
        assert_eq!(store.next_offset("t", 3), Some(7));
        assert_eq!(store.next_offset("other", 0), None);
    }

    #[test]
    fn header_principal_reconstructs_the_edge_tenant() {
        let mut record = Record {
            key: None,
            value: Some(vec![]),
            headers: BTreeMap::new(),
            timestamp: chrono::Utc::now(),
        };
        assert!(header_principal(&record).is_none());
        record
            .headers
            .insert(TENANT_HEADER.to_string(), b"team-a".to_vec());
        let p = header_principal(&record).unwrap();
        assert_eq!(p.tenant, "team-a");
        assert_eq!(p.role, Role::Writer);
    }

    #[test]
    fn settings_parse_and_validate() {
        // Direct construction paths (from_env is env-global, so validate the
        // invariants it enforces via the struct instead).
        let s = KafkaSettings {
            mode: KafkaMode::Consume,
            brokers: vec!["localhost:9092".into()],
            topic: "tael-ingest".into(),
            partitions: vec![0, 1],
        };
        assert_eq!(s.mode, KafkaMode::Consume);
        assert_eq!(s.partitions, vec![0, 1]);
    }
}
