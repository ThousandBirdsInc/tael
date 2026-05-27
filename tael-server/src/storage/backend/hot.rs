//! LSM hot tier for `TaelBackend`, backed by `fjall` (pure-Rust).
//!
//! Holds recent data and serves the core per-signal reads. Each signal lives in
//! its own keyspace with keys chosen for its dominant access pattern (see
//! `docs/tael-backend-design.md` → "Hot tier"):
//!
//! - **spans** — `spans` keyed `trace_id\0span_id` (span-tree prefix scan) and
//!   `spans_time` keyed `be(start_ns)+trace_id\0span_id` (recent time scan).
//! - **logs** — `logs` keyed `be(ts)+seq`; filtered scans for service/severity.
//! - **metrics** — `metrics` keyed `name\0be(ts)+seq`; series range scans.
//!
//! Records are stored as JSON (consistent with the rest of the codebase).
//! Phase 4 serves `query_traces`/`get_trace`/`list_services`/`query_logs`/
//! `query_metrics` here; heavier analytics still run on the DuckDB projection
//! until DataFusion (Phase 6).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};

use crate::storage::models::{
    LogQuery, LogRecord, LogSeverity, MetricPoint, MetricQuery, MetricType, ServiceInfo, Span,
    SpanStatus, TraceQuery,
};

const SEP: u8 = 0x00;

pub struct HotTier {
    db: Database,
    spans: Keyspace,
    spans_time: Keyspace,
    logs: Keyspace,
    metrics: Keyspace,
    /// Disambiguates records that share a timestamp within one process.
    seq: AtomicU64,
}

impl HotTier {
    pub fn open(data_dir: &str) -> Result<Self> {
        let path = std::path::Path::new(data_dir).join("hot");
        let db = Database::builder(&path).open()?;
        let spans = db.keyspace("spans", KeyspaceCreateOptions::default)?;
        let spans_time = db.keyspace("spans_time", KeyspaceCreateOptions::default)?;
        let logs = db.keyspace("logs", KeyspaceCreateOptions::default)?;
        let metrics = db.keyspace("metrics", KeyspaceCreateOptions::default)?;
        // Seed the seq from wall-clock nanos so log/metric keys stay unique
        // across process restarts (same-ts collisions would otherwise overwrite).
        let seed = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
        Ok(Self {
            db,
            spans,
            spans_time,
            logs,
            metrics,
            seq: AtomicU64::new(seed),
        })
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    /// Fsync the LSM journal. The write path persists with `Buffer` after every
    /// apply (the WAL is the durability boundary); this `SyncAll` is the
    /// stronger flush used on graceful shutdown so a restart replays less.
    pub fn flush(&self) -> Result<()> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }

    // ── Spans ───────────────────────────────────────────────────────

    pub fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        for span in spans {
            let value = serde_json::to_vec(span)?;
            let primary = span_key(&span.trace_id, &span.span_id);
            self.spans.insert(&primary, &value)?;
            self.spans_time
                .insert(span_time_key(span), primary.as_slice())?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(())
    }

    pub fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let mut prefix = trace_id.as_bytes().to_vec();
        prefix.push(SEP);
        let mut spans = Vec::new();
        for kv in self.spans.prefix(&prefix) {
            let value = kv.value()?;
            spans.push(serde_json::from_slice::<Span>(&value)?);
        }
        spans.sort_by_key(|s| s.start_time);
        Ok(spans)
    }

    pub fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let cutoff = query
            .last_seconds
            .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
        let mut out = Vec::new();
        // Most-recent first: reverse iteration over the time index.
        for kv in self.spans_time.iter().rev() {
            let primary = kv.value()?;
            let Some(raw) = self.spans.get(&primary)? else {
                continue;
            };
            let span: Span = serde_json::from_slice(&raw)?;
            if span_matches(&span, query, cutoff) {
                out.push(span);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Remove and return all spans whose `start_ns` is before `cutoff_ns`.
    /// Used by the compactor to roll aged spans into the cold tier.
    pub fn evict_spans_before(&self, cutoff_ns: i64) -> Result<Vec<Span>> {
        let upper = cutoff_ns.to_be_bytes();
        let mut evicted = Vec::new();
        let mut time_keys: Vec<Vec<u8>> = Vec::new();
        let mut primary_keys: Vec<Vec<u8>> = Vec::new();
        // `spans_time` keys begin with be(start_ns); range `..be(cutoff)` is
        // exactly the spans older than the cutoff.
        for kv in self.spans_time.range(..upper.as_slice()) {
            let (tkey, pkey) = kv.into_inner()?;
            if let Some(raw) = self.spans.get(&pkey)? {
                evicted.push(serde_json::from_slice::<Span>(&raw)?);
                primary_keys.push(pkey.to_vec());
            }
            time_keys.push(tkey.to_vec());
        }
        for pk in &primary_keys {
            self.spans.remove(pk)?;
        }
        for tk in &time_keys {
            self.spans_time.remove(tk)?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(evicted)
    }

    pub fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        struct Agg {
            span_count: i64,
            traces: std::collections::HashSet<String>,
            total_ms: f64,
            errors: i64,
        }
        let mut by_svc: HashMap<String, Agg> = HashMap::new();
        for kv in self.spans.iter() {
            let span: Span = serde_json::from_slice(&kv.value()?)?;
            let agg = by_svc.entry(span.service.clone()).or_insert_with(|| Agg {
                span_count: 0,
                traces: std::collections::HashSet::new(),
                total_ms: 0.0,
                errors: 0,
            });
            agg.span_count += 1;
            agg.traces.insert(span.trace_id.clone());
            agg.total_ms += span.duration_ms;
            if matches!(span.status, SpanStatus::Error) {
                agg.errors += 1;
            }
        }
        let mut services: Vec<ServiceInfo> = by_svc
            .into_iter()
            .map(|(name, a)| ServiceInfo {
                name,
                span_count: a.span_count,
                trace_count: a.traces.len() as i64,
                avg_duration_ms: if a.span_count > 0 {
                    a.total_ms / a.span_count as f64
                } else {
                    0.0
                },
                error_rate: if a.span_count > 0 {
                    a.errors as f64 / a.span_count as f64
                } else {
                    0.0
                },
            })
            .collect();
        services.sort_by(|a, b| b.span_count.cmp(&a.span_count));
        Ok(services)
    }

    // ── Logs ────────────────────────────────────────────────────────

    pub fn insert_logs(&self, logs: &[LogRecord]) -> Result<()> {
        for log in logs {
            let ts = log.timestamp.timestamp_nanos_opt().unwrap_or(0);
            let mut key = ts.to_be_bytes().to_vec();
            key.extend_from_slice(&self.next_seq().to_be_bytes());
            self.logs.insert(key, serde_json::to_vec(log)?)?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(())
    }

    /// Remove and return all logs older than `cutoff_ns` (keys begin with
    /// `be(ts)`, so a range scan is exact).
    pub fn evict_logs_before(&self, cutoff_ns: i64) -> Result<Vec<LogRecord>> {
        let upper = cutoff_ns.to_be_bytes();
        let mut evicted = Vec::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for kv in self.logs.range(..upper.as_slice()) {
            let (k, v) = kv.into_inner()?;
            evicted.push(serde_json::from_slice::<LogRecord>(&v)?);
            keys.push(k.to_vec());
        }
        for k in &keys {
            self.logs.remove(k)?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(evicted)
    }

    /// Remove and return all metric points older than `cutoff_ns`. Metric keys
    /// lead with the name (not time), so this is a full scan with a ts check.
    pub fn evict_metrics_before(&self, cutoff_ns: i64) -> Result<Vec<MetricPoint>> {
        let mut evicted = Vec::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for kv in self.metrics.iter() {
            let (k, v) = kv.into_inner()?;
            let m: MetricPoint = serde_json::from_slice(&v)?;
            if m.timestamp.timestamp_nanos_opt().unwrap_or(0) < cutoff_ns {
                evicted.push(m);
                keys.push(k.to_vec());
            }
        }
        for k in &keys {
            self.metrics.remove(k)?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(evicted)
    }

    pub fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let cutoff = query
            .last_seconds
            .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
        let mut out = Vec::new();
        for kv in self.logs.iter().rev() {
            let log: LogRecord = serde_json::from_slice(&kv.value()?)?;
            if log_matches(&log, query, cutoff) {
                out.push(log);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    // ── Metrics ─────────────────────────────────────────────────────

    pub fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        for m in metrics {
            let ts = m.timestamp.timestamp_nanos_opt().unwrap_or(0);
            let mut key = m.name.as_bytes().to_vec();
            key.push(SEP);
            key.extend_from_slice(&ts.to_be_bytes());
            key.extend_from_slice(&self.next_seq().to_be_bytes());
            self.metrics.insert(key, serde_json::to_vec(m)?)?;
        }
        self.db.persist(PersistMode::Buffer)?;
        Ok(())
    }

    pub fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let limit = query.limit.unwrap_or(500) as usize;
        let cutoff = query
            .last_seconds
            .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
        let mut out = Vec::new();
        for kv in self.metrics.iter().rev() {
            let m: MetricPoint = serde_json::from_slice(&kv.value()?)?;
            if metric_matches(&m, query, cutoff) {
                out.push(m);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }
}

/// Filter predicate for `query_logs`, shared with the cold-tier union.
pub(super) fn log_matches(
    log: &LogRecord,
    query: &LogQuery,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(ref svc) = query.service {
        if &log.service != svc {
            return false;
        }
    }
    if let Some(ref sev) = query.severity {
        if log.severity != LogSeverity::from_str(sev) {
            return false;
        }
    }
    if let Some(ref needle) = query.body_contains {
        if !log.body.contains(needle.as_str()) {
            return false;
        }
    }
    if let Some(ref tid) = query.trace_id {
        if log.trace_id.as_deref() != Some(tid.as_str()) {
            return false;
        }
    }
    if let Some(c) = cutoff {
        if log.timestamp < c {
            return false;
        }
    }
    true
}

/// Filter predicate for `query_metrics`, shared with the cold-tier union.
pub(super) fn metric_matches(
    m: &MetricPoint,
    query: &MetricQuery,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(ref svc) = query.service {
        if &m.service != svc {
            return false;
        }
    }
    if let Some(ref name) = query.name {
        if &m.name != name {
            return false;
        }
    }
    if let Some(ref mt) = query.metric_type {
        if m.metric_type != MetricType::from_str(mt) {
            return false;
        }
    }
    if let Some(c) = cutoff {
        if m.timestamp < c {
            return false;
        }
    }
    true
}

// ── Key encoding ────────────────────────────────────────────────────

fn span_key(trace_id: &str, span_id: &str) -> Vec<u8> {
    let mut k = trace_id.as_bytes().to_vec();
    k.push(SEP);
    k.extend_from_slice(span_id.as_bytes());
    k
}

fn span_time_key(span: &Span) -> Vec<u8> {
    let ts = span.start_time.timestamp_nanos_opt().unwrap_or(0);
    let mut k = ts.to_be_bytes().to_vec();
    k.extend_from_slice(span.trace_id.as_bytes());
    k.push(SEP);
    k.extend_from_slice(span.span_id.as_bytes());
    k
}

/// Mirror `DuckDbStore::query_traces` filter semantics in memory. Shared with
/// the cold-tier union in `TaelBackend`.
pub(super) fn span_matches(
    span: &Span,
    query: &TraceQuery,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(ref svc) = query.service {
        if &span.service != svc {
            return false;
        }
    }
    if let Some(ref op) = query.operation {
        if !span.operation.contains(op.as_str()) {
            return false;
        }
    }
    if let Some(min) = query.min_duration_ms {
        if span.duration_ms < min {
            return false;
        }
    }
    if let Some(max) = query.max_duration_ms {
        if span.duration_ms > max {
            return false;
        }
    }
    if let Some(ref status) = query.status {
        if span.status.to_string() != *status {
            return false;
        }
    }
    if let Some(c) = cutoff {
        if span.start_time < c {
            return false;
        }
    }
    for (k, v) in &query.attributes {
        if span.attributes.get(k).map(|s| s.as_str()) != Some(v.as_str()) {
            return false;
        }
    }
    true
}
