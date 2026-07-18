//! `TaelBackend` — the purpose-built storage engine (see
//! `docs/tael-backend-design.md`). Built incrementally behind the `Store`
//! trait so it ships opt-in alongside `DuckDbStore`.
//!
//! **Phase 3:** durable WAL write path + crash-gap replay.
//! **Phase 4 (this file):** an `fjall` LSM hot tier serves the core per-signal
//! reads (`query_traces`, `get_trace`, `list_services`, `query_logs`,
//! `query_metrics`). The legacy DuckDB projection is available behind the
//! `duckdb` Cargo feature, but the default backend is self-contained so the
//! default CLI install does not compile or link DuckDB.

mod cold;
mod hot;
mod wal;

use anyhow::{Result, bail};

#[cfg(feature = "duckdb")]
use super::DuckDbStore;
use super::Store;
use super::models::{
    Anomaly, AnomalyReport, CorrelateReport, ErrorOperation, LogQuery, LogRecord, LogSeverity,
    LogSummary, MetricPoint, MetricQuery, MetricSummary, ServiceInfo, ServiceSummary, Span,
    SpanStatus, SummaryReport, TraceComment, TraceQuery, TraceSummary,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::DynObjectBackend;
use super::SearchIndex;
use super::{CommentsStore, JsonlComments};
use cold::ColdTier;
use hot::HotTier;
use wal::WalLog;
pub use wal::{WalRecord, WalSink};

pub struct TaelBackend {
    /// LSM hot tier — serves recent reads.
    hot: HotTier,
    /// Parquet cold tier — aged spans rolled out of the hot tier.
    cold: ColdTier,
    /// Optional legacy projection used only when the `duckdb` feature is enabled.
    #[cfg(feature = "duckdb")]
    inner: DuckDbStore,
    /// Pluggable comments store (JSONL locally, Postgres in the cloud).
    comments: Box<dyn CommentsStore>,
    /// Full-text index over LLM payloads (shared with the ingest path).
    search: Arc<SearchIndex>,
    wal: WalLog,
}

impl TaelBackend {
    pub fn new(data_dir: &str) -> Result<Self> {
        Self::with_wal_key(data_dir, "tael-backend")
    }

    /// Like [`Self::new`] but with an explicit WAL namespace key — lets tests
    /// run isolated instances (the WAL key is process-global in walrus).
    pub fn with_wal_key(data_dir: &str, wal_key: &str) -> Result<Self> {
        Self::with_wal_key_and_sinks(data_dir, wal_key, Vec::new(), None)
    }

    /// Like [`Self::with_wal_key`] but with WAL replication sinks attached: this
    /// backend runs as a **leader** that ships every appended record to its
    /// standbys before acking the write (`docs/tael-server-scaling-ha.md` §5.1).
    /// `required_acks` is how many standbys must confirm before a write returns
    /// (`None` = all = fully synchronous; `Some(0)` = async best-effort). With
    /// no sinks the write path is unchanged. A standby on the receiving end
    /// applies shipped records via `Store::apply_framed_wal`.
    pub fn with_wal_key_and_sinks(
        data_dir: &str,
        wal_key: &str,
        sinks: Vec<Arc<dyn WalSink>>,
        required_acks: Option<usize>,
    ) -> Result<Self> {
        // Default components: local cold tier + local JSONL comments.
        let comments: Box<dyn CommentsStore> = Box::new(JsonlComments::open(data_dir)?);
        Self::with_components(data_dir, wal_key, sinks, required_acks, None, comments)
    }

    /// Full constructor used by the cloud deployment path: pass an optional
    /// cold-tier object backend (`None` = local filesystem) and an already-built
    /// comments store (JSONL or Postgres). The local constructors above forward
    /// here with the single-binary defaults, so their behavior is unchanged.
    pub fn with_components(
        data_dir: &str,
        wal_key: &str,
        sinks: Vec<Arc<dyn WalSink>>,
        required_acks: Option<usize>,
        cold_backend: Option<DynObjectBackend>,
        comments: Box<dyn CommentsStore>,
    ) -> Result<Self> {
        let hot = HotTier::open(data_dir)?;
        let cold = match cold_backend {
            Some(backend) => ColdTier::with_backend(backend)?,
            None => ColdTier::open(data_dir)?,
        };
        #[cfg(feature = "duckdb")]
        let inner = DuckDbStore::new(data_dir)?;
        let search = Arc::new(SearchIndex::open(data_dir)?);
        let mut wal = if sinks.is_empty() {
            WalLog::new_for_key(wal_key)?
        } else {
            WalLog::new_for_key_with_sinks(wal_key, sinks)?
        };
        if let Some(n) = required_acks {
            wal = wal.with_required_acks(n);
        }
        let backend = Self {
            hot,
            cold,
            #[cfg(feature = "duckdb")]
            inner,
            comments,
            search,
            wal,
        };
        backend.replay()?;
        Ok(backend)
    }

    /// The shared payload search index — handed to the ingest path so prompt/
    /// completion text is indexed at write time (the text isn't retained on the
    /// span itself, only its blob hashes).
    pub fn search_index(&self) -> Arc<SearchIndex> {
        Arc::clone(&self.search)
    }

    /// Roll spans older than `cutoff` out of the LSM hot tier into Parquet.
    /// Returns the number of spans compacted. Safe to call repeatedly.
    pub fn compact_spans(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let cutoff_ns = cutoff.timestamp_nanos_opt().unwrap_or(0);
        let evicted = self.hot.evict_spans_before(cutoff_ns)?;
        if evicted.is_empty() {
            return Ok(0);
        }
        // Write to cold first, then the hot eviction is already done; if we
        // crash between, the spans remain in the DuckDB projection and the WAL.
        self.cold.write_spans(&evicted)?;
        tracing::info!(
            spans = evicted.len(),
            "tael-backend: compacted spans to cold tier"
        );
        Ok(evicted.len())
    }

    /// Roll aged logs/metrics out of the hot tier into Parquet. Returns the
    /// total number of records compacted across both signals.
    pub fn compact_logs_metrics(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let cutoff_ns = cutoff.timestamp_nanos_opt().unwrap_or(0);
        let logs = self.hot.evict_logs_before(cutoff_ns)?;
        if !logs.is_empty() {
            self.cold.write_logs(&logs)?;
        }
        let metrics = self.hot.evict_metrics_before(cutoff_ns)?;
        if !metrics.is_empty() {
            self.cold.write_metrics(&metrics)?;
            // Downsample to 5m rollups alongside the raw cold write, so trends
            // survive once raw points are dropped by retention.
            self.cold.write_downsampled(&metrics)?;
        }
        let n = logs.len() + metrics.len();
        if n > 0 {
            tracing::info!(
                logs = logs.len(),
                metrics = metrics.len(),
                "tael-backend: compacted logs/metrics to cold tier"
            );
        }
        Ok(n)
    }

    /// Collect every blob hash still referenced by a live row — LLM prompt and
    /// completion hashes on spans, and `body_sha256` on logs — across hot and
    /// cold tiers. Drives blob GC (anything not here is unreferenced).
    pub fn collect_live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        use super::models::{LogQuery, TraceQuery};
        let mut live = std::collections::HashSet::new();
        // Spans (hot∪cold via the unioned read path), with a high limit.
        let spans = self.query_traces(&TraceQuery {
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        for s in spans {
            if let Some(llm) = s.llm {
                live.extend(llm.prompt_sha256);
                live.extend(llm.completion_sha256);
            }
        }
        let logs = self.query_logs(&LogQuery {
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        for l in logs {
            live.extend(l.body_sha256);
        }
        Ok(live)
    }

    /// Drop cold partitions (spans/logs/metrics) whose date is older than
    /// `keep`. Returns the total number of partitions removed. (Metadata GC;
    /// payload-blob GC runs separately in the maintenance task.)
    pub fn enforce_span_retention(&self, keep: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let cutoff_date = keep.format("%Y-%m-%d").to_string();
        let dropped = self.cold.drop_partitions_before(&cutoff_date)?;
        if dropped > 0 {
            tracing::info!(
                partitions = dropped,
                "tael-backend: dropped expired cold partitions"
            );
        }
        Ok(dropped)
    }

    /// Apply a batch to every projection. Used by both the live write path and
    /// WAL replay.
    fn apply_spans(&self, spans: &[Span]) -> Result<()> {
        self.hot.insert_spans(spans)?;
        #[cfg(feature = "duckdb")]
        {
            self.inner.insert_spans(spans)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            Ok(())
        }
    }
    fn apply_logs(&self, logs: &[LogRecord]) -> Result<()> {
        self.hot.insert_logs(logs)?;
        #[cfg(feature = "duckdb")]
        {
            self.inner.insert_logs(logs)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            Ok(())
        }
    }
    fn apply_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        self.hot.insert_metrics(metrics)?;
        #[cfg(feature = "duckdb")]
        {
            self.inner.insert_metrics(metrics)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            Ok(())
        }
    }

    /// Re-apply any WAL records left unconsumed by a crash, then advance past
    /// them (they are consumed by `drain`).
    fn replay(&self) -> Result<()> {
        let records = self.wal.drain()?;
        if records.is_empty() {
            return Ok(());
        }
        let mut spans = 0usize;
        let mut logs = 0usize;
        let mut metrics = 0usize;
        for record in records {
            match record {
                WalRecord::Spans(s) => {
                    spans += s.len();
                    self.apply_spans(&s)?;
                }
                WalRecord::Logs(l) => {
                    logs += l.len();
                    self.apply_logs(&l)?;
                }
                WalRecord::Metrics(m) => {
                    metrics += m.len();
                    self.apply_metrics(&m)?;
                }
            }
        }
        tracing::info!(spans, logs, metrics, "tael-backend: replayed WAL");
        Ok(())
    }
}

impl Store for TaelBackend {
    // ── Writes: WAL → apply (hot + projection) → mark applied ───────
    fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        self.wal.append_spans(spans)?;
        self.apply_spans(spans)?;
        self.wal.mark_applied()?;
        Ok(())
    }

    fn insert_logs(&self, logs: &[LogRecord]) -> Result<()> {
        self.wal.append_logs(logs)?;
        self.apply_logs(logs)?;
        self.wal.mark_applied()?;
        Ok(())
    }

    fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        self.wal.append_metrics(metrics)?;
        self.apply_metrics(metrics)?;
        self.wal.mark_applied()?;
        Ok(())
    }

    // ── Core reads: hot tier, unioned with the cold tier ────────────
    fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        // Full-text payload filter: restrict to traces whose LLM prompts/
        // completions match, then apply the rest of the query over those spans.
        if let Some(ref text) = query.text {
            let trace_ids = self.search.search_trace_ids(text, 1000)?;
            if trace_ids.is_empty() {
                return Ok(Vec::new());
            }
            let cutoff = query
                .last_seconds
                .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
            let limit = query.limit.unwrap_or(100) as usize;
            let mut matched: Vec<Span> = Vec::new();
            for tid in &trace_ids {
                for s in self.get_trace(tid)? {
                    if hot::span_matches(&s, query, cutoff) {
                        matched.push(s);
                    }
                }
            }
            matched.sort_by(|a, b| b.start_time.cmp(&a.start_time));
            matched.truncate(limit);
            return Ok(matched);
        }
        // Hot holds the most-recent spans; cold holds older ones. Newest-first
        // ordering means hot results lead; only dip into cold to fill the limit.
        let mut results = self.hot.query_traces(query)?;
        let limit = query.limit.unwrap_or(100) as usize;
        if results.len() < limit {
            let cutoff = query
                .last_seconds
                .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
            let mut cold: Vec<Span> = self
                .cold
                .all_spans()?
                .into_iter()
                .filter(|s| hot::span_matches(s, query, cutoff))
                .collect();
            cold.sort_by(|a, b| b.start_time.cmp(&a.start_time));
            for s in cold {
                if results.len() >= limit {
                    break;
                }
                results.push(s);
            }
        }
        Ok(results)
    }
    fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let mut spans = self.hot.get_trace(trace_id)?;
        let mut seen: std::collections::HashSet<String> =
            spans.iter().map(|s| s.span_id.clone()).collect();
        // Union with cold; dedup by span_id in case of transient overlap during
        // compaction.
        for s in self.cold.get_trace(trace_id)? {
            if seen.insert(s.span_id.clone()) {
                spans.push(s);
            }
        }
        spans.sort_by_key(|s| s.start_time);
        Ok(spans)
    }
    fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        self.hot.list_services()
    }
    fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let mut results = self.hot.query_logs(query)?;
        let limit = query.limit.unwrap_or(100) as usize;
        if results.len() < limit {
            let cutoff = query
                .last_seconds
                .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
            let mut cold: Vec<LogRecord> = self
                .cold
                .all_logs()?
                .into_iter()
                .filter(|l| hot::log_matches(l, query, cutoff))
                .collect();
            cold.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            for l in cold {
                if results.len() >= limit {
                    break;
                }
                results.push(l);
            }
        }
        Ok(results)
    }
    fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let mut results = self.hot.query_metrics(query)?;
        let limit = query.limit.unwrap_or(500) as usize;
        if results.len() < limit {
            let cutoff = query
                .last_seconds
                .map(|s| chrono::Utc::now() - chrono::Duration::seconds(s));
            let mut cold: Vec<MetricPoint> = self
                .cold
                .all_metrics()?
                .into_iter()
                .filter(|m| hot::metric_matches(m, query, cutoff))
                .collect();
            cold.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
            for m in cold {
                if results.len() >= limit {
                    break;
                }
                results.push(m);
            }
        }
        Ok(results)
    }

    // ── Comments & cross-signal analytics ───────────────────────────
    fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        #[cfg(feature = "duckdb")]
        {
            self.inner.add_comment(trace_id, span_id, author, body)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.comments.add(trace_id, span_id, author, body)
        }
    }
    fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        #[cfg(feature = "duckdb")]
        {
            self.inner.get_comments(trace_id)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.comments.get(trace_id)
        }
    }
    fn list_comments(&self, limit: usize) -> Result<Vec<TraceComment>> {
        // Cross-trace listing powers the reliability-loop scanners without the
        // SQL layer. The duckdb build keeps its SQL path in `query_sql`; here
        // the comments store enumerates directly.
        #[cfg(feature = "duckdb")]
        {
            let _ = limit;
            anyhow::bail!(
                "listing comments is served by the SQL layer on duckdb builds; \
                 query trace_comments via /api/v1/sql"
            )
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.comments.list_recent(limit)
        }
    }
    fn query_summary(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport> {
        #[cfg(feature = "duckdb")]
        {
            self.inner.query_summary(last_seconds, service)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.summary_native(last_seconds, service)
        }
    }
    fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        #[cfg(feature = "duckdb")]
        {
            self
                .inner
                .query_anomalies(current_seconds, baseline_seconds, service)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.anomalies_native(current_seconds, baseline_seconds, service)
        }
    }
    fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        #[cfg(feature = "duckdb")]
        {
            self.inner.query_correlate(trace_id)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            self.correlate_native(trace_id)
        }
    }
    fn query_sql(&self, sql: &str) -> Result<Vec<serde_json::Value>> {
        #[cfg(feature = "duckdb")]
        {
            self.inner.query_sql(sql)
        }
        #[cfg(not(feature = "duckdb"))]
        {
            let _ = sql;
            bail!("SQL queries require a build with the `duckdb` feature")
        }
    }

    fn flush(&self) -> Result<()> {
        // Graceful-shutdown flush: tighten the hot tier so a restart/standby
        // replays less WAL. WAL fsync already guarantees durability.
        self.hot.flush()
    }

    /// Standby entrypoint: durably accept a framed WAL record shipped from a
    /// leader and bring local state up to it. Mirrors the leader's write
    /// discipline (append → apply → consume) so the standby's WAL, hot tier, and
    /// projection stay byte-identical and itself replayable — the basis for
    /// promotion on leader loss (§5.1).
    fn apply_framed_wal(&self, framed: &[u8]) -> Result<()> {
        let record = WalRecord::decode(framed)?;
        self.wal.append_framed(framed)?;
        match &record {
            WalRecord::Spans(s) => self.apply_spans(s)?,
            WalRecord::Logs(l) => self.apply_logs(l)?,
            WalRecord::Metrics(m) => self.apply_metrics(m)?,
        }
        self.wal.mark_applied()?;
        Ok(())
    }
}

impl TaelBackend {
    fn summary_native(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport> {
        let spans = self.query_traces(&TraceQuery {
            service: service.map(str::to_string),
            last_seconds: Some(last_seconds),
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        let logs = self.query_logs(&LogQuery {
            service: service.map(str::to_string),
            last_seconds: Some(last_seconds),
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        let metrics = self.query_metrics(&MetricQuery {
            service: service.map(str::to_string),
            last_seconds: Some(last_seconds),
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        Ok(SummaryReport {
            window_seconds: last_seconds,
            service_filter: service.map(str::to_string),
            traces: trace_summary(&spans),
            top_services: service_summaries(&spans),
            top_error_operations: error_operations(&spans),
            logs: log_summary(&logs),
            metrics: metric_summary(&metrics),
        })
    }

    fn anomalies_native(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        let current = self.query_traces(&TraceQuery {
            service: service.map(str::to_string),
            last_seconds: Some(current_seconds),
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        let baseline = self.query_traces(&TraceQuery {
            service: service.map(str::to_string),
            last_seconds: Some(baseline_seconds),
            limit: Some(u32::MAX),
            ..Default::default()
        })?;
        Ok(AnomalyReport {
            current_seconds,
            baseline_seconds,
            service_filter: service.map(str::to_string),
            anomalies: anomalies(&current, &baseline),
        })
    }

    fn correlate_native(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        let spans = self.get_trace(trace_id)?;
        if spans.is_empty() {
            return Ok(None);
        }
        let start = spans
            .iter()
            .map(|s| s.start_time)
            .min()
            .unwrap_or_else(chrono::Utc::now);
        let end = spans
            .iter()
            .map(|s| s.end_time)
            .max()
            .unwrap_or_else(chrono::Utc::now);
        let services = spans
            .iter()
            .map(|s| s.service.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let logs = self.query_logs(&LogQuery {
            trace_id: Some(trace_id.to_string()),
            limit: Some(500),
            ..Default::default()
        })?;
        let service = spans.first().map(|s| s.service.clone());
        let metrics = self.query_metrics(&MetricQuery {
            service,
            limit: Some(500),
            ..Default::default()
        })?;
        Ok(Some(CorrelateReport {
            trace_id: trace_id.to_string(),
            span_count: spans.len(),
            services,
            start_time: start.to_rfc3339(),
            end_time: end.to_rfc3339(),
            duration_ms: (end - start).num_microseconds().unwrap_or(0) as f64 / 1000.0,
            error_count: spans
                .iter()
                .filter(|s| matches!(s.status, SpanStatus::Error))
                .count() as i64,
            logs,
            metrics,
        }))
    }
}

fn trace_summary(spans: &[Span]) -> TraceSummary {
    let mut durations: Vec<f64> = spans.iter().map(|s| s.duration_ms).collect();
    durations.sort_by(|a, b| a.total_cmp(b));
    let trace_count = spans.iter().map(|s| &s.trace_id).collect::<HashSet<_>>().len() as i64;
    let error_count = spans
        .iter()
        .filter(|s| matches!(s.status, SpanStatus::Error))
        .count() as i64;
    TraceSummary {
        span_count: spans.len() as i64,
        trace_count,
        error_count,
        error_rate: ratio(error_count, spans.len() as i64),
        avg_ms: if durations.is_empty() {
            0.0
        } else {
            durations.iter().sum::<f64>() / durations.len() as f64
        },
        max_ms: durations.last().copied().unwrap_or(0.0),
        p50_ms: percentile(&durations, 0.50),
        p95_ms: percentile(&durations, 0.95),
        p99_ms: percentile(&durations, 0.99),
    }
}

fn service_summaries(spans: &[Span]) -> Vec<ServiceSummary> {
    let mut by_service: HashMap<String, Vec<Span>> = HashMap::new();
    for span in spans {
        by_service
            .entry(span.service.clone())
            .or_default()
            .push(span.clone());
    }
    let mut rows: Vec<ServiceSummary> = by_service
        .into_iter()
        .map(|(service, spans)| {
            let summary = trace_summary(&spans);
            ServiceSummary {
                service,
                span_count: summary.span_count,
                error_rate: summary.error_rate,
                p95_ms: summary.p95_ms,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.span_count.cmp(&a.span_count));
    rows.truncate(10);
    rows
}

fn error_operations(spans: &[Span]) -> Vec<ErrorOperation> {
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    for span in spans
        .iter()
        .filter(|s| matches!(s.status, SpanStatus::Error))
    {
        *counts
            .entry((span.service.clone(), span.operation.clone()))
            .or_default() += 1;
    }
    let mut rows: Vec<ErrorOperation> = counts
        .into_iter()
        .map(|((service, operation), error_count)| ErrorOperation {
            service,
            operation,
            error_count,
        })
        .collect();
    rows.sort_by(|a, b| b.error_count.cmp(&a.error_count));
    rows.truncate(10);
    rows
}

fn log_summary(logs: &[LogRecord]) -> LogSummary {
    let mut summary = LogSummary {
        total: logs.len() as i64,
        ..Default::default()
    };
    for log in logs {
        match log.severity {
            LogSeverity::Error | LogSeverity::Fatal => summary.error += 1,
            LogSeverity::Warn => summary.warn += 1,
            LogSeverity::Info => summary.info += 1,
            LogSeverity::Debug => summary.debug += 1,
            _ => {}
        }
    }
    summary
}

fn metric_summary(metrics: &[MetricPoint]) -> MetricSummary {
    MetricSummary {
        point_count: metrics.len() as i64,
        unique_names: metrics.iter().map(|m| &m.name).collect::<HashSet<_>>().len() as i64,
    }
}

fn anomalies(current: &[Span], baseline: &[Span]) -> Vec<Anomaly> {
    let cur = service_summaries(current);
    let base = service_summaries(baseline);
    let base_map: HashMap<String, ServiceSummary> =
        base.into_iter().map(|s| (s.service.clone(), s)).collect();
    let mut rows = Vec::new();
    for c in cur {
        let Some(b) = base_map.get(&c.service) else {
            continue;
        };
        if c.error_rate > b.error_rate + 0.05 {
            rows.push(Anomaly {
                service: c.service.clone(),
                kind: "error_rate".to_string(),
                severity: if c.error_rate > b.error_rate + 0.20 {
                    "high".to_string()
                } else {
                    "medium".to_string()
                },
                current: c.error_rate,
                baseline: b.error_rate,
                delta: c.error_rate - b.error_rate,
                description: format!(
                    "{} error rate increased from {:.1}% to {:.1}%",
                    c.service,
                    b.error_rate * 100.0,
                    c.error_rate * 100.0
                ),
            });
        }
        if c.p95_ms > b.p95_ms * 1.5 && c.p95_ms - b.p95_ms > 25.0 {
            rows.push(Anomaly {
                service: c.service.clone(),
                kind: "p95_latency".to_string(),
                severity: if c.p95_ms > b.p95_ms * 2.0 {
                    "high".to_string()
                } else {
                    "medium".to_string()
                },
                current: c.p95_ms,
                baseline: b.p95_ms,
                delta: c.p95_ms - b.p95_ms,
                description: format!(
                    "{} p95 latency increased from {:.1}ms to {:.1}ms",
                    c.service, b.p95_ms, c.p95_ms
                ),
            });
        }
    }
    rows
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn ratio(numerator: i64, denominator: i64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{SpanKind, SpanStatus};
    use chrono::Utc;
    use std::collections::HashMap;

    /// Removes a walrus namespace dir (`wal_files/<key>`) on drop.
    struct NsGuard(String);
    impl Drop for NsGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.0));
        }
    }

    fn backend() -> (TaelBackend, tempfile::TempDir, NsGuard) {
        let dir = tempfile::tempdir().unwrap();
        let key = format!("tael-test-backend-{}", uuid::Uuid::new_v4());
        let b = TaelBackend::with_wal_key(dir.path().to_str().unwrap(), &key).unwrap();
        (b, dir, NsGuard(key))
    }

    fn span(trace: &str, sid: &str, svc: &str, dur: f64, status: SpanStatus) -> Span {
        let now = Utc::now();
        Span {
            trace_id: trace.into(),
            span_id: sid.into(),
            parent_span_id: None,
            service: svc.into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms: dur,
            status,
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    #[test]
    fn get_trace_reconstructs_span_tree_from_hot_tier() {
        let (b, _d, _g) = backend();
        b.insert_spans(&[
            span("t1", "s1", "api", 10.0, SpanStatus::Ok),
            span("t1", "s2", "db", 20.0, SpanStatus::Ok),
            span("t2", "s3", "api", 5.0, SpanStatus::Error),
        ])
        .unwrap();

        let trace = b.get_trace("t1").unwrap();
        assert_eq!(trace.len(), 2);
        assert!(trace.iter().all(|s| s.trace_id == "t1"));
        assert_eq!(b.get_trace("t2").unwrap().len(), 1);
        assert!(b.get_trace("missing").unwrap().is_empty());
    }

    #[test]
    #[cfg(feature = "duckdb")]
    fn query_traces_filters_match_duckdb() {
        let (b, _d, _g) = backend();
        let spans = vec![
            span("t1", "s1", "api", 10.0, SpanStatus::Ok),
            span("t2", "s2", "db", 600.0, SpanStatus::Error),
            span("t3", "s3", "api", 50.0, SpanStatus::Error),
        ];
        b.insert_spans(&spans).unwrap();

        // Independent DuckDB with identical data = the parity oracle.
        let oracle_dir = tempfile::tempdir().unwrap();
        let oracle = DuckDbStore::new(oracle_dir.path().to_str().unwrap()).unwrap();
        oracle.insert_spans(&spans).unwrap();

        let queries = [
            TraceQuery {
                service: Some("api".into()),
                ..Default::default()
            },
            TraceQuery {
                status: Some("error".into()),
                ..Default::default()
            },
            TraceQuery {
                min_duration_ms: Some(100.0),
                ..Default::default()
            },
            TraceQuery::default(),
        ];
        for q in &queries {
            let mut hot: Vec<String> = b
                .query_traces(q)
                .unwrap()
                .into_iter()
                .map(|s| s.span_id)
                .collect();
            let mut duck: Vec<String> = oracle
                .query_traces(q)
                .unwrap()
                .into_iter()
                .map(|s| s.span_id)
                .collect();
            hot.sort();
            duck.sort();
            assert_eq!(hot, duck, "hot tier and DuckDB disagree for {q:?}");
        }
    }

    #[test]
    fn list_services_aggregates_from_hot_tier() {
        let (b, _d, _g) = backend();
        b.insert_spans(&[
            span("t1", "s1", "api", 10.0, SpanStatus::Ok),
            span("t1", "s2", "api", 30.0, SpanStatus::Error),
            span("t2", "s3", "db", 20.0, SpanStatus::Ok),
        ])
        .unwrap();

        let services = b.list_services().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert_eq!(api.span_count, 2);
        assert_eq!(api.trace_count, 1);
        assert_eq!(api.avg_duration_ms, 20.0);
        assert!((api.error_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn standby_rebuilds_identical_state_from_shipped_wal() {
        use crate::storage::models::{LogRecord, LogSeverity};

        // The standby: a normal backend that is never written to directly.
        let (standby, _sd, _sg) = backend();
        let standby = Arc::new(standby);

        // A WAL sink that ships each framed record into the standby — the
        // in-process stand-in for the (deferred) network transport.
        struct ReplicaSink(Arc<TaelBackend>);
        impl WalSink for ReplicaSink {
            fn append_framed(&self, framed: &[u8]) -> Result<()> {
                self.0.apply_framed_wal(framed)
            }
        }

        // The leader, with the standby attached as a replication sink.
        let leader_dir = tempfile::tempdir().unwrap();
        let leader_key = format!("tael-test-leader-{}", uuid::Uuid::new_v4());
        let _lg = NsGuard(leader_key.clone());
        let leader = TaelBackend::with_wal_key_and_sinks(
            leader_dir.path().to_str().unwrap(),
            &leader_key,
            vec![Arc::new(ReplicaSink(Arc::clone(&standby)))],
            None, // synchronous: require all (one) standbys
        )
        .unwrap();

        // Write a mix of signals to the leader only.
        leader
            .insert_spans(&[
                span("t1", "s1", "api", 10.0, SpanStatus::Ok),
                span("t1", "s2", "db", 20.0, SpanStatus::Ok),
                span("t2", "s3", "api", 5.0, SpanStatus::Error),
            ])
            .unwrap();
        leader
            .insert_logs(&[LogRecord {
                timestamp: Utc::now(),
                observed_timestamp: Utc::now(),
                trace_id: Some("t1".into()),
                span_id: None,
                severity: LogSeverity::Error,
                severity_text: "ERROR".into(),
                body: "boom".into(),
                service: "api".into(),
                attributes: HashMap::new(),
                body_sha256: None,
            }])
            .unwrap();

        // The standby reconstructed identical state purely from the shipped WAL.
        assert_eq!(standby.get_trace("t1").unwrap().len(), 2);
        assert_eq!(standby.get_trace("t2").unwrap().len(), 1);
        let leader_traces = leader.query_traces(&TraceQuery::default()).unwrap();
        let standby_traces = standby.query_traces(&TraceQuery::default()).unwrap();
        assert_eq!(leader_traces.len(), standby_traces.len());
        assert_eq!(standby_traces.len(), 3);
    }

    #[test]
    fn logs_and_metrics_round_trip_through_hot_tier() {
        use crate::storage::models::{LogRecord, LogSeverity, MetricPoint, MetricType};
        let (b, _d, _g) = backend();

        b.insert_logs(&[LogRecord {
            timestamp: Utc::now(),
            observed_timestamp: Utc::now(),
            trace_id: Some("t1".into()),
            span_id: None,
            severity: LogSeverity::Error,
            severity_text: "ERROR".into(),
            body: "connection refused".into(),
            service: "api".into(),
            attributes: HashMap::new(),
            body_sha256: None,
        }])
        .unwrap();
        let logs = b
            .query_logs(&LogQuery {
                severity: Some("error".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].body, "connection refused");

        b.insert_metrics(&[MetricPoint {
            timestamp: Utc::now(),
            service: "api".into(),
            name: "http_requests_total".into(),
            metric_type: MetricType::Sum,
            value: 42.0,
            unit: "1".into(),
            attributes: HashMap::new(),
        }])
        .unwrap();
        let metrics = b
            .query_metrics(&MetricQuery {
                name: Some("http_requests_total".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].value, 42.0);
    }

    #[test]
    fn compaction_moves_spans_to_cold_and_reads_still_union() {
        let (b, _d, _g) = backend();
        b.insert_spans(&[
            span("t1", "s1", "api", 10.0, SpanStatus::Ok),
            span("t1", "s2", "db", 20.0, SpanStatus::Ok),
            span("t2", "s3", "api", 5.0, SpanStatus::Error),
        ])
        .unwrap();

        // Compact everything (cutoff in the future) → all spans roll to cold.
        let moved = b
            .compact_spans(Utc::now() + chrono::Duration::seconds(60))
            .unwrap();
        assert_eq!(moved, 3);
        // Hot tier is now empty...
        assert!(b.hot.get_trace("t1").unwrap().is_empty());
        // ...but the unioned reads still see everything.
        assert_eq!(b.get_trace("t1").unwrap().len(), 2);
        let all = b.query_traces(&TraceQuery::default()).unwrap();
        assert_eq!(all.len(), 3);
        let errors = b
            .query_traces(&TraceQuery {
                status: Some("error".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].span_id, "s3");

        // Re-compaction is a no-op (nothing left in hot).
        assert_eq!(b.compact_spans(Utc::now()).unwrap(), 0);
    }

    #[test]
    fn logs_metrics_compact_to_cold_and_union_reads() {
        use crate::storage::models::{LogRecord, LogSeverity, MetricPoint, MetricType};
        let (b, _d, _g) = backend();
        b.insert_logs(&[LogRecord {
            timestamp: Utc::now(),
            observed_timestamp: Utc::now(),
            trace_id: Some("t1".into()),
            span_id: None,
            severity: LogSeverity::Error,
            severity_text: "ERROR".into(),
            body: "boom".into(),
            service: "api".into(),
            attributes: HashMap::new(),
            body_sha256: None,
        }])
        .unwrap();
        b.insert_metrics(&[MetricPoint {
            timestamp: Utc::now(),
            service: "api".into(),
            name: "rps".into(),
            metric_type: MetricType::Sum,
            value: 7.0,
            unit: "1".into(),
            attributes: HashMap::new(),
        }])
        .unwrap();

        let moved = b
            .compact_logs_metrics(Utc::now() + chrono::Duration::seconds(60))
            .unwrap();
        assert_eq!(moved, 2);

        // Served from cold via union after the hot tier emptied.
        let logs = b.query_logs(&LogQuery::default()).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].body, "boom");
        let metrics = b.query_metrics(&MetricQuery::default()).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].value, 7.0);
    }

    #[test]
    fn full_text_search_filters_traces_by_payload() {
        let (b, _d, _g) = backend();
        b.insert_spans(&[
            span("t1", "s1", "llm-proxy", 100.0, SpanStatus::Ok),
            span("t2", "s2", "llm-proxy", 100.0, SpanStatus::Ok),
        ])
        .unwrap();
        // Index payload text the way ingestion would.
        let idx = b.search_index();
        idx.index_span("t1", "s1", "summarize the rate limit policy")
            .unwrap();
        idx.index_span("t2", "s2", "translate to French").unwrap();
        idx.commit().unwrap();

        let hits = b
            .query_traces(&TraceQuery {
                text: Some("rate limit".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].trace_id, "t1");

        // Non-matching query → empty.
        let none = b
            .query_traces(&TraceQuery {
                text: Some("quantum".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn survives_reopen_via_persistent_hot_tier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let key = format!("tael-test-reopen-{}", uuid::Uuid::new_v4());
        {
            let b = TaelBackend::with_wal_key(path, &key).unwrap();
            b.insert_spans(&[span("t1", "s1", "api", 10.0, SpanStatus::Ok)])
                .unwrap();
        }
        // Reopen: data persists (hot tier + DuckDB are durable).
        let b2 = TaelBackend::with_wal_key(path, &key).unwrap();
        assert_eq!(b2.get_trace("t1").unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(format!("wal_files/{key}"));
    }
}
