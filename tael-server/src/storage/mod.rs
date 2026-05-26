mod backend;
mod blobs;
mod duckdb_store;
pub mod models;
mod search;

pub use backend::TaelBackend;
pub use blobs::BlobStore;
pub use duckdb_store::DuckDbStore;
pub use search::SearchIndex;

use anyhow::Result;

use models::{
    AnomalyReport, CorrelateReport, LogQuery, LogRecord, MetricPoint, MetricQuery, ServiceInfo,
    Span, SummaryReport, TraceComment, TraceQuery,
};

/// Storage backend for all telemetry signals. The server depends on
/// `Arc<dyn Store>`, so backends (DuckDB today, `TaelBackend` next) are
/// swappable without touching the API, ingest, or query layers.
///
/// Synchronous by design — see `docs/tael-backend-impl-plan.md`, Phase 0.
/// `Send + Sync` so it can be shared as `Arc<dyn Store>` across tokio tasks and
/// held in axum state.
pub trait Store: Send + Sync {
    // ── Spans / traces ──────────────────────────────────────────────
    fn insert_spans(&self, spans: &[Span]) -> Result<()>;
    fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>>;
    fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>>;
    fn list_services(&self) -> Result<Vec<ServiceInfo>>;

    // ── Comments ────────────────────────────────────────────────────
    fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment>;
    fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>>;

    // ── Logs ────────────────────────────────────────────────────────
    fn insert_logs(&self, logs: &[LogRecord]) -> Result<()>;
    fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>>;

    // ── Metrics ─────────────────────────────────────────────────────
    fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()>;
    fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>>;

    // ── Cross-signal analytics ──────────────────────────────────────
    fn query_summary(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport>;
    fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport>;
    fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>>;

    /// Read-only SQL query surface (`SELECT`/`WITH`) over the telemetry tables,
    /// returning rows as JSON objects.
    fn query_sql(&self, sql: &str) -> Result<Vec<serde_json::Value>>;
}
