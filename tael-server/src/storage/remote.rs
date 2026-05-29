//! `RemoteStore` — a [`Store`] that speaks HTTP to another `tael-server`'s REST
//! API. This is the "remote `Store` client" the scaling/HA design calls for
//! (`docs/tael-server-scaling-ha.md` §3, Phase 2): the thin client that lets a
//! [`FanoutStore`](super::FanoutStore) scatter reads across N shard processes
//! without the REST/gRPC/CLI layers above the `Store` trait changing at all.
//!
//! ## Synchronous over blocking HTTP
//!
//! The `Store` trait is synchronous by design (`storage/mod.rs`). We therefore
//! use [`reqwest::blocking`], whose client owns its own runtime on a dedicated
//! thread and parks the caller on a channel — safe to call from inside the
//! server's tokio workers, and consistent with the rest of the engine treating
//! `Store` calls as blocking.
//!
//! ## Read-only
//!
//! There is no typed REST ingest endpoint (ingest is OTLP/gRPC), so the write
//! methods (`insert_*`) return an error. In the sharded topology, writes are
//! routed to the owning shard at the ingest edge (the OTel Collector hashing on
//! `trace_id`), never through this client — see the design's §3 routing layer.
//! Comment writes, which *do* have a REST endpoint, are supported.

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;

use super::Store;
use super::backend::WalSink;
use super::models::{
    AnomalyReport, CorrelateReport, LogQuery, LogRecord, MetricPoint, MetricQuery, ServiceInfo,
    Span, SummaryReport, TraceComment, TraceQuery,
};

/// A [`Store`] backed by another `tael-server`'s REST API over HTTP.
pub struct RemoteStore {
    base_url: String,
    http: Client,
}

impl RemoteStore {
    /// Connect to a `tael-server` REST endpoint, e.g. `http://shard-0:7701`.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::with_timeout(base_url, Duration::from_secs(30))
    }

    /// Like [`Self::new`] with an explicit per-request timeout.
    pub fn with_timeout(base_url: impl Into<String>, timeout: Duration) -> Result<Self> {
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .context("building RemoteStore HTTP client")?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    fn send_get(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<reqwest::blocking::Response> {
        self.http
            .get(format!("{}{path}", self.base_url))
            .query(params)
            .send()
            .with_context(|| format!("GET {path} from {}", self.base_url))
    }

    /// GET expecting a JSON object, surfacing non-2xx as an error.
    fn get_json(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        let resp = self
            .send_get(path, params)?
            .error_for_status()
            .with_context(|| format!("GET {path} from {}", self.base_url))?;
        resp.json::<Value>()
            .with_context(|| format!("decoding {path} response from {}", self.base_url))
    }
}

/// Pull a named field out of a JSON envelope and deserialize it. The REST API
/// wraps payloads, e.g. `{ "spans": [...] }`, `{ "logs": [...] }`.
fn field<T: DeserializeOwned>(mut value: Value, key: &str) -> Result<T> {
    let v = value
        .get_mut(key)
        .map(Value::take)
        .ok_or_else(|| anyhow!("response missing `{key}` field"))?;
    serde_json::from_value(v).with_context(|| format!("deserializing `{key}`"))
}

/// Express a `last_seconds` lower bound as the `last=` query param. The REST
/// layer's `parse_duration_to_seconds` accepts a bare integer as seconds.
fn last_param(params: &mut Vec<(&'static str, String)>, last_seconds: Option<i64>) {
    if let Some(s) = last_seconds {
        params.push(("last", s.to_string()));
    }
}

impl Store for RemoteStore {
    // ── Spans / traces ──────────────────────────────────────────────
    fn insert_spans(&self, _spans: &[Span]) -> Result<()> {
        bail!("RemoteStore is read-only: route span ingest to the owning shard via OTLP");
    }

    fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(ref s) = query.service {
            params.push(("service", s.clone()));
        }
        if let Some(ref o) = query.operation {
            params.push(("operation", o.clone()));
        }
        if let Some(d) = query.min_duration_ms {
            params.push(("min_duration_ms", d.to_string()));
        }
        if let Some(d) = query.max_duration_ms {
            params.push(("max_duration_ms", d.to_string()));
        }
        if let Some(ref s) = query.status {
            params.push(("status", s.clone()));
        }
        last_param(&mut params, query.last_seconds);
        if let Some(l) = query.limit {
            params.push(("limit", l.to_string()));
        }
        for (k, v) in &query.attributes {
            params.push(("attribute", format!("{k}={v}")));
        }
        if let Some(ref t) = query.text {
            params.push(("text", t.clone()));
        }
        let body = self.get_json("/api/v1/traces", &params)?;
        field(body, "spans")
    }

    fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        // 404 == "no such trace" rather than an error; map it to an empty set so
        // fan-out can union shards without a missing shard aborting the whole
        // lookup.
        let resp = self.send_get(&format!("/api/v1/traces/{trace_id}"), &[])?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(Vec::new()),
            s if s.is_success() => {
                let body = resp
                    .json::<Value>()
                    .context("decoding get_trace response")?;
                field(body, "spans")
            }
            s => bail!("get_trace {trace_id} on {}: HTTP {s}", self.base_url),
        }
    }

    fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        let body = self.get_json("/api/v1/services", &[])?;
        field(body, "services")
    }

    // ── Comments ────────────────────────────────────────────────────
    fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        let mut payload = serde_json::json!({ "author": author, "body": body });
        if let Some(s) = span_id {
            payload["span_id"] = serde_json::json!(s);
        }
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/traces/{trace_id}/comments",
                self.base_url
            ))
            .json(&payload)
            .send()
            .with_context(|| format!("POST comment to {}", self.base_url))?
            .error_for_status()
            .with_context(|| format!("POST comment to {}", self.base_url))?
            .json::<Value>()
            .context("decoding add_comment response")?;
        field(resp, "comment")
    }

    fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        let body = self.get_json(&format!("/api/v1/traces/{trace_id}/comments"), &[])?;
        field(body, "comments")
    }

    // ── Logs ────────────────────────────────────────────────────────
    fn insert_logs(&self, _logs: &[LogRecord]) -> Result<()> {
        bail!("RemoteStore is read-only: route log ingest to the owning shard via OTLP");
    }

    fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(ref s) = query.service {
            params.push(("service", s.clone()));
        }
        if let Some(ref s) = query.severity {
            params.push(("severity", s.clone()));
        }
        if let Some(ref b) = query.body_contains {
            params.push(("body_contains", b.clone()));
        }
        if let Some(ref t) = query.trace_id {
            params.push(("trace_id", t.clone()));
        }
        last_param(&mut params, query.last_seconds);
        if let Some(l) = query.limit {
            params.push(("limit", l.to_string()));
        }
        let body = self.get_json("/api/v1/logs", &params)?;
        field(body, "logs")
    }

    // ── Metrics ─────────────────────────────────────────────────────
    fn insert_metrics(&self, _metrics: &[MetricPoint]) -> Result<()> {
        bail!("RemoteStore is read-only: route metric ingest to the owning shard via OTLP");
    }

    fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(ref s) = query.service {
            params.push(("service", s.clone()));
        }
        if let Some(ref n) = query.name {
            params.push(("name", n.clone()));
        }
        if let Some(ref t) = query.metric_type {
            params.push(("metric_type", t.clone()));
        }
        last_param(&mut params, query.last_seconds);
        if let Some(l) = query.limit {
            params.push(("limit", l.to_string()));
        }
        let body = self.get_json("/api/v1/metrics", &params)?;
        field(body, "metrics")
    }

    // ── Cross-signal analytics ──────────────────────────────────────
    fn query_summary(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport> {
        let mut params: Vec<(&str, String)> = vec![("last", last_seconds.to_string())];
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        let body = self.get_json("/api/v1/summary", &params)?;
        serde_json::from_value(body).context("deserializing SummaryReport")
    }

    fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        let mut params: Vec<(&str, String)> = vec![
            ("last", current_seconds.to_string()),
            ("baseline", baseline_seconds.to_string()),
        ];
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        let body = self.get_json("/api/v1/anomalies", &params)?;
        serde_json::from_value(body).context("deserializing AnomalyReport")
    }

    fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        let resp = self.send_get("/api/v1/correlate", &[("trace", trace_id.to_string())])?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => {
                let body = resp
                    .json::<Value>()
                    .context("decoding correlate response")?;
                Ok(Some(
                    serde_json::from_value(body).context("deserializing CorrelateReport")?,
                ))
            }
            s => bail!("correlate {trace_id} on {}: HTTP {s}", self.base_url),
        }
    }

    fn query_sql(&self, sql: &str) -> Result<Vec<Value>> {
        let body = self.get_json("/api/v1/sql", &[("q", sql.to_string())])?;
        field(body, "rows")
    }

    // ── Lifecycle ───────────────────────────────────────────────────
    fn health(&self) -> Result<()> {
        let resp = self
            .send_get("/healthz", &[])?
            .error_for_status()
            .with_context(|| format!("health check on {}", self.base_url))?;
        let _ = resp.text();
        Ok(())
    }
}

/// A [`WalSink`] that ships framed WAL records to a standby `tael-server`'s
/// `POST /internal/wal/records` endpoint over blocking HTTP — the leader→standby
/// transport for WAL replication (`docs/tael-server-scaling-ha.md` §5.1).
/// Blocking, like [`RemoteStore`], because the WAL append path that drives it is
/// synchronous; `append_framed` returns only once the standby has applied the
/// record (the per-record ack that makes replication synchronous).
/// HTTP header carrying the leader's epoch, so the standby can fence out a
/// deposed leader's records (`cluster::EpochFencer`).
pub const WAL_EPOCH_HEADER: &str = "x-tael-wal-epoch";

pub struct RemoteWalSink {
    url: String,
    http: Client,
    /// The leader's current epoch, stamped on each shipped record. `None` when
    /// running without cluster coordination (single leader, no fencing needed).
    epoch: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

impl RemoteWalSink {
    /// Target a standby by its REST base URL (e.g. `http://standby-1:7701`).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::build(base_url, None)
    }

    /// Like [`Self::new`] but stamps the leader's current epoch (from cluster
    /// coordination) on each shipped record for standby fencing.
    pub fn with_epoch(
        base_url: impl Into<String>,
        epoch: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self> {
        Self::build(base_url, Some(epoch))
    }

    fn build(
        base_url: impl Into<String>,
        epoch: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    ) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("building RemoteWalSink HTTP client")?;
        let base = base_url.into().trim_end_matches('/').to_string();
        Ok(Self {
            url: format!("{base}/internal/wal/records"),
            http,
            epoch,
        })
    }
}

impl WalSink for RemoteWalSink {
    fn append_framed(&self, framed: &[u8]) -> Result<()> {
        let mut req = self
            .http
            .post(&self.url)
            .header("content-type", "application/octet-stream");
        if let Some(epoch) = &self.epoch {
            req = req.header(
                WAL_EPOCH_HEADER,
                epoch.load(std::sync::atomic::Ordering::Acquire).to_string(),
            );
        }
        req.body(framed.to_vec())
            .send()
            .with_context(|| format!("shipping WAL record to {}", self.url))?
            .error_for_status()
            .with_context(|| format!("standby {} rejected WAL record", self.url))?;
        Ok(())
    }

    fn name(&self) -> &str {
        &self.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DuckDbStore;
    use crate::storage::models::{SpanKind, SpanStatus};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;

    fn test_span(trace: &str, sid: &str, svc: &str) -> Span {
        let now = Utc::now();
        Span {
            trace_id: trace.into(),
            span_id: sid.into(),
            parent_span_id: None,
            service: svc.into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms: 12.0,
            status: SpanStatus::Ok,
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    /// Boot a real REST server (DuckDB-backed) on an ephemeral port in its own
    /// runtime thread, seeded with `spans`, and return its address. Running the
    /// server off-thread lets the blocking `RemoteStore` calls in the test
    /// thread proceed without nesting/deadlocking a runtime.
    fn serve_with(spans: Vec<Span>) -> SocketAddr {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Keep the tempdir alive for the server thread's lifetime.
            let _dir = dir;
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let store: Arc<dyn Store> = Arc::new(DuckDbStore::new(&path).unwrap());
                store.insert_spans(&spans).unwrap();
                let blobs = Arc::new(crate::storage::BlobStore::new(&path).unwrap());
                let bus = Arc::new(crate::span_bus::SpanBus::new().unwrap());
                let log_bus = Arc::new(crate::log_bus::LogBus::new().unwrap());
                let app = crate::api::rest::router(store, blobs, bus, log_bus, None);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        rx.recv().unwrap()
    }

    #[test]
    fn remote_store_roundtrips_reads_over_http() {
        let addr = serve_with(vec![
            test_span("t1", "s1", "api"),
            test_span("t1", "s2", "db"),
            test_span("t2", "s3", "api"),
        ]);
        let remote = RemoteStore::new(format!("http://{addr}")).unwrap();

        remote.health().expect("health");

        let traces = remote.query_traces(&TraceQuery::default()).unwrap();
        assert_eq!(traces.len(), 3, "all spans returned over HTTP");

        let t1 = remote.get_trace("t1").unwrap();
        assert_eq!(t1.len(), 2);
        assert!(t1.iter().all(|s| s.trace_id == "t1"));

        // 404 maps to an empty set, not an error.
        assert!(remote.get_trace("missing").unwrap().is_empty());

        let services = remote.list_services().unwrap();
        assert!(services.iter().any(|s| s.name == "api"));

        // Writes are rejected: ingest must go to the owning shard via OTLP.
        assert!(
            remote
                .insert_spans(&[test_span("t3", "s4", "api")])
                .is_err()
        );
    }

    /// Serve an existing store over a fresh REST server thread (the store is
    /// built by the caller so it can also hold a handle to it).
    fn serve_store(store: Arc<dyn Store>, data_dir: String) -> SocketAddr {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async move {
                let blobs = Arc::new(crate::storage::BlobStore::new(&data_dir).unwrap());
                let bus = Arc::new(crate::span_bus::SpanBus::new().unwrap());
                let log_bus = Arc::new(crate::log_bus::LogBus::new().unwrap());
                let app = crate::api::rest::router(store, blobs, bus, log_bus, None);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        rx.recv().unwrap()
    }

    /// Removes a walrus namespace dir on drop so test runs don't accrete state.
    struct WalKeyGuard(String);
    impl Drop for WalKeyGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.0));
        }
    }

    #[test]
    fn wal_replication_ships_leader_writes_to_standby_over_http() {
        use crate::storage::TaelBackend;

        // A standby tael-backend, served over HTTP so its
        // /internal/wal/records endpoint is reachable. We keep a handle to
        // assert its state directly.
        let standby_dir = tempfile::tempdir().unwrap();
        let standby_path = standby_dir.path().to_str().unwrap().to_string();
        let standby_key = format!("tael-test-standby-{}", uuid::Uuid::new_v4());
        let _sg = WalKeyGuard(standby_key.clone());
        let standby = Arc::new(TaelBackend::with_wal_key(&standby_path, &standby_key).unwrap());
        let standby_addr = serve_store(Arc::clone(&standby) as Arc<dyn Store>, standby_path);

        // A leader that ships its WAL to the standby over HTTP, synchronously
        // (required_acks = None ⇒ all standbys must ack before a write returns).
        let leader_dir = tempfile::tempdir().unwrap();
        let leader_key = format!("tael-test-leader-{}", uuid::Uuid::new_v4());
        let _lg = WalKeyGuard(leader_key.clone());
        let sink = Arc::new(RemoteWalSink::new(format!("http://{standby_addr}")).unwrap());
        let leader = TaelBackend::with_wal_key_and_sinks(
            leader_dir.path().to_str().unwrap(),
            &leader_key,
            vec![sink],
            None,
        )
        .unwrap();

        // Write to the leader only.
        leader
            .insert_spans(&[
                test_span("t1", "s1", "api"),
                test_span("t1", "s2", "db"),
                test_span("t2", "s3", "api"),
            ])
            .unwrap();

        // The standby received and applied the shipped records over HTTP.
        assert_eq!(standby.get_trace("t1").unwrap().len(), 2);
        assert_eq!(standby.get_trace("t2").unwrap().len(), 1);

        // And it serves them over its own REST API (full path works).
        let via_http = RemoteStore::new(format!("http://{standby_addr}")).unwrap();
        assert_eq!(
            via_http.query_traces(&TraceQuery::default()).unwrap().len(),
            3
        );
    }
}
