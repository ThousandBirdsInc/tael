//! Parquet cold tier for `TaelBackend` (spans).
//!
//! Aged spans roll out of the LSM hot tier into immutable Parquet objects,
//! **sorted by `trace_id`** within `spans/date=YYYY-MM-DD/hour=HH/` partitions
//! so a span-tree read is one contiguous scan (see
//! `docs/tael-backend-design.md` → "Cold tier"). Reads push their predicate
//! down three levels before a row is ever materialized (the design's Phase 6
//! pushdown, now complete):
//!
//! 1. **Partition pruning** — time-bounded reads drop whole `date=`/`hour=`
//!    partitions from the listing before fetching anything
//!    ([`partition_may_contain_since`]).
//! 2. **Row-group pruning** — inside a surviving object, row groups whose
//!    column statistics cannot satisfy the predicate are never decoded
//!    ([`row_group_may_match`]). Spans are sorted by `trace_id` and logs/
//!    metrics by `(service|name, ts)` within each object precisely so these
//!    statistics are tight.
//! 3. **Row filtering** — surviving row groups are decoded through a Parquet
//!    [`RowFilter`] built from the predicate, so non-matching rows are dropped
//!    at the decoder instead of being turned into `Span`s and filtered later.
//!
//! Scans stream: objects are visited one partition at a time, newest first,
//! and a visitor can stop at any partition boundary (how a limit-bounded query
//! avoids reading history it will discard). Every scan reports what it did in
//! a [`ColdScanStats`], which `--explain` surfaces.
//!
//! Objects live on the shared [`ObjectBackend`](crate::storage::ObjectBackend):
//! a local directory by default (`<data_dir>/cold`, overridable via
//! `TAEL_COLD_DIR`), or a GCS bucket under the `cloud` feature. Parquet is
//! built fully in memory and written with a single atomic `put`; reads `get`
//! the object and decode from `Bytes`. The `date=…/hour=…` layout is a valid
//! object-store key prefix.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Array, ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, TimeZone, Utc};
use parquet::arrow::arrow_reader::{ArrowPredicateFn, ParquetRecordBatchReaderBuilder, RowFilter};
use parquet::arrow::{ArrowWriter, ProjectionMask};
use parquet::file::metadata::RowGroupMetaData;
use parquet::file::properties::WriterProperties;
use parquet::file::statistics::Statistics;

use crate::storage::models::{
    LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanKind, SpanStatus,
};
use crate::storage::{DynObjectBackend, FsBackend};

/// A 5-minute downsampled metric aggregate (one series, one bucket).
#[derive(Debug, Clone, PartialEq)]
pub struct RollupPoint {
    pub bucket_start: DateTime<Utc>,
    pub service: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub count: i64,
}

impl RollupPoint {
    pub fn avg(&self) -> f64 {
        if self.count > 0 {
            self.sum / self.count as f64
        } else {
            0.0
        }
    }
}

const ROLLUP_BUCKET_SECS: i64 = 300; // 5 minutes

// Per-signal key prefixes within the cold object namespace.
const SPANS: &str = "spans";
const LOGS: &str = "logs";
const METRICS: &str = "metrics";
const METRICS_5M: &str = "metrics_5m";

/// Rows per Parquet row group. Small enough that row-group pruning has real
/// granularity inside an hour partition (a `get_trace` against trace-sorted
/// objects skips every group whose `trace_id` range excludes the target),
/// large enough that per-group metadata stays negligible.
const ROW_GROUP_ROWS: usize = 16 * 1024;

/// A predicate pushed down into the cold tier's Parquet reads.
///
/// Every field is optional; an empty predicate is a full scan. Fields map to
/// the columns each signal actually sorts or partitions by, which is what
/// makes the row-group statistics selective rather than decorative:
/// spans sort by `trace_id`, logs by `(service, ts)`, metrics by `(name, ts)`,
/// and everything partitions by time.
#[derive(Debug, Default, Clone)]
pub struct ColdPredicate {
    /// Keep rows with a timestamp at or after this instant. Also prunes whole
    /// partitions from the listing (level 1) before any object is fetched.
    pub since: Option<DateTime<Utc>>,
    /// Exact `trace_id` (spans and logs).
    pub trace_id: Option<String>,
    /// Exact `service` name.
    pub service: Option<String>,
    /// Exact metric `name` (metrics and rollups).
    pub name: Option<String>,
    /// Error spans only (`status == "error"`).
    pub error_only: bool,
}

impl ColdPredicate {
    /// A predicate that keeps only rows at or after `since` (`None` keeps all).
    pub fn since(since: Option<DateTime<Utc>>) -> Self {
        Self {
            since,
            ..Self::default()
        }
    }

    fn since_ns(&self) -> Option<i64> {
        self.since.and_then(|s| s.timestamp_nanos_opt())
    }
}

/// What a cold scan did, for `--explain` and the pruning tests.
///
/// The three prune counters correspond to the three pushdown levels described
/// in the module docs; `rows_decoded` is what survived all of them and was
/// actually materialized.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ColdScanStats {
    /// Objects under the signal's prefix.
    pub objects_listed: usize,
    /// Objects skipped because their `date=`/`hour=` partition cannot contain
    /// matching rows (never fetched).
    pub objects_pruned_by_partition: usize,
    /// Objects fetched and opened.
    pub objects_read: usize,
    /// Objects whose partitions were never reached because the visitor
    /// stopped early (a filled limit).
    pub objects_skipped_by_early_exit: usize,
    /// Row groups across all opened objects.
    pub row_groups_total: usize,
    /// Row groups skipped because their column statistics cannot satisfy the
    /// predicate (never decoded).
    pub row_groups_pruned: usize,
    /// Rows that survived the decoder-level row filter and were materialized.
    pub rows_decoded: usize,
}

/// Which columns of a signal's schema the pushdown predicate binds to.
/// A `None` means the signal has no such column and that predicate field is
/// simply not applied at this level (the caller's residual filter still runs).
struct PushdownColumns {
    ts: &'static str,
    trace_id: Option<&'static str>,
    service: Option<&'static str>,
    name: Option<&'static str>,
    status: Option<&'static str>,
}

const SPAN_COLUMNS: PushdownColumns = PushdownColumns {
    ts: "start_ns",
    trace_id: Some("trace_id"),
    service: Some("service"),
    name: None,
    status: Some("status"),
};
const LOG_COLUMNS: PushdownColumns = PushdownColumns {
    ts: "ts_ns",
    trace_id: Some("trace_id"),
    service: Some("service"),
    name: None,
    status: None,
};
const METRIC_COLUMNS: PushdownColumns = PushdownColumns {
    ts: "ts_ns",
    trace_id: None,
    service: Some("service"),
    name: Some("name"),
    status: None,
};
const ROLLUP_COLUMNS: PushdownColumns = PushdownColumns {
    ts: "bucket_ns",
    trace_id: None,
    service: Some("service"),
    name: Some("name"),
    status: None,
};

pub struct ColdTier {
    backend: DynObjectBackend,
    /// Rows per written row group. Constant in production; tests shrink it to
    /// exercise multi-group pruning without writing tens of thousands of rows.
    row_group_rows: usize,
}

impl ColdTier {
    pub fn open(data_dir: &str) -> Result<Self> {
        // The cold tier can live on a different mount than the hot tier — set
        // `TAEL_COLD_DIR` to a separate path to keep aged Parquet off the hot
        // disk. For native object storage (GCS), construct with
        // [`Self::with_backend`]; this default path is local filesystem.
        let base = match std::env::var("TAEL_COLD_DIR") {
            Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => Path::new(data_dir).join("cold"),
        };
        Self::with_backend(Arc::new(FsBackend::new(base)?))
    }

    /// Open the cold tier on an arbitrary object backend (e.g. GCS). The key
    /// layout is identical, so the backend is a transparent swap.
    pub fn with_backend(backend: DynObjectBackend) -> Result<Self> {
        Ok(Self {
            backend,
            row_group_rows: ROW_GROUP_ROWS,
        })
    }

    /// Shrink the row-group size so tests can produce multi-group objects
    /// from small batches.
    #[cfg(test)]
    pub(crate) fn set_row_group_rows(&mut self, rows: usize) {
        self.row_group_rows = rows.max(1);
    }

    /// Write a batch of spans to Parquet, grouped into `date=…/hour=…`
    /// partitions and sorted by `trace_id` within each object.
    pub fn write_spans(&self, spans: &[Span]) -> Result<()> {
        use std::collections::BTreeMap;
        // Group by (date, hour) of start_time.
        let mut by_partition: BTreeMap<(String, String), Vec<&Span>> = BTreeMap::new();
        for s in spans {
            let dt = s.start_time;
            let date = dt.format("%Y-%m-%d").to_string();
            let hour = dt.format("%H").to_string();
            by_partition.entry((date, hour)).or_default().push(s);
        }

        for ((date, hour), mut group) in by_partition {
            group.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
            let batch = spans_to_batch(&group)?;
            self.put_parquet(&partition_key(SPANS, &date, &hour, "spans"), &batch)?;
        }
        Ok(())
    }

    /// Drop whole `date=YYYY-MM-DD` partitions older than `cutoff_date`
    /// (exclusive). Returns the number of distinct partitions removed.
    /// `cutoff_date` is the oldest date to keep, formatted `YYYY-MM-DD`.
    ///
    /// Object stores have no atomic directory unlink, so this lists the keys
    /// under each signal and deletes the expired ones individually (a crash
    /// mid-drop leaves a harmless partial partition that a re-run finishes).
    pub fn drop_partitions_before(&self, cutoff_date: &str) -> Result<usize> {
        self.drop_partitions_per_signal(&[
            (SPANS, cutoff_date),
            (LOGS, cutoff_date),
            (METRICS, cutoff_date),
        ])
    }

    /// Drop expired partitions with a **per-signal** cutoff date.
    ///
    /// Signals age at different rates: span payloads are large and investigated
    /// within days, while 5-minute metric rollups are tiny and wanted for a
    /// year. A single shared cutoff forces the shortest useful window on
    /// everything, so each root gets its own.
    pub fn drop_partitions_per_signal(&self, cutoffs: &[(&str, &str)]) -> Result<usize> {
        use std::collections::HashSet;
        let mut dropped: HashSet<String> = HashSet::new();
        for (root, cutoff_date) in cutoffs {
            for key in self.backend.list(root)? {
                // Keys look like `spans/date=YYYY-MM-DD/hour=HH/…`; the date is
                // zero-padded fixed-width, so a lexicographic compare is correct.
                if let Some(date) = parse_date_segment(&key)
                    && date < *cutoff_date
                {
                    self.backend.delete(&key)?;
                    dropped.insert(format!("{root}/date={date}"));
                }
            }
        }
        Ok(dropped.len())
    }

    /// The object-namespace roots, so callers can name signals without
    /// duplicating the key strings.
    pub const SPANS_ROOT: &'static str = SPANS;
    pub const LOGS_ROOT: &'static str = LOGS;
    pub const METRICS_ROOT: &'static str = METRICS;
    pub const METRICS_5M_ROOT: &'static str = METRICS_5M;

    /// Read all spans for a trace from the cold tier.
    ///
    /// Pushed down: objects sort spans by `trace_id`, so row groups whose
    /// `trace_id` statistics exclude the target are skipped without decoding.
    pub fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let pred = ColdPredicate {
            trace_id: Some(trace_id.to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();
        self.scan_spans(&pred, &mut |spans| {
            out.extend(spans);
            true
        })?;
        Ok(out)
    }

    /// Cold spans from partitions that can hold rows at or after `since`.
    /// `None` reads everything (the hot∪cold union filters afterward).
    #[cfg(test)]
    pub fn spans_since(&self, since: Option<DateTime<Utc>>) -> Result<Vec<Span>> {
        self.spans_matching(&ColdPredicate::since(since))
            .map(|r| r.0)
    }

    /// All spans matching `pred`, with the scan's stats.
    pub fn spans_matching(&self, pred: &ColdPredicate) -> Result<(Vec<Span>, ColdScanStats)> {
        let mut out = Vec::new();
        let stats = self.scan_spans(pred, &mut |spans| {
            out.extend(spans);
            true
        })?;
        Ok((out, stats))
    }

    /// Stream spans matching `pred` one time partition at a time, **newest
    /// partition first**. `visit` receives every matching span of one
    /// `date=`/`hour=` partition and returns whether to continue; returning
    /// `false` stops the scan without touching older partitions.
    ///
    /// The partition boundary is the correct early-exit grain: objects within
    /// one partition are unordered relative to each other, but every row in an
    /// older partition is strictly older than every row in a newer one — so a
    /// caller that has filled its limit can stop at a boundary knowing nothing
    /// newer remains unread.
    pub fn scan_spans(
        &self,
        pred: &ColdPredicate,
        visit: &mut dyn FnMut(Vec<Span>) -> bool,
    ) -> Result<ColdScanStats> {
        self.scan_partitions(SPANS, pred, &SPAN_COLUMNS, &mut |batches| {
            let mut spans = Vec::new();
            for batch in batches {
                spans.extend(batch_to_spans(batch)?);
            }
            Ok(visit(spans))
        })
    }

    // ── Logs ────────────────────────────────────────────────────────

    /// Write aged logs to Parquet, partitioned by date/hour, sorted by
    /// `(service, ts)`.
    pub fn write_logs(&self, logs: &[LogRecord]) -> Result<()> {
        self.write_partitioned(
            LOGS,
            "logs",
            logs,
            |l| l.timestamp,
            |group| {
                group.sort_by(|a, b| {
                    (a.service.as_str(), a.timestamp).cmp(&(b.service.as_str(), b.timestamp))
                });
                logs_to_batch(group)
            },
        )
    }

    /// Cold logs from partitions that can hold rows at or after `since`.
    #[cfg(test)]
    pub fn logs_since(&self, since: Option<DateTime<Utc>>) -> Result<Vec<LogRecord>> {
        let mut out = Vec::new();
        self.scan_logs(&ColdPredicate::since(since), &mut |logs| {
            out.extend(logs);
            true
        })?;
        Ok(out)
    }

    /// Stream logs matching `pred`, newest partition first; same early-exit
    /// contract as [`Self::scan_spans`].
    pub fn scan_logs(
        &self,
        pred: &ColdPredicate,
        visit: &mut dyn FnMut(Vec<LogRecord>) -> bool,
    ) -> Result<ColdScanStats> {
        self.scan_partitions(LOGS, pred, &LOG_COLUMNS, &mut |batches| {
            let mut logs = Vec::new();
            for batch in batches {
                logs.extend(batch_to_logs(batch)?);
            }
            Ok(visit(logs))
        })
    }

    // ── Metrics ─────────────────────────────────────────────────────

    /// Write aged metric points to Parquet, partitioned by date/hour, sorted by
    /// `(name, ts)`.
    pub fn write_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        self.write_partitioned(
            METRICS,
            "metrics",
            metrics,
            |m| m.timestamp,
            |group| {
                group.sort_by(|a, b| {
                    (a.name.as_str(), a.timestamp).cmp(&(b.name.as_str(), b.timestamp))
                });
                metrics_to_batch(group)
            },
        )
    }

    /// Cold metric points from partitions that can hold rows at or after
    /// `since`.
    #[cfg(test)]
    pub fn metrics_since(&self, since: Option<DateTime<Utc>>) -> Result<Vec<MetricPoint>> {
        let mut out = Vec::new();
        self.scan_metrics(&ColdPredicate::since(since), &mut |points| {
            out.extend(points);
            true
        })?;
        Ok(out)
    }

    /// Stream metric points matching `pred`, newest partition first; same
    /// early-exit contract as [`Self::scan_spans`].
    pub fn scan_metrics(
        &self,
        pred: &ColdPredicate,
        visit: &mut dyn FnMut(Vec<MetricPoint>) -> bool,
    ) -> Result<ColdScanStats> {
        self.scan_partitions(METRICS, pred, &METRIC_COLUMNS, &mut |batches| {
            let mut points = Vec::new();
            for batch in batches {
                points.extend(batch_to_metrics(batch)?);
            }
            Ok(visit(points))
        })
    }

    // ── Metric downsampling (5m rollups) ────────────────────────────

    /// Aggregate raw points into 5-minute (`service`, `name`) buckets and write
    /// them to `metrics_5m/date=…/` (day-partitioned — rollups are sparse and
    /// long-lived). Idempotent per call; buckets across calls are not merged
    /// (acceptable: a series' raw points are downsampled once at compaction).
    pub fn write_downsampled(&self, points: &[MetricPoint]) -> Result<()> {
        let rollups = downsample(points);
        if rollups.is_empty() {
            return Ok(());
        }
        use std::collections::BTreeMap;
        let mut by_day: BTreeMap<String, Vec<&RollupPoint>> = BTreeMap::new();
        for r in &rollups {
            by_day
                .entry(r.bucket_start.format("%Y-%m-%d").to_string())
                .or_default()
                .push(r);
        }
        for (date, group) in by_day {
            let batch = rollups_to_batch(&group)?;
            self.put_parquet(&day_partition_key(METRICS_5M, &date, "metrics_5m"), &batch)?;
        }
        Ok(())
    }

    /// Rollups from day partitions that can hold buckets at or after `since`.
    pub fn rollups_since(&self, since: Option<DateTime<Utc>>) -> Result<Vec<RollupPoint>> {
        let mut out = Vec::new();
        self.scan_partitions(
            METRICS_5M,
            &ColdPredicate::since(since),
            &ROLLUP_COLUMNS,
            &mut |batches| {
                for batch in batches {
                    out.extend(batch_to_rollups(batch)?);
                }
                Ok(true)
            },
        )?;
        Ok(out)
    }

    // ── Object I/O helpers ──────────────────────────────────────────

    /// Encode `batch` as Parquet in memory and write it as one atomic object.
    ///
    /// Row groups are capped at [`ROW_GROUP_ROWS`] so the per-group column
    /// statistics (min/max, written by default) give the read path something
    /// to prune on inside large objects.
    fn put_parquet(&self, key: &str, batch: &RecordBatch) -> Result<()> {
        let props = WriterProperties::builder()
            .set_max_row_group_row_count(Some(self.row_group_rows))
            .build();
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut buf, batch.schema(), Some(props))
                .with_context(|| format!("creating parquet writer for {key}"))?;
            writer.write(batch)?;
            writer.close()?;
        }
        self.backend.put(key, &buf)
    }

    /// Group records by `date=…/hour=…` of their timestamp, sort+encode each
    /// group via `to_batch`, and write one Parquet object per partition.
    fn write_partitioned<T>(
        &self,
        root: &str,
        stem: &str,
        records: &[T],
        ts_of: impl Fn(&T) -> DateTime<Utc>,
        to_batch: impl Fn(&mut Vec<&T>) -> Result<RecordBatch>,
    ) -> Result<()> {
        use std::collections::BTreeMap;
        let mut by_partition: BTreeMap<(String, String), Vec<&T>> = BTreeMap::new();
        for r in records {
            let dt = ts_of(r);
            let key = (
                dt.format("%Y-%m-%d").to_string(),
                dt.format("%H").to_string(),
            );
            by_partition.entry(key).or_default().push(r);
        }
        for ((date, hour), mut group) in by_partition {
            let batch = to_batch(&mut group)?;
            self.put_parquet(&partition_key(root, &date, &hour, stem), &batch)?;
        }
        Ok(())
    }

    /// The scan core: list the objects under `prefix`, group them by time
    /// partition, and visit partitions newest-first, applying the three
    /// pushdown levels along the way. `visit` receives the filtered record
    /// batches of one partition and returns `Ok(true)` to continue.
    fn scan_partitions(
        &self,
        prefix: &str,
        pred: &ColdPredicate,
        columns: &PushdownColumns,
        visit: &mut dyn FnMut(&[RecordBatch]) -> Result<bool>,
    ) -> Result<ColdScanStats> {
        let mut stats = ColdScanStats::default();
        let mut keys: Vec<String> = self
            .backend
            .list(prefix)?
            .into_iter()
            .filter(|k| k.ends_with(".parquet"))
            .collect();
        stats.objects_listed = keys.len();

        // Level 1: drop whole partitions the time bound excludes.
        if let Some(since) = pred.since {
            let before = keys.len();
            keys.retain(|k| partition_may_contain_since(k, since));
            stats.objects_pruned_by_partition = before - keys.len();
        }

        // Group by partition path (the key up to the final `/`) and order
        // newest-first. Fixed-width date/hour segments make the partition path
        // lexicographically chronological; unparsable keys sort last but are
        // always visited (pruning must never hide data over a naming surprise).
        let mut partitions: std::collections::BTreeMap<std::cmp::Reverse<String>, Vec<String>> =
            std::collections::BTreeMap::new();
        for key in keys {
            let partition = key
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_string())
                .unwrap_or_default();
            partitions
                .entry(std::cmp::Reverse(partition))
                .or_default()
                .push(key);
        }

        let mut stopped = false;
        for (_, part_keys) in partitions {
            if stopped {
                stats.objects_skipped_by_early_exit += part_keys.len();
                continue;
            }
            let mut batches = Vec::new();
            for key in &part_keys {
                let Some(bytes) = self.backend.get(key)? else {
                    continue; // raced with a concurrent delete (e.g. retention)
                };
                stats.objects_read += 1;
                self.read_object(bytes, pred, columns, &mut stats, &mut batches)
                    .with_context(|| format!("reading cold object {key}"))?;
            }
            if !visit(&batches)? {
                stopped = true;
            }
        }
        Ok(stats)
    }

    /// Open one Parquet object, prune row groups by statistics (level 2), and
    /// decode the survivors through a row filter (level 3).
    fn read_object(
        &self,
        bytes: Vec<u8>,
        pred: &ColdPredicate,
        columns: &PushdownColumns,
        stats: &mut ColdScanStats,
        out: &mut Vec<RecordBatch>,
    ) -> Result<()> {
        let mut builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))?;

        // Level 2: keep only row groups whose statistics can match.
        let groups_total = builder.metadata().row_groups().len();
        stats.row_groups_total += groups_total;
        let keep: Vec<usize> = builder
            .metadata()
            .row_groups()
            .iter()
            .enumerate()
            .filter(|(_, rg)| row_group_may_match(rg, pred, columns))
            .map(|(i, _)| i)
            .collect();
        stats.row_groups_pruned += groups_total - keep.len();
        if keep.is_empty() {
            return Ok(());
        }
        if keep.len() < groups_total {
            builder = builder.with_row_groups(keep);
        }

        // Level 3: filter rows at the decoder.
        if let Some(filter) = build_row_filter(&builder, pred, columns) {
            builder = builder.with_row_filter(filter);
        }

        for batch in builder.build()? {
            let batch = batch?;
            if batch.num_rows() == 0 {
                continue;
            }
            stats.rows_decoded += batch.num_rows();
            out.push(batch);
        }
        Ok(())
    }
}

/// Whether a row group's column statistics admit any row matching `pred`.
/// Conservative: a missing column or missing statistics keeps the group.
fn row_group_may_match(
    rg: &RowGroupMetaData,
    pred: &ColdPredicate,
    columns: &PushdownColumns,
) -> bool {
    if let Some(since_ns) = pred.since_ns()
        && let Some((_, max)) = i64_column_stats(rg, columns.ts)
        && max < since_ns
    {
        return false;
    }
    let excludes_eq = |col: Option<&str>, value: Option<&str>| -> bool {
        match (col, value) {
            (Some(col), Some(value)) => match utf8_column_stats(rg, col) {
                Some((min, max)) => value < min.as_str() || value > max.as_str(),
                None => false,
            },
            _ => false,
        }
    };
    if excludes_eq(columns.trace_id, pred.trace_id.as_deref())
        || excludes_eq(columns.service, pred.service.as_deref())
        || excludes_eq(columns.name, pred.name.as_deref())
    {
        return false;
    }
    if pred.error_only && excludes_eq(columns.status, Some("error")) {
        return false;
    }
    true
}

/// Min/max statistics of an `Int64` column in a row group, if present.
fn i64_column_stats(rg: &RowGroupMetaData, column: &str) -> Option<(i64, i64)> {
    let col = rg
        .columns()
        .iter()
        .find(|c| c.column_descr().name() == column)?;
    match col.statistics()? {
        Statistics::Int64(s) => Some((*s.min_opt()?, *s.max_opt()?)),
        _ => None,
    }
}

/// Min/max statistics of a UTF-8 column in a row group, if present.
fn utf8_column_stats(rg: &RowGroupMetaData, column: &str) -> Option<(String, String)> {
    let col = rg
        .columns()
        .iter()
        .find(|c| c.column_descr().name() == column)?;
    match col.statistics()? {
        Statistics::ByteArray(s) => Some((
            s.min_opt()?.as_utf8().ok()?.to_string(),
            s.max_opt()?.as_utf8().ok()?.to_string(),
        )),
        _ => None,
    }
}

/// Build the decoder-level [`RowFilter`] for `pred`, or `None` when the
/// predicate binds to no column of this file (then everything decodes and the
/// caller's residual filter is the only gate, exactly as before pushdown).
fn build_row_filter<T>(
    builder: &parquet::arrow::arrow_reader::ArrowReaderBuilder<T>,
    pred: &ColdPredicate,
    columns: &PushdownColumns,
) -> Option<RowFilter> {
    // (leaf index, comparison) for every predicate field with a column in
    // this file. All tael cold schemas are flat, so the arrow field index is
    // the parquet leaf index.
    enum Cmp {
        TsAtLeast(i64),
        Utf8Eq(String),
    }
    let schema = builder.schema();
    let mut comparisons: Vec<(usize, Cmp)> = Vec::new();
    let mut bind = |col: Option<&str>, cmp: Cmp| {
        if let Some(col) = col
            && let Some((idx, _)) = schema.column_with_name(col)
        {
            comparisons.push((idx, cmp));
        }
    };
    if let Some(since_ns) = pred.since_ns() {
        bind(Some(columns.ts), Cmp::TsAtLeast(since_ns));
    }
    if let Some(t) = &pred.trace_id {
        bind(columns.trace_id, Cmp::Utf8Eq(t.clone()));
    }
    if let Some(s) = &pred.service {
        bind(columns.service, Cmp::Utf8Eq(s.clone()));
    }
    if let Some(n) = &pred.name {
        bind(columns.name, Cmp::Utf8Eq(n.clone()));
    }
    if pred.error_only {
        bind(columns.status, Cmp::Utf8Eq("error".to_string()));
    }
    if comparisons.is_empty() {
        return None;
    }

    let mask = ProjectionMask::leaves(
        builder.parquet_schema(),
        comparisons.iter().map(|(idx, _)| *idx),
    );
    // The masked batch keeps schema order but re-indexes columns, so the
    // predicate resolves them by name at evaluation time.
    let by_name: Vec<(String, Cmp)> = comparisons
        .into_iter()
        .map(|(idx, cmp)| (schema.field(idx).name().clone(), cmp))
        .collect();
    let predicate = ArrowPredicateFn::new(mask, move |batch: RecordBatch| {
        use arrow::compute::kernels::cmp;
        let mut keep: Option<BooleanArray> = None;
        for (name, comparison) in &by_name {
            let column = batch.column_by_name(name).ok_or_else(|| {
                arrow::error::ArrowError::SchemaError(format!(
                    "predicate column {name} missing from projected batch"
                ))
            })?;
            let matched = match comparison {
                Cmp::TsAtLeast(ns) => cmp::gt_eq(column, &Int64Array::new_scalar(*ns))?,
                Cmp::Utf8Eq(v) => cmp::eq(column, &StringArray::new_scalar(v.clone()))?,
            };
            keep = Some(match keep {
                Some(prev) => arrow::compute::and(&prev, &matched)?,
                None => matched,
            });
        }
        Ok(keep.expect("comparisons is non-empty"))
    });
    Some(RowFilter::new(vec![Box::new(predicate)]))
}

/// Whether the partition a key belongs to can contain rows with timestamps at
/// or after `since`. Rows land in the partition of their own timestamp, so a
/// partition strictly before `since`'s date (and hour, when the key carries
/// one) cannot. Both segments are fixed-width zero-padded, so lexicographic
/// comparison is chronological. A key with no parsable date is kept — pruning
/// must never hide data over a naming surprise.
fn partition_may_contain_since(key: &str, since: DateTime<Utc>) -> bool {
    let Some(date) = parse_date_segment(key) else {
        return true;
    };
    let since_date = since.format("%Y-%m-%d").to_string();
    match date.cmp(since_date.as_str()) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            // Same date: an hour-partitioned key needs its hour checked; a
            // day-partitioned key (rollups) covers the whole day.
            match key.split('/').find_map(|seg| seg.strip_prefix("hour=")) {
                Some(hour) => hour >= since.format("%H").to_string().as_str(),
                None => true,
            }
        }
    }
}

/// `<root>/date=<date>/hour=<hour>/<stem>-<ulid>.parquet`.
fn partition_key(root: &str, date: &str, hour: &str, stem: &str) -> String {
    format!(
        "{root}/date={date}/hour={hour}/{stem}-{}.parquet",
        ulid::Ulid::new()
    )
}

/// `<root>/date=<date>/<stem>-<ulid>.parquet` (day-granular, for rollups).
fn day_partition_key(root: &str, date: &str, stem: &str) -> String {
    format!("{root}/date={date}/{stem}-{}.parquet", ulid::Ulid::new())
}

/// Extract the `date=YYYY-MM-DD` segment's value from a partition key.
fn parse_date_segment(key: &str) -> Option<&str> {
    key.split('/').find_map(|seg| seg.strip_prefix("date="))
}

/// Aggregate raw points into 5-minute (service, name) buckets.
fn downsample(points: &[MetricPoint]) -> Vec<RollupPoint> {
    use std::collections::HashMap;
    let mut buckets: HashMap<(String, String, i64), RollupPoint> = HashMap::new();
    for p in points {
        let ns = p.timestamp.timestamp_nanos_opt().unwrap_or(0);
        let secs = ns.div_euclid(1_000_000_000);
        let bucket_secs = secs - secs.rem_euclid(ROLLUP_BUCKET_SECS);
        let key = (p.service.clone(), p.name.clone(), bucket_secs);
        let entry = buckets.entry(key).or_insert_with(|| RollupPoint {
            bucket_start: Utc.timestamp_opt(bucket_secs, 0).unwrap(),
            service: p.service.clone(),
            name: p.name.clone(),
            min: p.value,
            max: p.value,
            sum: 0.0,
            count: 0,
        });
        entry.min = entry.min.min(p.value);
        entry.max = entry.max.max(p.value);
        entry.sum += p.value;
        entry.count += 1;
    }
    buckets.into_values().collect()
}

// ── Arrow schema + (de)serialization ────────────────────────────────

fn span_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("span_id", DataType::Utf8, false),
        Field::new("parent_span_id", DataType::Utf8, true),
        Field::new("service", DataType::Utf8, false),
        Field::new("operation", DataType::Utf8, false),
        Field::new("start_ns", DataType::Int64, false),
        Field::new("end_ns", DataType::Int64, false),
        Field::new("duration_ms", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("attributes_json", DataType::Utf8, false),
        Field::new("events_json", DataType::Utf8, false),
        Field::new("llm_json", DataType::Utf8, true),
    ]))
}

fn spans_to_batch(spans: &[&Span]) -> Result<RecordBatch> {
    let trace_id: Vec<&str> = spans.iter().map(|s| s.trace_id.as_str()).collect();
    let span_id: Vec<&str> = spans.iter().map(|s| s.span_id.as_str()).collect();
    let parent: Vec<Option<&str>> = spans.iter().map(|s| s.parent_span_id.as_deref()).collect();
    let service: Vec<&str> = spans.iter().map(|s| s.service.as_str()).collect();
    let operation: Vec<&str> = spans.iter().map(|s| s.operation.as_str()).collect();
    let start_ns: Vec<i64> = spans
        .iter()
        .map(|s| s.start_time.timestamp_nanos_opt().unwrap_or(0))
        .collect();
    let end_ns: Vec<i64> = spans
        .iter()
        .map(|s| s.end_time.timestamp_nanos_opt().unwrap_or(0))
        .collect();
    let duration: Vec<f64> = spans.iter().map(|s| s.duration_ms).collect();
    let status: Vec<String> = spans.iter().map(|s| s.status.to_string()).collect();
    let kind: Vec<String> = spans.iter().map(|s| s.kind.to_string()).collect();
    let attrs: Vec<String> = spans
        .iter()
        .map(|s| serde_json::to_string(&s.attributes).unwrap_or_else(|_| "{}".into()))
        .collect();
    let events: Vec<String> = spans
        .iter()
        .map(|s| serde_json::to_string(&s.events).unwrap_or_else(|_| "[]".into()))
        .collect();
    let llm: Vec<Option<String>> = spans
        .iter()
        .map(|s| {
            s.llm
                .as_ref()
                .map(|l| serde_json::to_string(l).unwrap_or_default())
        })
        .collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(trace_id)),
        Arc::new(StringArray::from(span_id)),
        Arc::new(StringArray::from(parent)),
        Arc::new(StringArray::from(service)),
        Arc::new(StringArray::from(operation)),
        Arc::new(Int64Array::from(start_ns)),
        Arc::new(Int64Array::from(end_ns)),
        Arc::new(Float64Array::from(duration)),
        Arc::new(StringArray::from(status)),
        Arc::new(StringArray::from(kind)),
        Arc::new(StringArray::from(attrs)),
        Arc::new(StringArray::from(events)),
        Arc::new(StringArray::from(llm)),
    ];
    Ok(RecordBatch::try_new(span_schema(), columns)?)
}

fn batch_to_spans(batch: &RecordBatch) -> Result<Vec<Span>> {
    macro_rules! col {
        ($i:expr, $ty:ty) => {
            batch
                .column($i)
                .as_any()
                .downcast_ref::<$ty>()
                .context("unexpected column type in cold parquet")?
        };
    }
    let trace_id = col!(0, StringArray);
    let span_id = col!(1, StringArray);
    let parent = col!(2, StringArray);
    let service = col!(3, StringArray);
    let operation = col!(4, StringArray);
    let start_ns = col!(5, Int64Array);
    let end_ns = col!(6, Int64Array);
    let duration = col!(7, Float64Array);
    let status = col!(8, StringArray);
    let kind = col!(9, StringArray);
    let attrs = col!(10, StringArray);
    let events = col!(11, StringArray);
    let llm = col!(12, StringArray);

    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        out.push(Span {
            trace_id: trace_id.value(i).to_string(),
            span_id: span_id.value(i).to_string(),
            parent_span_id: if parent.is_null(i) {
                None
            } else {
                Some(parent.value(i).to_string())
            },
            service: service.value(i).to_string(),
            operation: operation.value(i).to_string(),
            start_time: ns_to_dt(start_ns.value(i)),
            end_time: ns_to_dt(end_ns.value(i)),
            duration_ms: duration.value(i),
            status: SpanStatus::from_str(status.value(i)),
            attributes: serde_json::from_str(attrs.value(i)).unwrap_or_default(),
            events: serde_json::from_str(events.value(i)).unwrap_or_default(),
            kind: SpanKind::from_str(kind.value(i)),
            llm: if llm.is_null(i) {
                None
            } else {
                serde_json::from_str(llm.value(i)).ok()
            },
        });
    }
    Ok(out)
}

fn ns_to_dt(ns: i64) -> DateTime<Utc> {
    Utc.timestamp_nanos(ns)
}

// ── Logs schema ─────────────────────────────────────────────────────

fn log_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_ns", DataType::Int64, false),
        Field::new("observed_ns", DataType::Int64, false),
        Field::new("trace_id", DataType::Utf8, true),
        Field::new("span_id", DataType::Utf8, true),
        Field::new("severity", DataType::Utf8, false),
        Field::new("severity_text", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("attributes_json", DataType::Utf8, false),
        Field::new("body_sha256", DataType::Utf8, true),
    ]))
}

fn logs_to_batch(logs: &[&LogRecord]) -> Result<RecordBatch> {
    let ns = |dt: DateTime<Utc>| dt.timestamp_nanos_opt().unwrap_or(0);
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(
            logs.iter().map(|l| ns(l.timestamp)).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            logs.iter()
                .map(|l| ns(l.observed_timestamp))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| l.trace_id.as_deref())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| l.span_id.as_deref())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| l.severity.to_string())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| l.severity_text.as_str())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter().map(|l| l.body.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter().map(|l| l.service.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| serde_json::to_string(&l.attributes).unwrap_or_else(|_| "{}".into()))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            logs.iter()
                .map(|l| l.body_sha256.as_deref())
                .collect::<Vec<_>>(),
        )),
    ];
    Ok(RecordBatch::try_new(log_schema(), columns)?)
}

fn batch_to_logs(batch: &RecordBatch) -> Result<Vec<LogRecord>> {
    macro_rules! col {
        ($i:expr, $ty:ty) => {
            batch
                .column($i)
                .as_any()
                .downcast_ref::<$ty>()
                .context("bad log column")?
        };
    }
    let ts = col!(0, Int64Array);
    let observed = col!(1, Int64Array);
    let trace_id = col!(2, StringArray);
    let span_id = col!(3, StringArray);
    let severity = col!(4, StringArray);
    let severity_text = col!(5, StringArray);
    let body = col!(6, StringArray);
    let service = col!(7, StringArray);
    let attrs = col!(8, StringArray);
    let body_sha = col!(9, StringArray);
    let opt = |a: &StringArray, i: usize| {
        if a.is_null(i) {
            None
        } else {
            Some(a.value(i).to_string())
        }
    };

    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        out.push(LogRecord {
            timestamp: ns_to_dt(ts.value(i)),
            observed_timestamp: ns_to_dt(observed.value(i)),
            trace_id: opt(trace_id, i),
            span_id: opt(span_id, i),
            severity: LogSeverity::from_str(severity.value(i)),
            severity_text: severity_text.value(i).to_string(),
            body: body.value(i).to_string(),
            service: service.value(i).to_string(),
            attributes: serde_json::from_str(attrs.value(i)).unwrap_or_default(),
            body_sha256: opt(body_sha, i),
        });
    }
    Ok(out)
}

// ── Metrics schema ──────────────────────────────────────────────────

fn metric_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts_ns", DataType::Int64, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("metric_type", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
        Field::new("unit", DataType::Utf8, false),
        Field::new("attributes_json", DataType::Utf8, false),
        // Bucket layout for histogram points; null for every other type.
        // Stored as JSON alongside `attributes_json` rather than as a nested
        // list column so the SQL surface can read it without a struct decoder.
        Field::new("histogram_json", DataType::Utf8, true),
    ]))
}

fn metrics_to_batch(metrics: &[&MetricPoint]) -> Result<RecordBatch> {
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(
            metrics
                .iter()
                .map(|m| m.timestamp.timestamp_nanos_opt().unwrap_or(0))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics
                .iter()
                .map(|m| m.service.as_str())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics
                .iter()
                .map(|m| m.metric_type.to_string())
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            metrics.iter().map(|m| m.value).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics.iter().map(|m| m.unit.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics
                .iter()
                .map(|m| serde_json::to_string(&m.attributes).unwrap_or_else(|_| "{}".into()))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            metrics
                .iter()
                .map(|m| {
                    m.histogram
                        .as_ref()
                        .and_then(|h| serde_json::to_string(h).ok())
                })
                .collect::<Vec<_>>(),
        )),
    ];
    Ok(RecordBatch::try_new(metric_schema(), columns)?)
}

// ── Rollup (metrics_5m) schema ──────────────────────────────────────

fn rollup_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("bucket_ns", DataType::Int64, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("min", DataType::Float64, false),
        Field::new("max", DataType::Float64, false),
        Field::new("sum", DataType::Float64, false),
        Field::new("count", DataType::Int64, false),
    ]))
}

fn rollups_to_batch(rollups: &[&RollupPoint]) -> Result<RecordBatch> {
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from(
            rollups
                .iter()
                .map(|r| r.bucket_start.timestamp_nanos_opt().unwrap_or(0))
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rollups
                .iter()
                .map(|r| r.service.as_str())
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rollups.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rollups.iter().map(|r| r.min).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rollups.iter().map(|r| r.max).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rollups.iter().map(|r| r.sum).collect::<Vec<_>>(),
        )),
        Arc::new(Int64Array::from(
            rollups.iter().map(|r| r.count).collect::<Vec<_>>(),
        )),
    ];
    Ok(RecordBatch::try_new(rollup_schema(), columns)?)
}

fn batch_to_rollups(batch: &RecordBatch) -> Result<Vec<RollupPoint>> {
    macro_rules! col {
        ($i:expr, $ty:ty) => {
            batch
                .column($i)
                .as_any()
                .downcast_ref::<$ty>()
                .context("bad rollup column")?
        };
    }
    let bucket = col!(0, Int64Array);
    let service = col!(1, StringArray);
    let name = col!(2, StringArray);
    let min = col!(3, Float64Array);
    let max = col!(4, Float64Array);
    let sum = col!(5, Float64Array);
    let count = col!(6, Int64Array);
    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        out.push(RollupPoint {
            bucket_start: ns_to_dt(bucket.value(i)),
            service: service.value(i).to_string(),
            name: name.value(i).to_string(),
            min: min.value(i),
            max: max.value(i),
            sum: sum.value(i),
            count: count.value(i),
        });
    }
    Ok(out)
}

fn batch_to_metrics(batch: &RecordBatch) -> Result<Vec<MetricPoint>> {
    macro_rules! col {
        ($i:expr, $ty:ty) => {
            batch
                .column($i)
                .as_any()
                .downcast_ref::<$ty>()
                .context("bad metric column")?
        };
    }
    let ts = col!(0, Int64Array);
    let service = col!(1, StringArray);
    let name = col!(2, StringArray);
    let mtype = col!(3, StringArray);
    let value = col!(4, Float64Array);
    let unit = col!(5, StringArray);
    let attrs = col!(6, StringArray);
    // Parquet files written before histogram buckets were retained have no
    // such column; those points simply read back without a distribution.
    let histograms = (batch.num_columns() > 7)
        .then(|| {
            batch
                .column(7)
                .as_any()
                .downcast_ref::<StringArray>()
                .context("bad histogram column")
        })
        .transpose()?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        out.push(MetricPoint {
            timestamp: ns_to_dt(ts.value(i)),
            service: service.value(i).to_string(),
            name: name.value(i).to_string(),
            metric_type: MetricType::from_str(mtype.value(i)),
            value: value.value(i),
            unit: unit.value(i).to_string(),
            attributes: serde_json::from_str(attrs.value(i)).unwrap_or_default(),
            histogram: histograms
                .filter(|h| !h.is_null(i))
                .and_then(|h| serde_json::from_str(h.value(i)).ok()),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn partition_pruning_compares_date_then_hour() {
        let since = Utc.with_ymd_and_hms(2026, 7, 15, 9, 30, 0).unwrap();
        let key = |d: &str, h: &str| format!("spans/date={d}/hour={h}/spans-x.parquet");
        assert!(!partition_may_contain_since(
            &key("2026-07-14", "23"),
            since
        ));
        assert!(!partition_may_contain_since(
            &key("2026-07-15", "08"),
            since
        ));
        // since's own hour partition holds rows on both sides of since.
        assert!(partition_may_contain_since(&key("2026-07-15", "09"), since));
        assert!(partition_may_contain_since(&key("2026-07-15", "10"), since));
        assert!(partition_may_contain_since(&key("2026-07-16", "00"), since));
        // Day-granular keys (rollups) prune on date alone.
        assert!(!partition_may_contain_since(
            "metrics_5m/date=2026-07-14/r.parquet",
            since
        ));
        assert!(partition_may_contain_since(
            "metrics_5m/date=2026-07-15/r.parquet",
            since
        ));
        // No parsable date: never pruned.
        assert!(partition_may_contain_since("spans/odd-key.parquet", since));
    }

    #[test]
    fn spans_since_skips_old_partitions_but_keeps_boundary_rows() {
        let dir = tempfile::tempdir().unwrap();
        let tier = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        let old = Utc::now() - chrono::Duration::days(10);
        let recent = Utc::now();
        tier.write_spans(&[span_at("t-old", "s1", old), span_at("t-new", "s2", recent)])
            .unwrap();

        let all = tier.spans_since(None).unwrap();
        assert_eq!(all.len(), 2);

        let since = Utc::now() - chrono::Duration::days(1);
        let pruned = tier.spans_since(Some(since)).unwrap();
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0].trace_id, "t-new");
    }

    fn span(trace: &str, sid: &str) -> Span {
        Span {
            trace_id: trace.into(),
            span_id: sid.into(),
            parent_span_id: None,
            service: "svc".into(),
            operation: "op".into(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 1.0,
            status: SpanStatus::Ok,
            attributes: HashMap::from([("k".to_string(), "v".to_string())]),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    fn span_at(trace: &str, sid: &str, when: DateTime<Utc>) -> Span {
        let mut s = span(trace, sid);
        s.start_time = when;
        s.end_time = when;
        s
    }

    #[test]
    fn spans_round_trip_through_parquet() {
        let dir = tempfile::tempdir().unwrap();
        let cold = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        cold.write_spans(&[span("t1", "a"), span("t1", "b"), span("t2", "c")])
            .unwrap();

        let t1 = cold.get_trace("t1").unwrap();
        assert_eq!(t1.len(), 2);
        assert!(t1.iter().all(|s| s.trace_id == "t1"));
        assert_eq!(t1[0].attributes.get("k").map(String::as_str), Some("v"));
        assert_eq!(cold.spans_since(None).unwrap().len(), 3);
        assert!(cold.get_trace("missing").unwrap().is_empty());
    }

    #[test]
    fn downsampling_aggregates_5m_buckets() {
        use crate::storage::models::{MetricPoint, MetricType};
        let dir = tempfile::tempdir().unwrap();
        let cold = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        let base = Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap();
        let mk = |offset_secs: i64, v: f64| MetricPoint {
            timestamp: base + chrono::Duration::seconds(offset_secs),
            service: "api".into(),
            name: "rps".into(),
            metric_type: MetricType::Gauge,
            value: v,
            unit: "1".into(),
            attributes: std::collections::HashMap::new(),
            histogram: None,
        };
        // Three points in the same 5m bucket (0,60,120s) + one in the next (360s).
        cold.write_downsampled(&[mk(0, 10.0), mk(60, 30.0), mk(120, 20.0), mk(360, 5.0)])
            .unwrap();

        let mut rollups = cold.rollups_since(None).unwrap();
        rollups.sort_by_key(|r| r.bucket_start);
        assert_eq!(rollups.len(), 2);
        let first = &rollups[0];
        assert_eq!(first.count, 3);
        assert_eq!(first.min, 10.0);
        assert_eq!(first.max, 30.0);
        assert_eq!(first.sum, 60.0);
        assert_eq!(first.avg(), 20.0);
        assert_eq!(rollups[1].count, 1);
    }

    /// A tier with tiny row groups, so a few hundred rows produce several
    /// groups per object and pruning has something to skip.
    fn tier_with_small_groups(dir: &tempfile::TempDir, rows: usize) -> ColdTier {
        let mut tier = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        tier.set_row_group_rows(rows);
        tier
    }

    #[test]
    fn get_trace_prunes_row_groups_via_trace_id_statistics() {
        let dir = tempfile::tempdir().unwrap();
        let tier = tier_with_small_groups(&dir, 16);
        // 128 spans, one per trace, all in one hour partition → one object
        // with 8 row groups sorted by trace_id.
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let spans: Vec<Span> = (0..128)
            .map(|i| span_at(&format!("trace-{i:04}"), &format!("s{i}"), base))
            .collect();
        tier.write_spans(&spans).unwrap();

        let pred = ColdPredicate {
            trace_id: Some("trace-0100".into()),
            ..Default::default()
        };
        let (found, stats) = tier.spans_matching(&pred).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].trace_id, "trace-0100");
        assert_eq!(stats.objects_read, 1);
        assert_eq!(stats.row_groups_total, 8);
        assert!(
            stats.row_groups_pruned >= 7,
            "trace-sorted statistics must exclude the other groups, pruned {} of {}",
            stats.row_groups_pruned,
            stats.row_groups_total
        );
        assert_eq!(
            stats.rows_decoded, 1,
            "the row filter drops non-matches at the decoder"
        );
    }

    #[test]
    fn time_predicate_prunes_row_groups_inside_a_partition() {
        // Rollup objects are day-partitioned, so one day's buckets span many
        // hours inside a single object — exactly the case partition pruning
        // alone cannot help with and row-group statistics can.
        let dir = tempfile::tempdir().unwrap();
        let tier = tier_with_small_groups(&dir, 16);
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let spans: Vec<Span> = (0..64)
            .map(|i| {
                span_at(
                    &format!("t{i:03}"),
                    &format!("s{i}"),
                    base + chrono::Duration::minutes(i),
                )
            })
            .collect();
        // All within one hour partition; written sorted by trace id, which
        // here coincides with time order so ts statistics are tight.
        tier.write_spans(&spans).unwrap();

        let since = base + chrono::Duration::minutes(56);
        let (found, stats) = tier
            .spans_matching(&ColdPredicate::since(Some(since)))
            .unwrap();
        assert_eq!(found.len(), 8);
        assert!(
            stats.row_groups_pruned >= 2,
            "row groups wholly before `since` must be skipped, pruned {} of {}",
            stats.row_groups_pruned,
            stats.row_groups_total
        );
        assert_eq!(stats.rows_decoded, 8);
    }

    #[test]
    fn pushdown_matches_a_full_scan_plus_filter() {
        // The oracle test: for a grid of predicates, the pushed-down scan must
        // return exactly what "read everything, filter in memory" returns.
        let dir = tempfile::tempdir().unwrap();
        let tier = tier_with_small_groups(&dir, 8);
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let mut spans = Vec::new();
        for i in 0..60 {
            let mut s = span_at(
                &format!("t{:02}", i % 20),
                &format!("s{i}"),
                base + chrono::Duration::hours(i % 5),
            );
            s.service = if i % 3 == 0 {
                "api".into()
            } else {
                "worker".into()
            };
            if i % 7 == 0 {
                s.status = SpanStatus::Error;
            }
            spans.push(s);
        }
        tier.write_spans(&spans).unwrap();
        let all = tier.spans_since(None).unwrap();
        assert_eq!(all.len(), 60);

        let preds = [
            ColdPredicate {
                service: Some("api".into()),
                ..Default::default()
            },
            ColdPredicate {
                error_only: true,
                ..Default::default()
            },
            ColdPredicate {
                trace_id: Some("t07".into()),
                ..Default::default()
            },
            ColdPredicate {
                since: Some(base + chrono::Duration::hours(3)),
                service: Some("worker".into()),
                ..Default::default()
            },
        ];
        for pred in &preds {
            let naive: Vec<&Span> = all
                .iter()
                .filter(|s| pred.since.is_none_or(|c| s.start_time >= c))
                .filter(|s| pred.trace_id.as_deref().is_none_or(|t| s.trace_id == t))
                .filter(|s| pred.service.as_deref().is_none_or(|v| s.service == v))
                .filter(|s| !pred.error_only || matches!(s.status, SpanStatus::Error))
                .collect();
            let (got, _) = tier.spans_matching(pred).unwrap();
            let mut got_ids: Vec<&str> = got.iter().map(|s| s.span_id.as_str()).collect();
            let mut want_ids: Vec<&str> = naive.iter().map(|s| s.span_id.as_str()).collect();
            got_ids.sort();
            want_ids.sort();
            assert_eq!(got_ids, want_ids, "pushdown diverged for {pred:?}");
        }
    }

    #[test]
    fn a_scan_stopped_at_a_partition_boundary_skips_older_objects() {
        let dir = tempfile::tempdir().unwrap();
        let tier = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        // Three hour partitions, one object each.
        for h in 0..3 {
            tier.write_spans(&[span_at(
                &format!("t{h}"),
                &format!("s{h}"),
                base - chrono::Duration::hours(h),
            )])
            .unwrap();
        }

        let mut seen = Vec::new();
        let stats = tier
            .scan_spans(&ColdPredicate::default(), &mut |spans| {
                seen.extend(spans.into_iter().map(|s| s.trace_id));
                false // stop after the first (newest) partition
            })
            .unwrap();
        assert_eq!(
            seen,
            vec!["t0".to_string()],
            "newest partition visits first"
        );
        assert_eq!(stats.objects_read, 1);
        assert_eq!(
            stats.objects_skipped_by_early_exit, 2,
            "older partitions are never fetched once the visitor stops"
        );
    }

    #[test]
    fn logs_and_metrics_reads_push_time_and_identity_down() {
        use crate::storage::models::{LogRecord, LogSeverity, MetricPoint, MetricType};
        let dir = tempfile::tempdir().unwrap();
        let tier = tier_with_small_groups(&dir, 8);
        let base = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();

        let logs: Vec<LogRecord> = (0..32)
            .map(|i| LogRecord {
                timestamp: base + chrono::Duration::seconds(i),
                observed_timestamp: base,
                trace_id: Some(format!("t{i}")),
                span_id: None,
                severity: LogSeverity::Info,
                severity_text: "INFO".into(),
                body: format!("line {i}"),
                service: if i % 2 == 0 {
                    "api".into()
                } else {
                    "worker".into()
                },
                attributes: HashMap::new(),
                body_sha256: None,
            })
            .collect();
        tier.write_logs(&logs).unwrap();
        assert_eq!(tier.logs_since(None).unwrap().len(), 32);

        let mut matched = Vec::new();
        let stats = tier
            .scan_logs(
                &ColdPredicate {
                    service: Some("api".into()),
                    ..Default::default()
                },
                &mut |logs| {
                    matched.extend(logs);
                    true
                },
            )
            .unwrap();
        assert_eq!(matched.len(), 16);
        assert!(matched.iter().all(|l| l.service == "api"));
        assert_eq!(
            stats.rows_decoded, 16,
            "the row filter must drop the other service at the decoder"
        );
        // Logs sort by (service, ts) → at least the all-worker groups prune.
        assert!(stats.row_groups_pruned >= 1);

        let points: Vec<MetricPoint> = (0..32)
            .map(|i| MetricPoint {
                timestamp: base + chrono::Duration::seconds(i),
                service: "api".into(),
                name: if i % 2 == 0 {
                    "aaa.rps".into()
                } else {
                    "zzz.lat".into()
                },
                metric_type: MetricType::Gauge,
                value: i as f64,
                unit: "1".into(),
                attributes: HashMap::new(),
                histogram: None,
            })
            .collect();
        tier.write_metrics(&points).unwrap();
        assert_eq!(tier.metrics_since(None).unwrap().len(), 32);

        let mut matched = Vec::new();
        let stats = tier
            .scan_metrics(
                &ColdPredicate {
                    name: Some("aaa.rps".into()),
                    ..Default::default()
                },
                &mut |points| {
                    matched.extend(points);
                    true
                },
            )
            .unwrap();
        assert_eq!(matched.len(), 16);
        assert!(matched.iter().all(|m| m.name == "aaa.rps"));
        assert_eq!(stats.rows_decoded, 16);
        // Metrics sort by (name, ts) → the zzz-only groups prune on name stats.
        assert!(stats.row_groups_pruned >= 1);
    }

    #[test]
    fn retention_drops_old_partitions_only() {
        let dir = tempfile::tempdir().unwrap();
        let cold = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        let old = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
        cold.write_spans(&[span_at("told", "a", old), span_at("tnew", "b", recent)])
            .unwrap();
        assert_eq!(cold.spans_since(None).unwrap().len(), 2);

        // Keep everything on/after 2026-05-01 → the Jan partition is dropped.
        let dropped = cold.drop_partitions_before("2026-05-01").unwrap();
        assert_eq!(dropped, 1);
        let remaining = cold.spans_since(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].trace_id, "tnew");
    }
}
