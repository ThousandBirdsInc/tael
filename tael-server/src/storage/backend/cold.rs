//! Parquet cold tier for `TaelBackend` (spans).
//!
//! Aged spans roll out of the LSM hot tier into immutable Parquet files,
//! **sorted by `trace_id`** within `cold/spans/date=YYYY-MM-DD/hour=HH/`
//! partitions so a span-tree read is one contiguous scan (see
//! `docs/tael-backend-design.md` → "Cold tier"). Reads currently scan the
//! partitions and filter in memory; DataFusion (Phase 6) replaces the manual
//! scan with predicate/partition pushdown. v1 writes to local disk; the path
//! layout is a valid object-store key prefix for the S3/R2 move (Phase 9).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::{Array, ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, TimeZone, Utc};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::storage::models::{
    LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanKind, SpanStatus,
};

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

pub struct ColdTier {
    spans_root: PathBuf,
    logs_root: PathBuf,
    metrics_root: PathBuf,
    metrics_5m_root: PathBuf,
}

impl ColdTier {
    pub fn open(data_dir: &str) -> Result<Self> {
        // The cold tier can live on a different mount than the hot tier — set
        // `TAEL_COLD_DIR` to a path backed by network/object storage (e.g. an
        // s3fs/gcsfuse mount) to keep aged Parquet off local disk. Native
        // S3/R2 via the `object_store` crate is the v2 follow-on (design B5),
        // which needs the read path to go async.
        let base = match std::env::var("TAEL_COLD_DIR") {
            Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
            _ => Path::new(data_dir).join("cold"),
        };
        let spans_root = base.join("spans");
        let logs_root = base.join("logs");
        let metrics_root = base.join("metrics");
        let metrics_5m_root = base.join("metrics_5m");
        for d in [&spans_root, &logs_root, &metrics_root, &metrics_5m_root] {
            std::fs::create_dir_all(d)
                .with_context(|| format!("creating cold dir {}", d.display()))?;
        }
        Ok(Self {
            spans_root,
            logs_root,
            metrics_root,
            metrics_5m_root,
        })
    }

    /// Write a batch of spans to Parquet, grouped into `date=…/hour=…`
    /// partitions and sorted by `trace_id` within each file.
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
            let dir = self
                .spans_root
                .join(format!("date={date}"))
                .join(format!("hour={hour}"));
            std::fs::create_dir_all(&dir)?;
            let file_path = dir.join(format!("spans-{}.parquet", ulid::Ulid::new()));
            let batch = spans_to_batch(&group)?;
            let file = std::fs::File::create(&file_path)
                .with_context(|| format!("creating parquet {}", file_path.display()))?;
            let mut writer = ArrowWriter::try_new(file, batch.schema(), None)?;
            writer.write(&batch)?;
            writer.close()?;
        }
        Ok(())
    }

    /// Drop whole `date=YYYY-MM-DD` partitions older than `cutoff_date`
    /// (exclusive) — metadata GC at partition granularity is an `unlink`, not a
    /// rewrite. Returns the number of partitions removed. `cutoff_date` is the
    /// oldest date to keep, formatted `YYYY-MM-DD`.
    pub fn drop_partitions_before(&self, cutoff_date: &str) -> Result<usize> {
        let mut dropped = 0;
        for root in [&self.spans_root, &self.logs_root, &self.metrics_root] {
            if !root.exists() {
                continue;
            }
            for entry in std::fs::read_dir(root)? {
                let path = entry?.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // Partition dirs look like `date=YYYY-MM-DD`; lexicographic
                // compare works because the date is zero-padded, fixed-width.
                if let Some(date) = name.strip_prefix("date=") {
                    if date < cutoff_date {
                        std::fs::remove_dir_all(&path)
                            .with_context(|| format!("dropping partition {}", path.display()))?;
                        dropped += 1;
                    }
                }
            }
        }
        Ok(dropped)
    }

    /// Read all spans for a trace from the cold tier.
    pub fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        let mut out = Vec::new();
        self.for_each_span(&mut |s: Span| {
            if s.trace_id == trace_id {
                out.push(s);
            }
        })?;
        Ok(out)
    }

    /// Read every cold span (used by the hot∪cold union, which then filters).
    pub fn all_spans(&self) -> Result<Vec<Span>> {
        let mut out = Vec::new();
        self.for_each_span(&mut |s: Span| out.push(s))?;
        Ok(out)
    }

    /// Walk every Parquet file under the spans root, decoding each row.
    fn for_each_span(&self, f: &mut dyn FnMut(Span)) -> Result<()> {
        for_each_row(&self.spans_root, &mut |b| {
            for s in batch_to_spans(b)? {
                f(s);
            }
            Ok(())
        })
    }

    // ── Logs ────────────────────────────────────────────────────────

    /// Write aged logs to Parquet, partitioned by date/hour, sorted by
    /// `(service, ts)`.
    pub fn write_logs(&self, logs: &[LogRecord]) -> Result<()> {
        write_partitioned(
            &self.logs_root,
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

    pub fn all_logs(&self) -> Result<Vec<LogRecord>> {
        let mut out = Vec::new();
        for_each_row(&self.logs_root, &mut |b| {
            out.extend(batch_to_logs(b)?);
            Ok(())
        })?;
        Ok(out)
    }

    // ── Metrics ─────────────────────────────────────────────────────

    /// Write aged metric points to Parquet, partitioned by date/hour, sorted by
    /// `(name, ts)`.
    pub fn write_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        write_partitioned(
            &self.metrics_root,
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

    pub fn all_metrics(&self) -> Result<Vec<MetricPoint>> {
        let mut out = Vec::new();
        for_each_row(&self.metrics_root, &mut |b| {
            out.extend(batch_to_metrics(b)?);
            Ok(())
        })?;
        Ok(out)
    }

    // ── Metric downsampling (5m rollups) ────────────────────────────

    /// Aggregate raw points into 5-minute (`service`, `name`) buckets and write
    /// them to `cold/metrics_5m/date=…/` (day-partitioned — rollups are sparse
    /// and long-lived). Idempotent per call; buckets across calls are not
    /// merged (acceptable: a series' raw points are downsampled once at
    /// compaction).
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
            let dir = self.metrics_5m_root.join(format!("date={date}"));
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("metrics_5m-{}.parquet", ulid::Ulid::new()));
            let batch = rollups_to_batch(&group)?;
            let file = std::fs::File::create(&path)?;
            let mut writer = ArrowWriter::try_new(file, batch.schema(), None)?;
            writer.write(&batch)?;
            writer.close()?;
        }
        Ok(())
    }

    pub fn all_rollups(&self) -> Result<Vec<RollupPoint>> {
        let mut out = Vec::new();
        for_each_row(&self.metrics_5m_root, &mut |b| {
            out.extend(batch_to_rollups(b)?);
            Ok(())
        })?;
        Ok(out)
    }
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

/// Group records by `date=…/hour=…` of their timestamp, sort+encode each group
/// via `to_batch`, and write one Parquet file per partition.
fn write_partitioned<T>(
    root: &Path,
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
        let dir = root
            .join(format!("date={date}"))
            .join(format!("hour={hour}"));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{stem}-{}.parquet", ulid::Ulid::new()));
        let batch = to_batch(&mut group)?;
        let file = std::fs::File::create(&path)
            .with_context(|| format!("creating parquet {}", path.display()))?;
        let mut writer = ArrowWriter::try_new(file, batch.schema(), None)?;
        writer.write(&batch)?;
        writer.close()?;
    }
    Ok(())
}

/// Walk every Parquet file under `root`, invoking `f` with each RecordBatch.
fn for_each_row(root: &Path, f: &mut dyn FnMut(&RecordBatch) -> Result<()>) -> Result<()> {
    let mut files = Vec::new();
    collect_parquet_files(root, &mut files)?;
    for path in files {
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening parquet {}", path.display()))?;
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
        for batch in reader {
            f(&batch?)?;
        }
    }
    Ok(())
}

fn collect_parquet_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_parquet_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
            out.push(path);
        }
    }
    Ok(())
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
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        assert_eq!(cold.all_spans().unwrap().len(), 3);
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
        };
        // Three points in the same 5m bucket (0,60,120s) + one in the next (360s).
        cold.write_downsampled(&[mk(0, 10.0), mk(60, 30.0), mk(120, 20.0), mk(360, 5.0)])
            .unwrap();

        let mut rollups = cold.all_rollups().unwrap();
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

    #[test]
    fn retention_drops_old_partitions_only() {
        let dir = tempfile::tempdir().unwrap();
        let cold = ColdTier::open(dir.path().to_str().unwrap()).unwrap();
        let old = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let recent = Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap();
        cold.write_spans(&[span_at("told", "a", old), span_at("tnew", "b", recent)])
            .unwrap();
        assert_eq!(cold.all_spans().unwrap().len(), 2);

        // Keep everything on/after 2026-05-01 → the Jan partition is dropped.
        let dropped = cold.drop_partitions_before("2026-05-01").unwrap();
        assert_eq!(dropped, 1);
        let remaining = cold.all_spans().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].trace_id, "tnew");
    }
}
