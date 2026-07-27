use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service: String,
    pub operation: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub duration_ms: f64,
    pub status: SpanStatus,
    pub attributes: HashMap<String, String>,
    pub events: Vec<SpanEvent>,
    /// Span kind. `Llm` is a synthetic marker set when GenAI attributes are
    /// detected during ingestion (see `ingest::otlp`). Defaults keep older
    /// stored rows and existing call sites working.
    #[serde(default)]
    pub kind: SpanKind,
    /// Typed LLM extension, present iff this span is an LLM call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<LlmSpan>,
}

/// Span kind. Mirrors the OpenTelemetry `SpanKind`, plus a synthetic `Llm`
/// variant that marks spans carrying a typed [`LlmSpan`] extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpanKind {
    #[default]
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
    Llm,
}

impl std::fmt::Display for SpanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SpanKind::Internal => "internal",
            SpanKind::Server => "server",
            SpanKind::Client => "client",
            SpanKind::Producer => "producer",
            SpanKind::Consumer => "consumer",
            SpanKind::Llm => "llm",
        };
        write!(f, "{s}")
    }
}

impl SpanKind {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "server" => SpanKind::Server,
            "client" => SpanKind::Client,
            "producer" => SpanKind::Producer,
            "consumer" => SpanKind::Consumer,
            "llm" => SpanKind::Llm,
            _ => SpanKind::Internal,
        }
    }
}

/// High-level LLM operation, from `gen_ai.operation.name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmOperation {
    #[default]
    Chat,
    Completion,
    Embedding,
    Tool,
    Other,
}

impl LlmOperation {
    /// Map an OpenTelemetry GenAI `gen_ai.operation.name` value.
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "chat" => LlmOperation::Chat,
            "text_completion" | "completion" => LlmOperation::Completion,
            "embeddings" | "embedding" => LlmOperation::Embedding,
            "execute_tool" | "tool" => LlmOperation::Tool,
            _ => LlmOperation::Other,
        }
    }
}

/// Typed extension for LLM spans. Well-known GenAI attributes are flattened
/// into these fields; the unbounded tail stays in `Span::attributes`. Prompt
/// and completion payloads are content-addressed blobs referenced by hash
/// (populated in a later phase); only the hashes live here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSpan {
    pub provider: String,
    pub model: String,
    pub operation: LlmOperation,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,

    /// Time to first token (streaming responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<f64>,
    /// Mean inter-token latency (streaming responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inter_token_ms: Option<f64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_sha256: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    Ok,
    Error,
    Unset,
}

impl std::fmt::Display for SpanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpanStatus::Ok => write!(f, "ok"),
            SpanStatus::Error => write!(f, "error"),
            SpanStatus::Unset => write!(f, "unset"),
        }
    }
}

impl SpanStatus {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ok" => SpanStatus::Ok,
            "error" => SpanStatus::Error,
            _ => SpanStatus::Unset,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    pub name: String,
    pub timestamp: DateTime<Utc>,
    pub attributes: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceComment {
    pub id: String,
    pub trace_id: String,
    pub span_id: Option<String>,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceQuery {
    pub service: Option<String>,
    pub operation: Option<String>,
    pub min_duration_ms: Option<f64>,
    pub max_duration_ms: Option<f64>,
    pub status: Option<String>,
    pub last_seconds: Option<i64>,
    pub limit: Option<u32>,
    /// Filters on span attributes. Each entry is ANDed.
    /// Keys with characters outside `[A-Za-z0-9._\-:/]` are rejected at the storage layer.
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
    /// Substring filters on span attribute values (`--attribute k~=v`). Exact
    /// matching alone forces callers to already know a value they are usually
    /// trying to discover — a URL with an ID in it, a model name with a date
    /// suffix — so a contains form is the difference between the filter being
    /// usable and not.
    #[serde(default)]
    pub attributes_contains: Vec<(String, String)>,
    /// Regex filters on span attribute values (`--attribute 'k=~pattern'`).
    #[serde(default)]
    pub attributes_regex: Vec<(String, String)>,
    /// Full-text query over LLM prompt/completion payloads (Tantivy syntax).
    /// Only honored by the `tael-backend` storage engine; ignored by DuckDB
    /// (which doesn't retain payload text).
    #[serde(default)]
    pub text: Option<String>,
    /// Restrict to one tenant. Set by the API layer from the caller's
    /// credentials, never by the caller — see [`crate::tenancy`].
    #[serde(default)]
    pub tenant: Option<String>,
}

/// Per-service rollup returned by `Store::list_services`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub span_count: i64,
    pub trace_count: i64,
    pub avg_duration_ms: f64,
    pub error_rate: f64,
}

// ── Log models ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub timestamp: DateTime<Utc>,
    pub observed_timestamp: DateTime<Utc>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub severity: LogSeverity,
    pub severity_text: String,
    /// The log body. For oversized bodies this is emptied at ingestion and the
    /// content moved to the blob store, referenced by [`Self::body_sha256`].
    pub body: String,
    pub service: String,
    pub attributes: HashMap<String, String>,
    /// Set when the body was offloaded to the content-addressed blob store
    /// (large bodies, e.g. stack traces). Resolve via the blob store to get
    /// the original text. `None` for inline bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogSeverity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Unspecified,
}

impl std::fmt::Display for LogSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogSeverity::Trace => write!(f, "trace"),
            LogSeverity::Debug => write!(f, "debug"),
            LogSeverity::Info => write!(f, "info"),
            LogSeverity::Warn => write!(f, "warn"),
            LogSeverity::Error => write!(f, "error"),
            LogSeverity::Fatal => write!(f, "fatal"),
            LogSeverity::Unspecified => write!(f, "unspecified"),
        }
    }
}

impl LogSeverity {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "trace" => LogSeverity::Trace,
            "debug" => LogSeverity::Debug,
            "info" => LogSeverity::Info,
            "warn" => LogSeverity::Warn,
            "error" => LogSeverity::Error,
            "fatal" => LogSeverity::Fatal,
            _ => LogSeverity::Unspecified,
        }
    }

    pub fn from_severity_number(n: i32) -> Self {
        match n {
            1..=4 => LogSeverity::Trace,
            5..=8 => LogSeverity::Debug,
            9..=12 => LogSeverity::Info,
            13..=16 => LogSeverity::Warn,
            17..=20 => LogSeverity::Error,
            21..=24 => LogSeverity::Fatal,
            _ => LogSeverity::Unspecified,
        }
    }
}

// ── Metric models ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricPoint {
    pub timestamp: DateTime<Utc>,
    pub service: String,
    pub name: String,
    pub metric_type: MetricType,
    pub value: f64,
    pub unit: String,
    pub attributes: HashMap<String, String>,
    /// Bucket layout for histogram points. Present for OTLP `Histogram` and
    /// `ExponentialHistogram` data points, which would otherwise collapse to
    /// their `sum` and make quantiles uncomputable. `None` for every other
    /// metric type, and for histograms ingested before buckets were retained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub histogram: Option<HistogramBuckets>,
}

/// An explicit-bounds histogram snapshot.
///
/// OTLP's exponential histograms are converted to explicit bounds at ingest, so
/// there is one shape to query no matter how a client encoded it. `counts` is
/// per-bucket (not cumulative) and always has exactly one more entry than
/// `bounds`: the trailing element counts observations above the last bound.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistogramBuckets {
    /// Inclusive upper bounds, ascending.
    pub bounds: Vec<f64>,
    /// Per-bucket observation counts; `bounds.len() + 1` entries.
    pub counts: Vec<u64>,
    /// Total observations, i.e. the sum of `counts`.
    pub count: u64,
    /// Sum of all observed values.
    pub sum: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Whether the counts are running totals or per-interval deltas. This
    /// decides how points combine over a window, so a quantile computed
    /// without it would be wrong for one of the two encodings.
    #[serde(default)]
    pub temporality: Temporality,
}

/// A 5-minute downsampled metric aggregate, as returned by the query API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricRollup {
    pub bucket_start: DateTime<Utc>,
    pub service: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: i64,
}

/// How a histogram's counts accumulate over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Temporality {
    /// Counts are running totals since the producer started; the newest point
    /// already contains every earlier observation.
    #[default]
    Cumulative,
    /// Counts cover only the interval since the previous point, so a window's
    /// distribution is the sum of its points.
    Delta,
}

impl HistogramBuckets {
    /// Whether the bucket arrays are consistent enough to compute from.
    pub fn is_well_formed(&self) -> bool {
        self.counts.len() == self.bounds.len() + 1
    }

    /// Estimate the value at quantile `phi` (0.0–1.0).
    ///
    /// Uses the same linear-interpolation-within-a-bucket approach Prometheus's
    /// `histogram_quantile` does, so the answer is bucket-resolution accurate
    /// rather than exact — a histogram does not retain individual observations.
    /// Observations that fall in the open-ended top bucket can only be reported
    /// as "at least the last bound", which is what `max` is used for when the
    /// producer sent it.
    pub fn quantile(&self, phi: f64) -> Option<f64> {
        if !self.is_well_formed() || self.count == 0 || !(0.0..=1.0).contains(&phi) {
            return None;
        }
        let target = phi * self.count as f64;
        let mut cumulative = 0u64;
        for (i, bucket_count) in self.counts.iter().enumerate() {
            let next = cumulative + bucket_count;
            if (next as f64) >= target && *bucket_count > 0 {
                // The open-ended bucket above the last bound has no upper edge
                // to interpolate toward.
                let Some(&upper) = self.bounds.get(i) else {
                    return Some(
                        self.max.unwrap_or_else(|| {
                            self.bounds.last().copied().unwrap_or(f64::INFINITY)
                        }),
                    );
                };
                let lower = if i == 0 {
                    // Below the first bound, fall back to 0 for the usual case
                    // of a non-negative measurement (latency, size, count).
                    self.min.unwrap_or(0.0).min(upper)
                } else {
                    self.bounds[i - 1]
                };
                let within = (target - cumulative as f64) / *bucket_count as f64;
                return Some(lower + (upper - lower) * within.clamp(0.0, 1.0));
            }
            cumulative = next;
        }
        // Every bucket was empty except beyond the target; report the top edge.
        self.max.or_else(|| self.bounds.last().copied())
    }

    /// Mean of the observed values, or `None` for an empty histogram.
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }

    /// Add `other`'s observations into this histogram.
    ///
    /// Only histograms sharing a bucket layout can be combined — adding counts
    /// across different bounds would attribute observations to the wrong
    /// ranges — so a mismatch is refused rather than approximated.
    pub fn merge(&mut self, other: &HistogramBuckets) -> bool {
        if self.bounds != other.bounds || !self.is_well_formed() || !other.is_well_formed() {
            return false;
        }
        for (slot, add) in self.counts.iter_mut().zip(&other.counts) {
            *slot += add;
        }
        self.count += other.count;
        self.sum += other.sum;
        self.min = match (self.min, other.min) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MetricType {
    Gauge,
    Sum,
    Histogram,
    Summary,
    Unknown,
}

impl std::fmt::Display for MetricType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetricType::Gauge => write!(f, "gauge"),
            MetricType::Sum => write!(f, "sum"),
            MetricType::Histogram => write!(f, "histogram"),
            MetricType::Summary => write!(f, "summary"),
            MetricType::Unknown => write!(f, "unknown"),
        }
    }
}

impl MetricType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "gauge" => MetricType::Gauge,
            "sum" => MetricType::Sum,
            "histogram" => MetricType::Histogram,
            "summary" => MetricType::Summary,
            _ => MetricType::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricQuery {
    pub service: Option<String>,
    pub name: Option<String>,
    pub metric_type: Option<String>,
    pub last_seconds: Option<i64>,
    pub limit: Option<u32>,
    /// Restrict to one tenant; set by the API layer, not the caller.
    #[serde(default)]
    pub tenant: Option<String>,
}

// ── Summary models ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryReport {
    pub window_seconds: i64,
    pub service_filter: Option<String>,
    pub traces: TraceSummary,
    pub top_services: Vec<ServiceSummary>,
    pub top_error_operations: Vec<ErrorOperation>,
    pub logs: LogSummary,
    pub metrics: MetricSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceSummary {
    pub span_count: i64,
    pub trace_count: i64,
    pub error_count: i64,
    pub error_rate: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceSummary {
    pub service: String,
    pub span_count: i64,
    pub error_rate: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorOperation {
    pub service: String,
    pub operation: String,
    pub error_count: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogSummary {
    pub total: i64,
    pub error: i64,
    pub warn: i64,
    pub info: i64,
    pub debug: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricSummary {
    pub point_count: i64,
    pub unique_names: i64,
}

// ── Anomaly models ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyReport {
    pub current_seconds: i64,
    pub baseline_seconds: i64,
    pub service_filter: Option<String>,
    pub anomalies: Vec<Anomaly>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anomaly {
    pub service: String,
    pub kind: String,
    pub severity: String,
    pub current: f64,
    pub baseline: f64,
    pub delta: f64,
    pub description: String,
}

// ── Correlate models ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelateReport {
    pub trace_id: String,
    pub span_count: usize,
    pub services: Vec<String>,
    pub start_time: String,
    pub end_time: String,
    pub duration_ms: f64,
    pub error_count: i64,
    pub logs: Vec<LogRecord>,
    pub metrics: Vec<MetricPoint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQuery {
    pub service: Option<String>,
    pub severity: Option<String>,
    pub body_contains: Option<String>,
    pub trace_id: Option<String>,
    pub last_seconds: Option<i64>,
    pub limit: Option<u32>,
    /// Restrict to one tenant; set by the API layer, not the caller.
    #[serde(default)]
    pub tenant: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A latency histogram: 10 observations spread across four buckets with
    /// bounds at 10/50/100ms, plus an open-ended bucket above 100ms.
    fn latency_histogram() -> HistogramBuckets {
        HistogramBuckets {
            bounds: vec![10.0, 50.0, 100.0],
            //          <=10  <=50  <=100  >100
            counts: vec![2, 5, 2, 1],
            count: 10,
            sum: 420.0,
            min: Some(3.0),
            max: Some(250.0),
            temporality: Temporality::Cumulative,
        }
    }

    #[test]
    fn quantile_interpolates_within_the_containing_bucket() {
        let h = latency_histogram();
        // p50 → the 5th observation, which falls in the (10, 50] bucket.
        // Two observations precede it, so it is 3/5 of the way through a
        // bucket spanning 40ms: 10 + 40*0.6 = 34.
        let p50 = h.quantile(0.5).unwrap();
        assert!((p50 - 34.0).abs() < 1e-9, "p50 was {p50}");

        // p90 → the 9th observation, at the top of the (50, 100] bucket.
        let p90 = h.quantile(0.9).unwrap();
        assert!((p90 - 100.0).abs() < 1e-9, "p90 was {p90}");
    }

    #[test]
    fn quantile_in_the_open_ended_bucket_reports_the_observed_max() {
        let h = latency_histogram();
        // The single observation above 100ms has no upper bound to interpolate
        // toward, so the producer-reported max is the best available answer.
        assert_eq!(h.quantile(1.0), Some(250.0));
    }

    #[test]
    fn quantile_rejects_empty_malformed_and_out_of_range_input() {
        let mut empty = latency_histogram();
        empty.counts = vec![0, 0, 0, 0];
        empty.count = 0;
        assert_eq!(empty.quantile(0.95), None);

        let mut malformed = latency_histogram();
        malformed.counts.pop();
        assert!(!malformed.is_well_formed());
        assert_eq!(malformed.quantile(0.5), None);

        assert_eq!(latency_histogram().quantile(1.5), None);
    }

    #[test]
    fn mean_uses_the_reported_sum_and_count() {
        assert_eq!(latency_histogram().mean(), Some(42.0));
    }

    #[test]
    fn merging_adds_counts_across_a_shared_bucket_layout() {
        let mut a = latency_histogram();
        let b = latency_histogram();
        assert!(a.merge(&b));
        assert_eq!(a.counts, vec![4, 10, 4, 2]);
        assert_eq!(a.count, 20);
        assert_eq!(a.sum, 840.0);
        assert_eq!(a.min, Some(3.0));
        assert_eq!(a.max, Some(250.0));
    }

    #[test]
    fn merging_refuses_mismatched_bucket_layouts() {
        let mut a = latency_histogram();
        let mut b = latency_histogram();
        b.bounds = vec![5.0, 25.0, 250.0];
        // Adding these counts would attribute observations to the wrong ranges.
        assert!(!a.merge(&b));
        assert_eq!(a.counts, vec![2, 5, 2, 1], "left side must be untouched");
    }

    #[test]
    fn histogram_survives_a_json_round_trip() {
        let h = latency_histogram();
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<HistogramBuckets>(&json).unwrap(), h);
    }

    #[test]
    fn metric_points_without_histograms_deserialize_from_older_records() {
        // The hot tier and WAL hold JSON, so points written before buckets were
        // retained must still load.
        let legacy = r#"{
            "timestamp": "2026-01-01T00:00:00Z",
            "service": "api",
            "name": "http.duration",
            "metric_type": "histogram",
            "value": 1.5,
            "unit": "ms",
            "attributes": {}
        }"#;
        let point: MetricPoint = serde_json::from_str(legacy).unwrap();
        assert!(point.histogram.is_none());
    }
}
