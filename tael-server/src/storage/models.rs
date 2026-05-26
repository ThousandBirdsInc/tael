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
    /// Equality filters on span attributes. Each entry is ANDed.
    /// Keys with characters outside `[A-Za-z0-9._\-:/]` are rejected at the storage layer.
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
    /// Full-text query over LLM prompt/completion payloads (Tantivy syntax).
    /// Only honored by the `tael-backend` storage engine; ignored by DuckDB
    /// (which doesn't retain payload text).
    #[serde(default)]
    pub text: Option<String>,
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
}
