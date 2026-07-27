mod backend;
mod blobs;
mod comments;
#[cfg(feature = "duckdb")]
mod duckdb_store;
mod fanout;
pub mod models;
mod objstore;
mod remote;
mod search;

pub use backend::{TaelBackend, WalSink};
pub use blobs::BlobStore;
pub use comments::{CommentsStore, JsonlComments, open as open_comments};
#[cfg(feature = "duckdb")]
pub use duckdb_store::DuckDbStore;
pub use fanout::FanoutStore;
pub use objstore::{DynObjectBackend, FsBackend, StoreLocation, open_object_backend};
pub use remote::{RemoteStore, RemoteWalSink, WAL_EPOCH_HEADER};
pub use search::SearchIndex;

#[cfg(test)]
pub(crate) mod testing;

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
    /// The most recent comments across ALL traces, newest first. Powers the
    /// reliability-loop scanners (`tael issue list`, `signal trend`, `eval
    /// suite inspect`) on builds without the SQL layer. Default: unsupported —
    /// backends that can enumerate comments override this.
    fn list_comments(&self, _limit: usize) -> Result<Vec<TraceComment>> {
        anyhow::bail!("listing comments across traces is not supported by this storage backend")
    }

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

    /// Read 5-minute downsampled metric aggregates.
    ///
    /// Rollups outlive raw points by design (a year against thirty days), so
    /// they are the only way to answer a long-range trend question once raw
    /// retention has passed. Without a read path they were being written and
    /// expired but never queried, which made the downsampling pure cost.
    ///
    /// Default: unsupported — only the tiered engine keeps rollups.
    fn query_metric_rollups(
        &self,
        _name: Option<&str>,
        _service: Option<&str>,
        _last_seconds: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<models::MetricRollup>> {
        anyhow::bail!("metric rollups are not supported by this storage backend")
    }

    /// Describe how a trace query would execute: which tiers are consulted,
    /// which access path is taken, and how many rows are examined to produce
    /// the result.
    ///
    /// This is the agent's substitute for a query-planner UI. An agent that
    /// gets an unexpectedly empty or slow answer needs to know *why* — a
    /// filter that matched nothing looks identical to a filter the engine
    /// ignored, unless the engine says which it was.
    ///
    /// The default reports that the backend can't introspect itself rather
    /// than inventing plausible-looking numbers.
    fn explain_traces(&self, _query: &TraceQuery) -> Result<serde_json::Value> {
        Ok(serde_json::json!({
            "supported": false,
            "reason": "this storage backend does not report query execution details",
        }))
    }

    // ── Lifecycle / operability (default no-ops) ────────────────────
    /// Readiness probe — `Ok(())` when this store can serve requests. Backs the
    /// REST `/readyz` endpoint (`docs/tael-server-scaling-ha.md` §5.4). The
    /// default is `Ok(())`: an embedded backend that constructed successfully
    /// and holds its file locks is, by definition, ready. Backends that depend
    /// on the network (e.g. [`RemoteStore`], [`FanoutStore`](crate::storage))
    /// override this to probe their dependencies.
    fn health(&self) -> Result<()> {
        Ok(())
    }

    /// Flush durable buffered state ahead of a graceful shutdown. The WAL fsync
    /// on the write path is the real durability boundary, so this is
    /// best-effort: it tightens the hot tier's on-disk state so a restart or
    /// standby replays less WAL (§5.4 "flush the hot tier"). Default is a no-op.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Standby entrypoint for WAL replication: durably accept a framed WAL
    /// record shipped from a leader and bring local state up to it
    /// (`docs/tael-server-scaling-ha.md` §5.1). Backs the
    /// `POST /internal/wal/records` endpoint. Default: rejected — only the
    /// tael-backend engine, which owns a WAL, can act as a standby.
    fn apply_framed_wal(&self, _framed: &[u8]) -> Result<()> {
        anyhow::bail!("this store does not accept WAL replication")
    }
}
