//! LSM hot tier for `TaelBackend`, backed by `fjall` (pure-Rust).
//!
//! Holds recent data and serves the core per-signal reads. Each signal lives in
//! its own keyspace with keys chosen for its dominant access pattern (see
//! `docs/tael-backend-design.md` → "Hot tier"):
//!
//! - **spans** — `spans` keyed `trace_id\0span_id` (span-tree prefix scan),
//!   plus three time-ordered indexes over it: `spans_time` keyed
//!   `be(start_ns)+trace_id\0span_id`, `spans_svc` keyed
//!   `service\0be(start_ns)+trace_id\0span_id`, and `spans_err` (error spans
//!   only) keyed like `spans_time`. See [`SpanIndex`].
//! - **logs** — `logs` keyed `be(ts)+content_hash`; filtered scans for
//!   service/severity.
//! - **metrics** — `metrics` keyed `name\0be(ts)+content_hash`; series scans.
//!
//! **Every key is a pure function of the record it stores.** The WAL only
//! advances its cursor at checkpoints, so a crash replays records that were
//! already applied; a key with a sequence number or a timestamp in it would
//! make that replay insert duplicates instead of overwriting. Records are
//! stored with the shared MessagePack [`codec`].
//!
//! Phase 4 serves `query_traces`/`get_trace`/`list_services`/`query_logs`/
//! `query_metrics` here; heavier analytics still run on the DuckDB projection
//! until DataFusion (Phase 6).

use std::collections::HashMap;

use anyhow::Result;
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};

use super::codec;
use crate::storage::models::{
    LogQuery, LogRecord, LogSeverity, MetricPoint, MetricQuery, MetricType, ServiceInfo, Span,
    SpanStatus, TraceQuery,
};

const SEP: u8 = 0x00;

pub struct HotTier {
    db: Database,
    spans: Keyspace,
    spans_time: Keyspace,
    spans_svc: Keyspace,
    spans_err: Keyspace,
    logs: Keyspace,
    metrics: Keyspace,
}

impl HotTier {
    pub fn open(data_dir: &str) -> Result<Self> {
        let path = std::path::Path::new(data_dir).join("hot");
        let db = Database::builder(&path).open()?;
        let spans = db.keyspace("spans", KeyspaceCreateOptions::default)?;
        let spans_time = db.keyspace("spans_time", KeyspaceCreateOptions::default)?;
        let spans_svc = db.keyspace("spans_svc", KeyspaceCreateOptions::default)?;
        let spans_err = db.keyspace("spans_err", KeyspaceCreateOptions::default)?;
        let logs = db.keyspace("logs", KeyspaceCreateOptions::default)?;
        let metrics = db.keyspace("metrics", KeyspaceCreateOptions::default)?;
        Ok(Self {
            db,
            spans,
            spans_time,
            spans_svc,
            spans_err,
            logs,
            metrics,
        })
    }

    /// Fsync the LSM journal. The write path persists with `Buffer` after every
    /// apply (the WAL is the durability boundary); this `SyncAll` is the
    /// stronger flush a checkpoint takes before consuming WAL records, and that
    /// graceful shutdown takes so a restart replays less.
    pub fn flush(&self) -> Result<()> {
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }

    // ── Spans ───────────────────────────────────────────────────────

    pub fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        for span in spans {
            let value = codec::encode(span)?;
            let primary = span_key(&span.trace_id, &span.span_id);
            let index_value = encode_index_entry(span, &primary);
            self.spans.insert(&primary, &value)?;
            self.spans_time
                .insert(span_time_key(span), index_value.as_slice())?;
            self.spans_svc
                .insert(span_service_key(span), index_value.as_slice())?;
            // Only error spans get an entry, so the index is small and a
            // "what is failing?" scan touches nothing else.
            if matches!(span.status, SpanStatus::Error) {
                self.spans_err
                    .insert(span_time_key(span), index_value.as_slice())?;
            }
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
            spans.push(codec::decode::<Span>(&value)?);
        }
        spans.sort_by_key(|s| s.start_time);
        Ok(spans)
    }

    pub fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        Ok(self.scan_spans(query)?.spans)
    }

    /// Scan the spans matching `query`, newest first, reporting how the scan
    /// was executed.
    ///
    /// Two things keep this from being a full table scan. The index is chosen
    /// from the query's filters, so a service-scoped or error-only query walks
    /// only rows that already satisfy that filter. And each index entry carries
    /// a covering header — service, status, duration — so the remaining rows
    /// are rejected on those filters without a second keyspace lookup and
    /// without decoding the span. Only a candidate that survives all of that is
    /// materialized.
    pub fn scan_spans(&self, query: &TraceQuery) -> Result<SpanScan> {
        let limit = query.limit.unwrap_or(100) as usize;
        let cutoff = query
            .last_seconds
            .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
        let cutoff_ns = cutoff.and_then(|c| c.timestamp_nanos_opt()).unwrap_or(0);

        let index = SpanIndex::choose(query);
        let mut scan = SpanScan {
            index,
            rows_scanned: 0,
            rows_decoded: 0,
            spans: Vec::new(),
        };
        if limit == 0 {
            return Ok(scan);
        }

        // `rev()` over a time-ordered index gives newest-first; the range's
        // lower bound means an aged-out span is never even visited.
        let entries: Box<dyn Iterator<Item = fjall::Guard>> = match index {
            SpanIndex::Service => {
                let service = query.service.as_deref().unwrap_or_default();
                let (lo, hi) = service_bounds(service, cutoff_ns);
                Box::new(self.spans_svc.range(lo..hi).rev())
            }
            SpanIndex::Errors => {
                Box::new(self.spans_err.range(time_lower_bound(cutoff_ns)..).rev())
            }
            SpanIndex::Time => Box::new(self.spans_time.range(time_lower_bound(cutoff_ns)..).rev()),
        };

        for kv in entries {
            scan.rows_scanned += 1;
            let value = kv.value()?;
            let Some(entry) = decode_index_entry(&value) else {
                continue;
            };
            if !entry.could_match(query) {
                continue;
            }
            let Some(raw) = self.spans.get(entry.primary)? else {
                continue;
            };
            scan.rows_decoded += 1;
            let span: Span = codec::decode(&raw)?;
            if span_matches(&span, query, cutoff) {
                scan.spans.push(span);
                if scan.spans.len() >= limit {
                    break;
                }
            }
        }
        Ok(scan)
    }

    /// Visit every span in a window without reading a single span.
    ///
    /// The aggregates behind `summarize` and `anomalies` need five things from
    /// each span — trace id, service, operation, duration, whether it errored —
    /// and all five live in a span index: four in the covering header and the
    /// trace id in the key. So the whole report can be computed from an index
    /// scan, which is the difference between decoding every span in the window
    /// and touching none of them. The numbers stay exact; nothing here samples
    /// or estimates.
    ///
    /// Returns the number of entries visited.
    pub fn for_each_span_fact(
        &self,
        service: Option<&str>,
        cutoff_ns: i64,
        mut visit: impl FnMut(SpanFacts<'_>),
    ) -> Result<u64> {
        let mut visited = 0u64;
        let entries: Box<dyn Iterator<Item = fjall::Guard>> = match service {
            Some(service) => {
                let (lo, hi) = service_bounds(service, cutoff_ns);
                Box::new(self.spans_svc.range(lo..hi))
            }
            None => Box::new(self.spans_time.range(time_lower_bound(cutoff_ns)..)),
        };
        for kv in entries {
            let (key, value) = kv.into_inner()?;
            let Some(entry) = decode_index_entry(&value) else {
                continue;
            };
            let trace_id = match service {
                Some(_) => trace_id_of_service_key(&key),
                None => trace_id_of_time_key(&key),
            };
            let Some(trace_id) = trace_id else { continue };
            visited += 1;
            visit(SpanFacts {
                trace_id,
                service: entry.service,
                operation: entry.operation,
                duration_ms: entry.duration_ms,
                is_error: entry.status == STATUS_ERROR,
            });
        }
        Ok(visited)
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
            let (tkey, value) = kv.into_inner()?;
            if let Some(entry) = decode_index_entry(&value)
                && let Some(raw) = self.spans.get(entry.primary)?
            {
                evicted.push(codec::decode::<Span>(&raw)?);
                primary_keys.push(entry.primary.to_vec());
            }
            time_keys.push(tkey.to_vec());
        }
        for pk in &primary_keys {
            self.spans.remove(pk)?;
        }
        // Every index key is derivable from the span, so the evicted records
        // are enough to clean up all of them — no reverse lookup needed.
        for span in &evicted {
            self.spans_svc.remove(span_service_key(span))?;
            if matches!(span.status, SpanStatus::Error) {
                self.spans_err.remove(span_time_key(span))?;
            }
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
        // Reads the service index, not the spans themselves: the covering
        // header holds every field this needs except the trace id, which is in
        // the key. Nothing here decodes a span.
        for kv in self.spans_svc.iter() {
            let (key, value) = kv.into_inner()?;
            let Some(entry) = decode_index_entry(&value) else {
                continue;
            };
            let Some(trace_id) = trace_id_of_service_key(&key) else {
                continue;
            };
            let agg = by_svc
                .entry(entry.service.to_string())
                .or_insert_with(|| Agg {
                    span_count: 0,
                    traces: std::collections::HashSet::new(),
                    total_ms: 0.0,
                    errors: 0,
                });
            agg.span_count += 1;
            if !agg.traces.contains(trace_id) {
                agg.traces.insert(trace_id.to_string());
            }
            agg.total_ms += entry.duration_ms;
            if entry.status == STATUS_ERROR {
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
            let value = codec::encode(log)?;
            let ts = log.timestamp.timestamp_nanos_opt().unwrap_or(0);
            let mut key = ts.to_be_bytes().to_vec();
            key.extend_from_slice(&codec::content_hash(&value).to_be_bytes());
            self.logs.insert(key, &value)?;
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
            evicted.push(codec::decode::<LogRecord>(&v)?);
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
            let m: MetricPoint = codec::decode(&v)?;
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
        // Keys lead with be(ts), so an age cutoff becomes a range bound rather
        // than a predicate applied after decoding every record in the tier.
        let lower = time_lower_bound(cutoff.and_then(|c| c.timestamp_nanos_opt()).unwrap_or(0));
        for kv in self.logs.range(lower..).rev() {
            let log: LogRecord = codec::decode(&kv.value()?)?;
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
            let value = codec::encode(m)?;
            let ts = m.timestamp.timestamp_nanos_opt().unwrap_or(0);
            let mut key = m.name.as_bytes().to_vec();
            key.push(SEP);
            key.extend_from_slice(&ts.to_be_bytes());
            key.extend_from_slice(&codec::content_hash(&value).to_be_bytes());
            self.metrics.insert(key, &value)?;
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
        // Metric keys lead with the name, so a named query is a prefix scan and
        // only an unnamed one has to walk every series.
        let range: Box<dyn Iterator<Item = fjall::Guard>> = match &query.name {
            Some(name) => {
                let (lo, hi) = series_bounds(name);
                Box::new(self.metrics.range(lo..hi).rev())
            }
            None => Box::new(self.metrics.iter().rev()),
        };
        for kv in range {
            let m: MetricPoint = codec::decode(&kv.value()?)?;
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
    if let Some(tenant) = &query.tenant
        && crate::tenancy::owner_of(&log.attributes) != tenant.as_str()
    {
        return false;
    }
    if let Some(ref svc) = query.service
        && &log.service != svc
    {
        return false;
    }
    if let Some(ref sev) = query.severity
        && log.severity != LogSeverity::from_str(sev)
    {
        return false;
    }
    if let Some(ref needle) = query.body_contains
        && !log.body.contains(needle.as_str())
    {
        return false;
    }
    if let Some(ref tid) = query.trace_id
        && log.trace_id.as_deref() != Some(tid.as_str())
    {
        return false;
    }
    if let Some(c) = cutoff
        && log.timestamp < c
    {
        return false;
    }
    true
}

/// Filter predicate for `query_metrics`, shared with the cold-tier union.
pub(super) fn metric_matches(
    m: &MetricPoint,
    query: &MetricQuery,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(tenant) = &query.tenant
        && crate::tenancy::owner_of(&m.attributes) != tenant.as_str()
    {
        return false;
    }
    if let Some(ref svc) = query.service
        && &m.service != svc
    {
        return false;
    }
    if let Some(ref name) = query.name
        && &m.name != name
    {
        return false;
    }
    if let Some(ref mt) = query.metric_type
        && m.metric_type != MetricType::from_str(mt)
    {
        return false;
    }
    if let Some(c) = cutoff
        && m.timestamp < c
    {
        return false;
    }
    true
}

// ── Span index selection ────────────────────────────────────────────

/// Which time-ordered index a span scan walks.
///
/// All three are ordered by start time, so any of them can be walked backwards
/// to produce newest-first results; they differ only in how many rows they make
/// the scan look at. Picking one is the whole optimization — a service-scoped
/// query on the time index has to examine every span in the window to find the
/// handful belonging to that service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanIndex {
    /// `spans_svc` — the query names a service, so only that service's spans
    /// are visited.
    Service,
    /// `spans_err` — the query wants errors, which are a small minority.
    Errors,
    /// `spans_time` — no filter narrows the scan, so it walks the window.
    Time,
}

impl SpanIndex {
    fn choose(query: &TraceQuery) -> Self {
        // Errors win over service when a query asks for both. The error index
        // holds a fraction of the tier; the service index holds the tier
        // divided by the number of services, which for a single-service
        // deployment is the whole thing. And the covering header rejects the
        // wrong service for free, so nothing is lost by narrowing on errors
        // first. This is the triage query — "what is failing in api" — so it
        // is the one worth getting right.
        if query.status.as_deref() == Some("error") {
            SpanIndex::Errors
        } else if query.service.is_some() {
            SpanIndex::Service
        } else {
            SpanIndex::Time
        }
    }

    /// The keyspace name, for `explain`.
    pub fn keyspace(self) -> &'static str {
        match self {
            SpanIndex::Service => "spans_svc",
            SpanIndex::Errors => "spans_err",
            SpanIndex::Time => "spans_time",
        }
    }
}

/// Everything an aggregate needs about one span, read straight out of a span
/// index. Borrowed from the index entry, so a visitor that wants to keep a
/// string has to say so.
pub struct SpanFacts<'a> {
    pub trace_id: &'a str,
    pub service: &'a str,
    pub operation: &'a str,
    pub duration_ms: f64,
    pub is_error: bool,
}

/// The result of a span scan, including what it cost.
///
/// `rows_scanned` versus `rows_decoded` is the number an agent needs to tell a
/// selective query from one that read the whole window and threw it away.
pub struct SpanScan {
    pub index: SpanIndex,
    pub rows_scanned: u64,
    pub rows_decoded: u64,
    pub spans: Vec<Span>,
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

/// `service\0` + the time key, so one service's spans are a contiguous,
/// time-ordered range.
fn span_service_key(span: &Span) -> Vec<u8> {
    let mut k = span.service.as_bytes().to_vec();
    k.push(SEP);
    k.extend_from_slice(&span_time_key(span));
    k
}

/// The trace id embedded in a `spans_svc` key, for readers that want it without
/// fetching the span. `service\0` + `be(ts)` + `trace_id` + `\0` + `span_id`.
fn trace_id_of_service_key(key: &[u8]) -> Option<&str> {
    let sep = key.iter().position(|&b| b == SEP)?;
    trace_id_of_time_key(key.get(sep + 1..)?)
}

/// The trace id embedded in a `spans_time` (or `spans_err`) key:
/// `be(ts)` + `trace_id` + `\0` + `span_id`.
fn trace_id_of_time_key(key: &[u8]) -> Option<&str> {
    let rest = key.get(8..)?;
    let end = rest.iter().position(|&b| b == SEP)?;
    std::str::from_utf8(&rest[..end]).ok()
}

/// Inclusive lower bound for a `be(ts)`-prefixed keyspace.
///
/// Big-endian i64 only sorts correctly for non-negative values, and a negative
/// cutoff would sort *above* every real timestamp and hide the whole tier — so
/// pre-epoch cutoffs clamp to "everything" rather than "nothing".
fn time_lower_bound(cutoff_ns: i64) -> Vec<u8> {
    cutoff_ns.max(0).to_be_bytes().to_vec()
}

/// Half-open bounds covering one service's spans at or after `cutoff_ns`.
///
/// The upper bound is the successor of `service\0`: `SEP` is `0x00`, so
/// incrementing it to `0x01` lands just past every key under that prefix
/// without needing to know how long the keys are.
fn service_bounds(service: &str, cutoff_ns: i64) -> (Vec<u8>, Vec<u8>) {
    let mut lo = service.as_bytes().to_vec();
    lo.push(SEP);
    let mut hi = lo.clone();
    lo.extend_from_slice(&time_lower_bound(cutoff_ns));
    let last = hi.len() - 1;
    hi[last] = SEP + 1;
    (lo, hi)
}

/// Half-open bounds covering one metric series (`name\0…`).
fn series_bounds(name: &str) -> (Vec<u8>, Vec<u8>) {
    let mut lo = name.as_bytes().to_vec();
    lo.push(SEP);
    let mut hi = lo.clone();
    let last = hi.len() - 1;
    hi[last] = SEP + 1;
    (lo, hi)
}

// ── Covering index entries ──────────────────────────────────────────

const STATUS_OK: u8 = 0;
const STATUS_ERROR: u8 = 1;
const STATUS_UNSET: u8 = 2;
/// `status` + `duration_ms` + `service_len`.
const INDEX_HEADER_LEN: usize = 1 + 8 + 2;

fn status_code(status: &SpanStatus) -> u8 {
    match status {
        SpanStatus::Ok => STATUS_OK,
        SpanStatus::Error => STATUS_ERROR,
        SpanStatus::Unset => STATUS_UNSET,
    }
}

/// A span index's value: the filters worth answering without touching the span,
/// followed by the primary key of the span itself.
///
/// `[status: u8][duration_ms: f64 le][service_len: u16 le][service]`
/// `[operation_len: u16 le][operation][primary]`.
///
/// Storing the whole span here instead would remove the second lookup entirely,
/// but at three indexes it would also quadruple the hot tier. This is the small
/// part of a span that filters and aggregates actually ask about — between
/// these four fields and the trace id in the key, `summarize` never has to read
/// a span at all.
struct IndexEntry<'a> {
    status: u8,
    duration_ms: f64,
    service: &'a str,
    operation: &'a str,
    primary: &'a [u8],
}

fn encode_index_entry(span: &Span, primary: &[u8]) -> Vec<u8> {
    let service = span.service.as_bytes();
    let operation = span.operation.as_bytes();
    let mut v =
        Vec::with_capacity(INDEX_HEADER_LEN + service.len() + 2 + operation.len() + primary.len());
    v.push(status_code(&span.status));
    v.extend_from_slice(&span.duration_ms.to_le_bytes());
    v.extend_from_slice(&(service.len() as u16).to_le_bytes());
    v.extend_from_slice(service);
    v.extend_from_slice(&(operation.len() as u16).to_le_bytes());
    v.extend_from_slice(operation);
    v.extend_from_slice(primary);
    v
}

fn decode_index_entry(bytes: &[u8]) -> Option<IndexEntry<'_>> {
    if bytes.len() < INDEX_HEADER_LEN {
        return None;
    }
    let duration_ms = f64::from_le_bytes(bytes[1..9].try_into().ok()?);
    let service_len = u16::from_le_bytes(bytes[9..11].try_into().ok()?) as usize;
    let service_end = INDEX_HEADER_LEN + service_len;
    let service = std::str::from_utf8(bytes.get(INDEX_HEADER_LEN..service_end)?).ok()?;
    let operation_len =
        u16::from_le_bytes(bytes.get(service_end..service_end + 2)?.try_into().ok()?) as usize;
    let operation_end = service_end + 2 + operation_len;
    let operation = std::str::from_utf8(bytes.get(service_end + 2..operation_end)?).ok()?;
    Some(IndexEntry {
        status: bytes[0],
        duration_ms,
        service,
        operation,
        primary: bytes.get(operation_end..)?,
    })
}

impl IndexEntry<'_> {
    /// Whether the span behind this entry could satisfy `query`.
    ///
    /// Conservative in one direction only: a `false` means the span definitely
    /// does not match, a `true` means the full predicate still has to run. Any
    /// filter this doesn't cover is simply not consulted here.
    fn could_match(&self, query: &TraceQuery) -> bool {
        if let Some(service) = &query.service
            && self.service != service.as_str()
        {
            return false;
        }
        if let Some(status) = &query.status
            && self.status != status_code(&SpanStatus::from_str(status))
        {
            return false;
        }
        if let Some(min) = query.min_duration_ms
            && self.duration_ms < min
        {
            return false;
        }
        if let Some(max) = query.max_duration_ms
            && self.duration_ms > max
        {
            return false;
        }
        if let Some(operation) = &query.operation
            && !self.operation.contains(operation.as_str())
        {
            return false;
        }
        true
    }
}

/// Mirror `DuckDbStore::query_traces` filter semantics in memory. Shared with
/// the cold-tier union in `TaelBackend`.
pub(super) fn span_matches(
    span: &Span,
    query: &TraceQuery,
    cutoff: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    if let Some(ref svc) = query.service
        && &span.service != svc
    {
        return false;
    }
    if let Some(ref op) = query.operation
        && !span.operation.contains(op.as_str())
    {
        return false;
    }
    if let Some(min) = query.min_duration_ms
        && span.duration_ms < min
    {
        return false;
    }
    if let Some(max) = query.max_duration_ms
        && span.duration_ms > max
    {
        return false;
    }
    if let Some(ref status) = query.status
        && span.status.to_string() != *status
    {
        return false;
    }
    if let Some(c) = cutoff
        && span.start_time < c
    {
        return false;
    }
    for (k, v) in &query.attributes {
        if span.attributes.get(k).map(|s| s.as_str()) != Some(v.as_str()) {
            return false;
        }
    }
    if let Some(tenant) = &query.tenant
        && crate::tenancy::owner_of(&span.attributes) != tenant.as_str()
    {
        return false;
    }
    for (k, needle) in &query.attributes_contains {
        match span.attributes.get(k) {
            Some(value) if value.contains(needle.as_str()) => {}
            _ => return false,
        }
    }
    for (k, pattern) in &query.attributes_regex {
        // An unparseable pattern matches nothing rather than everything. The
        // API layer rejects it up front with a message; this is the guard for
        // any path that skips that check.
        let Ok(re) = regex::Regex::new(pattern) else {
            return false;
        };
        match span.attributes.get(k) {
            Some(value) if re.is_match(value) => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{LogSeverity, MetricType, SpanKind};

    fn tier() -> (HotTier, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let tier = HotTier::open(dir.path().to_str().unwrap()).unwrap();
        (tier, dir)
    }

    fn span(id: &str, service: &str, status: SpanStatus, duration_ms: f64) -> Span {
        let now = chrono::Utc::now();
        Span {
            trace_id: format!("t-{id}"),
            span_id: format!("s-{id}"),
            parent_span_id: None,
            service: service.into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms,
            status,
            attributes: Default::default(),
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        }
    }

    fn log(body: &str) -> LogRecord {
        LogRecord {
            timestamp: chrono::Utc::now(),
            observed_timestamp: chrono::Utc::now(),
            severity: LogSeverity::Info,
            severity_text: "INFO".into(),
            body: body.into(),
            service: "api".into(),
            trace_id: None,
            span_id: None,
            attributes: Default::default(),
            body_sha256: None,
        }
    }

    fn metric(name: &str, value: f64) -> MetricPoint {
        MetricPoint {
            timestamp: chrono::Utc::now(),
            name: name.into(),
            value,
            metric_type: MetricType::Gauge,
            service: "api".into(),
            attributes: Default::default(),
            unit: String::new(),
            histogram: None,
        }
    }

    #[test]
    fn re_applying_a_batch_overwrites_rather_than_duplicates() {
        // The invariant the whole checkpointing WAL rests on: a crash replays
        // records that were already applied, so applying the same batch twice
        // has to leave the tier exactly as applying it once did. Before keys
        // were content-derived, the sequence number in them made this fail for
        // logs and metrics while quietly passing for spans.
        let (tier, _dir) = tier();
        let spans = vec![span("a", "api", SpanStatus::Ok, 1.0)];
        let logs = vec![log("hello"), log("world")];
        let metrics = vec![metric("cpu", 1.0), metric("mem", 2.0)];

        for _ in 0..3 {
            tier.insert_spans(&spans).unwrap();
            tier.insert_logs(&logs).unwrap();
            tier.insert_metrics(&metrics).unwrap();
        }

        let q = TraceQuery {
            limit: Some(100),
            ..Default::default()
        };
        assert_eq!(tier.query_traces(&q).unwrap().len(), 1);
        assert_eq!(
            tier.query_logs(&LogQuery {
                limit: Some(100),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            tier.query_metrics(&MetricQuery {
                limit: Some(100),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn two_distinct_records_sharing_a_timestamp_both_survive() {
        // Content-derived keys must not collapse different records. Logs that
        // differ only in body are the case that would silently lose data.
        let (tier, _dir) = tier();
        let ts = chrono::Utc::now();
        let mut a = log("first");
        let mut b = log("second");
        a.timestamp = ts;
        b.timestamp = ts;
        tier.insert_logs(&[a, b]).unwrap();
        assert_eq!(
            tier.query_logs(&LogQuery {
                limit: Some(100),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn a_service_query_scans_only_that_services_spans() {
        // The point of the secondary index: cost tracks matches, not rows.
        let (tier, _dir) = tier();
        let mut spans = vec![span("hit", "api", SpanStatus::Ok, 1.0)];
        for i in 0..200 {
            spans.push(span(&format!("miss{i}"), "worker", SpanStatus::Ok, 1.0));
        }
        tier.insert_spans(&spans).unwrap();

        let scan = tier
            .scan_spans(&TraceQuery {
                service: Some("api".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scan.index, SpanIndex::Service);
        assert_eq!(scan.spans.len(), 1);
        assert_eq!(
            scan.rows_scanned, 1,
            "the other service's 200 spans must never be visited"
        );
    }

    #[test]
    fn an_error_query_scans_only_error_spans() {
        let (tier, _dir) = tier();
        let mut spans = vec![span("bad", "api", SpanStatus::Error, 1.0)];
        for i in 0..200 {
            spans.push(span(&format!("ok{i}"), "api", SpanStatus::Ok, 1.0));
        }
        tier.insert_spans(&spans).unwrap();

        let scan = tier
            .scan_spans(&TraceQuery {
                status: Some("error".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scan.index, SpanIndex::Errors);
        assert_eq!(scan.spans.len(), 1);
        assert_eq!(scan.rows_scanned, 1);
    }

    #[test]
    fn a_service_scoped_error_query_narrows_on_errors_first() {
        // Both filters have an index. The error index is a fraction of the
        // tier while the service index is the tier divided by the number of
        // services, so errors is the narrower one — and the covering header
        // rejects the other services for free.
        let (tier, _dir) = tier();
        let mut spans = vec![span("bad", "api", SpanStatus::Error, 1.0)];
        for i in 0..200 {
            spans.push(span(&format!("ok{i}"), "api", SpanStatus::Ok, 1.0));
            spans.push(span(&format!("other{i}"), "worker", SpanStatus::Error, 1.0));
        }
        tier.insert_spans(&spans).unwrap();

        let scan = tier
            .scan_spans(&TraceQuery {
                service: Some("api".into()),
                status: Some("error".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scan.index, SpanIndex::Errors);
        assert_eq!(scan.spans.len(), 1);
        // 201 error spans exist; only api's is read back in full.
        assert_eq!(scan.rows_scanned, 201);
        assert_eq!(scan.rows_decoded, 1);
    }

    #[test]
    fn the_covering_header_rejects_candidates_without_reading_the_span() {
        // A duration filter has no index of its own, so every row is visited —
        // but the header means almost none of them are decoded.
        let (tier, _dir) = tier();
        let mut spans = vec![span("slow", "api", SpanStatus::Ok, 900.0)];
        for i in 0..200 {
            spans.push(span(&format!("fast{i}"), "api", SpanStatus::Ok, 1.0));
        }
        tier.insert_spans(&spans).unwrap();

        let scan = tier
            .scan_spans(&TraceQuery {
                min_duration_ms: Some(500.0),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scan.index, SpanIndex::Time);
        assert_eq!(scan.rows_scanned, 201, "no index narrows a duration filter");
        assert_eq!(
            scan.rows_decoded, 1,
            "but only the matching span is read back"
        );
    }

    #[test]
    fn a_time_cutoff_bounds_the_scan_instead_of_filtering_after_it() {
        let (tier, _dir) = tier();
        let mut old = span("old", "api", SpanStatus::Ok, 1.0);
        old.start_time = chrono::Utc::now() - chrono::Duration::hours(2);
        tier.insert_spans(&[old, span("new", "api", SpanStatus::Ok, 1.0)])
            .unwrap();

        let scan = tier
            .scan_spans(&TraceQuery {
                last_seconds: Some(600),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scan.spans.len(), 1);
        assert_eq!(
            scan.rows_scanned, 1,
            "the aged-out span is below the range bound, not filtered out after"
        );
    }

    #[test]
    fn eviction_clears_every_index_it_wrote() {
        // A leftover entry in a secondary index would resurrect an evicted span
        // as a phantom row, or make a scan chase a primary key that is gone.
        let (tier, _dir) = tier();
        let mut old = span("old", "api", SpanStatus::Error, 1.0);
        old.start_time = chrono::Utc::now() - chrono::Duration::hours(2);
        tier.insert_spans(&[old, span("new", "api", SpanStatus::Ok, 1.0)])
            .unwrap();

        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(1))
            .timestamp_nanos_opt()
            .unwrap();
        assert_eq!(tier.evict_spans_before(cutoff).unwrap().len(), 1);

        for query in [
            TraceQuery {
                limit: Some(100),
                ..Default::default()
            },
            TraceQuery {
                service: Some("api".into()),
                limit: Some(100),
                ..Default::default()
            },
            TraceQuery {
                status: Some("error".into()),
                limit: Some(100),
                ..Default::default()
            },
        ] {
            let scan = tier.scan_spans(&query).unwrap();
            assert!(
                scan.spans.iter().all(|s| s.trace_id == "t-new"),
                "evicted span still reachable via {}",
                scan.index.keyspace()
            );
        }
        // And the error index is empty now, so nothing is even visited.
        assert_eq!(
            tier.scan_spans(&TraceQuery {
                status: Some("error".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap()
            .rows_scanned,
            0
        );
    }

    #[test]
    fn list_services_aggregates_from_the_index_alone() {
        let (tier, _dir) = tier();
        tier.insert_spans(&[
            span("a", "api", SpanStatus::Ok, 10.0),
            span("b", "api", SpanStatus::Error, 30.0),
            span("c", "worker", SpanStatus::Ok, 5.0),
        ])
        .unwrap();

        let services = tier.list_services().unwrap();
        assert_eq!(services.len(), 2);
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert_eq!(api.span_count, 2);
        assert_eq!(api.trace_count, 2);
        assert_eq!(api.avg_duration_ms, 20.0);
        assert_eq!(api.error_rate, 0.5);
    }

    #[test]
    fn a_named_metric_query_scans_only_that_series() {
        let (tier, _dir) = tier();
        let mut points = vec![metric("http.latency", 42.0)];
        for i in 0..100 {
            points.push(metric(&format!("other.{i}"), i as f64));
        }
        tier.insert_metrics(&points).unwrap();

        let got = tier
            .query_metrics(&MetricQuery {
                name: Some("http.latency".into()),
                limit: Some(100),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, 42.0);
    }
}
