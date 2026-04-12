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
    pub body: String,
    pub service: String,
    pub attributes: HashMap<String, String>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQuery {
    pub service: Option<String>,
    pub severity: Option<String>,
    pub body_contains: Option<String>,
    pub trace_id: Option<String>,
    pub last_seconds: Option<i64>,
    pub limit: Option<u32>,
}
