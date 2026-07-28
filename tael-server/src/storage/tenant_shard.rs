//! `TenantShardedStore` — physical per-tenant storage isolation
//! (`docs/tael-server-scaling-ha.md` §3 "tenant as shard key", roadmap D1's
//! residual).
//!
//! The plain multi-tenant mode (`TAEL_MULTI_TENANT=1`) is *authorization*:
//! one engine holds every tenant's rows and the query layer filters. This
//! store is *isolation*: the tenant is the top-level shard key of the storage
//! layout itself. Each tenant gets its own complete `TaelBackend` — WAL
//! namespace, LSM hot tier, Parquet cold tier, text index, and comments file —
//! under `<data_dir>/tenants/<tenant>/`, so no storage path is shared between
//! tenants and nothing that bypasses the query layer can cross them.
//!
//! Routing mirrors [`FanoutStore`](super::FanoutStore), with the tenant in
//! place of `hash(trace_id)`:
//!
//! - **Writes** split by each record's stamped `tael.tenant` attribute and go
//!   to the owning tenant's engine (ingest stamps the attribute from the
//!   authenticated principal, so a client cannot write into another tenant by
//!   forging it).
//! - **Scoped reads** (`query.tenant` set by the API layer) go only to that
//!   tenant's engine — other tenants' data is not merely filtered out, it is
//!   never opened.
//! - **Unscoped reads** (admin, or the trace-addressed lookups that carry no
//!   tenant) fan out across the open tenant engines and merge, reusing the
//!   fan-out layer's aggregation.
//!
//! WAL replication composes: each tenant engine ships its own framed records
//! to the same standbys, and a standby running this store routes each shipped
//! record back to the owning tenant's engine by the stamped attribute.
//!
//! **Shared on purpose:** the payload blob store stays process-wide. Blobs are
//! content-addressed (`sha256(content)`), so cross-tenant dedup of identical
//! system prompts is free and a key is only reachable by knowing the content's
//! hash; blob GC unions live hashes across every tenant engine before
//! sweeping.
//!
//! Enabling isolation on a server with pre-isolation data does not migrate it:
//! rows written before the flag live in the root engine's directories and are
//! invisible to the tenant engines. The server warns at startup when it sees
//! that layout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::models::{
    AnomalyReport, CorrelateReport, LogQuery, LogRecord, MetricPoint, MetricQuery, MetricRollup,
    ServiceInfo, Span, SummaryReport, TraceComment, TraceQuery,
};
use super::{
    DynObjectBackend, SearchIndex, Store, TaelBackend, WalSink,
    backend::WalRecord,
    fanout::{merge_anomalies, merge_services, merge_summaries},
};
use crate::retention::RetentionCutoffs;
use crate::tenancy;

/// Builds the cold-tier object backend for one tenant, or `None` for the
/// engine's default (`<tenant_dir>/cold`). This is how object-store cold tiers
/// stay isolated too: the factory appends a per-tenant prefix to the bucket.
pub type ColdBackendFactory = Box<dyn Fn(&str) -> Result<Option<DynObjectBackend>> + Send + Sync>;

pub struct TenantShardedStore {
    data_dir: PathBuf,
    /// Prefix of each tenant engine's process-global walrus WAL key
    /// (`<prefix>@<tenant>`). The server default is `tael-backend`; tests give
    /// each store instance its own so two stores can coexist in one process.
    wal_prefix: String,
    /// Open tenant engines, keyed by tenant name. Engines open lazily on first
    /// write and at startup for every tenant directory found on disk.
    tenants: RwLock<HashMap<String, Arc<TaelBackend>>>,
    /// WAL replication sinks handed to every tenant engine (empty = no
    /// replication, the single-node default).
    sinks: Vec<Arc<dyn WalSink>>,
    required_acks: Option<usize>,
    cold_factory: ColdBackendFactory,
}

impl TenantShardedStore {
    /// Open the store rooted at `data_dir`, re-opening every tenant engine
    /// that already exists on disk so reads see all tenants from the start.
    ///
    /// `cold_factory` builds the per-tenant cold backend; pass `None` for the
    /// local default, which honors `TAEL_COLD_DIR` by giving each tenant a
    /// `tenants/<tenant>` subtree of it.
    pub fn open(
        data_dir: &str,
        sinks: Vec<Arc<dyn WalSink>>,
        required_acks: Option<usize>,
        cold_factory: Option<ColdBackendFactory>,
    ) -> Result<Self> {
        Self::open_with_wal_prefix(data_dir, "tael-backend", sinks, required_acks, cold_factory)
    }

    /// Like [`Self::open`] with an explicit WAL-key prefix, so tests can run
    /// several isolated stores in one process (walrus keys are process-global).
    pub fn open_with_wal_prefix(
        data_dir: &str,
        wal_prefix: &str,
        sinks: Vec<Arc<dyn WalSink>>,
        required_acks: Option<usize>,
        cold_factory: Option<ColdBackendFactory>,
    ) -> Result<Self> {
        let cold_factory = cold_factory.unwrap_or_else(default_cold_factory);
        let store = Self {
            data_dir: PathBuf::from(data_dir),
            wal_prefix: wal_prefix.to_string(),
            tenants: RwLock::new(HashMap::new()),
            sinks,
            required_acks,
            cold_factory,
        };
        for tenant in store.tenants_on_disk()? {
            store.backend_for(&tenant)?;
        }
        Ok(store)
    }

    /// Tenants with a directory under `<data_dir>/tenants/`.
    fn tenants_on_disk(&self) -> Result<Vec<String>> {
        let root = self.data_dir.join("tenants");
        let mut out = Vec::new();
        if !root.exists() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&root)
            .with_context(|| format!("listing tenant dirs in {}", root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(decode_tenant_dir(name));
            }
        }
        out.sort();
        Ok(out)
    }

    /// The engine owning `tenant`, opened on first use.
    pub fn backend_for(&self, tenant: &str) -> Result<Arc<TaelBackend>> {
        if let Some(b) = self.tenants.read().unwrap().get(tenant) {
            return Ok(Arc::clone(b));
        }
        let mut tenants = self.tenants.write().unwrap();
        // Double-checked: another writer may have opened it between locks.
        if let Some(b) = tenants.get(tenant) {
            return Ok(Arc::clone(b));
        }
        let dir = self.tenant_dir(tenant);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating tenant dir {}", dir.display()))?;
        let dir_str = dir
            .to_str()
            .with_context(|| format!("non-UTF-8 tenant dir {}", dir.display()))?;
        let comments = Box::new(super::JsonlComments::open(dir_str)?);
        let backend = Arc::new(TaelBackend::with_components(
            dir_str,
            &format!("{}@{}", self.wal_prefix, encode_tenant_dir(tenant)),
            self.sinks.clone(),
            self.required_acks,
            (self.cold_factory)(tenant)?,
            comments,
        )?);
        tenants.insert(tenant.to_string(), Arc::clone(&backend));
        tracing::info!(tenant, dir = %dir.display(), "opened tenant engine");
        Ok(backend)
    }

    /// The engine owning `tenant`, only if it already exists — a read for a
    /// tenant that never wrote anything must return empty, not create disk
    /// state.
    fn backend_if_open(&self, tenant: &str) -> Option<Arc<TaelBackend>> {
        self.tenants.read().unwrap().get(tenant).cloned()
    }

    /// Every open tenant engine, for fan-out reads and maintenance.
    pub fn backends(&self) -> Vec<(String, Arc<TaelBackend>)> {
        let mut v: Vec<(String, Arc<TaelBackend>)> = self
            .tenants
            .read()
            .unwrap()
            .iter()
            .map(|(t, b)| (t.clone(), Arc::clone(b)))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// The text index a batch written by `tenant` must be indexed into (the
    /// tenant engine's own index, so `--text` queries routed to that engine
    /// find it).
    pub fn search_index_for(&self, tenant: &str) -> Result<Arc<SearchIndex>> {
        Ok(self.backend_for(tenant)?.search_index())
    }

    fn tenant_dir(&self, tenant: &str) -> PathBuf {
        self.data_dir
            .join("tenants")
            .join(encode_tenant_dir(tenant))
    }

    // ── Maintenance (driven by the server's compactor task) ─────────

    pub fn compact_spans(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let mut n = 0;
        for (_, b) in self.backends() {
            n += b.compact_spans(cutoff)?;
        }
        Ok(n)
    }

    pub fn compact_logs_metrics(&self, cutoff: chrono::DateTime<chrono::Utc>) -> Result<usize> {
        let mut n = 0;
        for (_, b) in self.backends() {
            n += b.compact_logs_metrics(cutoff)?;
        }
        Ok(n)
    }

    pub fn enforce_retention(&self, cutoffs: &RetentionCutoffs) -> Result<usize> {
        let mut n = 0;
        for (_, b) in self.backends() {
            n += b.enforce_retention(cutoffs)?;
        }
        Ok(n)
    }

    // ── Read routing helpers ────────────────────────────────────────

    /// Scoped → that one tenant's engine (if it exists); unscoped → all.
    fn read_targets(&self, tenant: Option<&str>) -> Vec<Arc<TaelBackend>> {
        match tenant {
            Some(t) => self.backend_if_open(t).into_iter().collect(),
            None => self.backends().into_iter().map(|(_, b)| b).collect(),
        }
    }

    /// Run `f` over every open tenant engine, tolerating per-tenant failures
    /// (same partial-availability contract as the shard fan-out: error only
    /// when everything failed).
    fn fan_out<T>(&self, op: &str, f: impl Fn(&TaelBackend) -> Result<T>) -> Result<Vec<T>> {
        let backends = self.backends();
        let mut out = Vec::with_capacity(backends.len());
        let mut last_err = None;
        for (tenant, b) in &backends {
            match f(b) {
                Ok(v) => out.push(v),
                Err(e) => {
                    tracing::warn!(tenant, op, error = %e, "tenant engine failed; serving partial results");
                    last_err = Some(e);
                }
            }
        }
        if out.is_empty()
            && let Some(e) = last_err
        {
            return Err(e.context(format!("all tenant engines failed for {op}")));
        }
        Ok(out)
    }
}

/// Group records by their stamped owner and hand each group to its tenant's
/// engine. Unstamped records belong to the default tenant (`tenancy::owner_of`).
fn route_writes<T: Clone>(
    store: &TenantShardedStore,
    items: &[T],
    owner: impl Fn(&T) -> &str,
    insert: impl Fn(&TaelBackend, &[T]) -> Result<()>,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let mut groups: HashMap<&str, Vec<T>> = HashMap::new();
    for item in items {
        groups.entry(owner(item)).or_default().push(item.clone());
    }
    for (tenant, batch) in groups {
        insert(store.backend_for(tenant)?.as_ref(), &batch)?;
    }
    Ok(())
}

impl Store for TenantShardedStore {
    // ── Writes: routed by the stamped tenant attribute ──────────────
    fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        route_writes(
            self,
            spans,
            |s| tenancy::owner_of(&s.attributes),
            |b, batch| b.insert_spans(batch),
        )
    }

    fn insert_logs(&self, logs: &[LogRecord]) -> Result<()> {
        route_writes(
            self,
            logs,
            |l| tenancy::owner_of(&l.attributes),
            |b, batch| b.insert_logs(batch),
        )
    }

    fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        route_writes(
            self,
            metrics,
            |m| tenancy::owner_of(&m.attributes),
            |b, batch| b.insert_metrics(batch),
        )
    }

    // ── Reads: routed when scoped, fanned out when not ──────────────
    fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let mut all = Vec::new();
        for b in self.read_targets(query.tenant.as_deref()) {
            all.extend(b.query_traces(query)?);
        }
        all.sort_by_key(|s| std::cmp::Reverse(s.start_time));
        all.truncate(limit);
        Ok(all)
    }

    fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        // A trace lives in exactly one tenant's engine; stop at the first hit.
        for (_, b) in self.backends() {
            let spans = b.get_trace(trace_id)?;
            if !spans.is_empty() {
                return Ok(spans);
            }
        }
        Ok(Vec::new())
    }

    fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        let per_tenant = self.fan_out("list_services", |b| b.list_services())?;
        Ok(merge_services(per_tenant))
    }

    fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let mut all = Vec::new();
        for b in self.read_targets(query.tenant.as_deref()) {
            all.extend(b.query_logs(query)?);
        }
        all.sort_by_key(|l| std::cmp::Reverse(l.timestamp));
        all.truncate(limit);
        Ok(all)
    }

    fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let limit = query.limit.unwrap_or(500) as usize;
        let mut all = Vec::new();
        for b in self.read_targets(query.tenant.as_deref()) {
            all.extend(b.query_metrics(query)?);
        }
        all.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        all.truncate(limit);
        Ok(all)
    }

    // ── Comments: routed to the trace's owning tenant ───────────────
    fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        // Comments attach to traces, so they live with the trace's engine.
        // A comment on a trace nobody stored lands in the default tenant —
        // same behavior as the single-engine store, which accepts it too.
        for (_, b) in self.backends() {
            if !b.get_trace(trace_id)?.is_empty() {
                return b.add_comment(trace_id, span_id, author, body);
            }
        }
        self.backend_for(tenancy::DEFAULT_TENANT)?
            .add_comment(trace_id, span_id, author, body)
    }

    fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        let mut all = Vec::new();
        for (_, b) in self.backends() {
            all.extend(b.get_comments(trace_id)?);
        }
        all.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(all)
    }

    fn list_comments(&self, limit: usize) -> Result<Vec<TraceComment>> {
        let per_tenant = self.fan_out("list_comments", |b| b.list_comments(limit))?;
        let mut all: Vec<TraceComment> = per_tenant.into_iter().flatten().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all.truncate(limit);
        Ok(all)
    }

    // ── Cross-signal analytics: fan out + re-aggregate ──────────────
    fn query_summary(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport> {
        let per_tenant =
            self.fan_out("query_summary", |b| b.query_summary(last_seconds, service))?;
        Ok(merge_summaries(per_tenant, last_seconds, service))
    }

    fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        let per_tenant = self.fan_out("query_anomalies", |b| {
            b.query_anomalies(current_seconds, baseline_seconds, service)
        })?;
        Ok(merge_anomalies(
            per_tenant,
            current_seconds,
            baseline_seconds,
            service,
        ))
    }

    fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        for (_, b) in self.backends() {
            if let Some(r) = b.query_correlate(trace_id)? {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    fn query_sql(&self, _sql: &str) -> Result<Vec<Value>> {
        // Arbitrary SQL cannot be scoped to one tenant or merged soundly
        // across engines — the same reasoning as the shard fan-out's (c).
        bail!(
            "SQL is not available under tenant isolation: each tenant's data lives in its own \
             engine and arbitrary SQL can neither be scoped nor merged across them. Use the \
             structured query commands, which are tenant-scoped."
        );
    }

    fn query_metric_rollups(
        &self,
        name: Option<&str>,
        service: Option<&str>,
        last_seconds: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MetricRollup>> {
        let per_tenant = self.fan_out("query_metric_rollups", |b| {
            b.query_metric_rollups(name, service, last_seconds, limit)
        })?;
        let mut all: Vec<MetricRollup> = per_tenant.into_iter().flatten().collect();
        all.sort_by_key(|r| std::cmp::Reverse(r.bucket_start));
        all.truncate(limit);
        Ok(all)
    }

    fn explain_traces(&self, query: &TraceQuery) -> Result<Value> {
        match query.tenant.as_deref() {
            Some(t) => match self.backend_if_open(t) {
                Some(b) => b.explain_traces(query),
                None => Ok(serde_json::json!({
                    "supported": true,
                    "engine": "tael-backend (tenant-isolated)",
                    "access_path": "no_such_tenant",
                    "rows_scanned": 0,
                    "rows_returned": 0,
                })),
            },
            None => {
                let mut per_tenant = serde_json::Map::new();
                for (tenant, b) in self.backends() {
                    per_tenant.insert(tenant, b.explain_traces(query)?);
                }
                Ok(serde_json::json!({
                    "supported": true,
                    "engine": "tael-backend (tenant-isolated)",
                    "access_path": "tenant_fan_out",
                    "tenants": per_tenant,
                }))
            }
        }
    }

    // ── Lifecycle / replication ─────────────────────────────────────
    fn flush(&self) -> Result<()> {
        for (_, b) in self.backends() {
            b.flush()?;
        }
        Ok(())
    }

    fn collect_live_blob_hashes(&self) -> Result<std::collections::HashSet<String>> {
        // The blob store is shared across tenants, so its GC live set is the
        // union of every tenant engine's references.
        let mut live = std::collections::HashSet::new();
        for (_, b) in self.backends() {
            live.extend(b.collect_live_blob_hashes()?);
        }
        Ok(live)
    }

    /// Standby entrypoint under isolation: a shipped record is decoded and
    /// routed to the owning tenant's engine by the stamped attribute, exactly
    /// like a live write (a standby engine has no sinks of its own, so
    /// `insert_*` is the same append→apply discipline as `apply_framed_wal`).
    fn apply_framed_wal(&self, framed: &[u8]) -> Result<()> {
        match WalRecord::decode(framed)? {
            WalRecord::Spans(s) => self.insert_spans(&s),
            WalRecord::Logs(l) => self.insert_logs(&l),
            WalRecord::Metrics(m) => self.insert_metrics(&m),
        }
    }
}

/// The filesystem/key component a tenant name maps to — exposed so the server
/// can build per-tenant object-store prefixes with the same encoding the
/// on-disk layout uses.
pub fn tenant_dir_component(tenant: &str) -> String {
    encode_tenant_dir(tenant)
}

/// Encode a tenant name into a filesystem- and WAL-key-safe directory name.
/// Alphanumerics, `-`, `_`, and `.` pass through; everything else (including
/// `/`, which would escape the tenants root) percent-encodes. `..` is encoded
/// too, so no tenant name can traverse upward.
fn encode_tenant_dir(tenant: &str) -> String {
    let plain = tenant
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain && !tenant.is_empty() && tenant != "." && tenant != ".." {
        return tenant.to_string();
    }
    let mut out = String::with_capacity(tenant.len() * 3);
    for byte in tenant.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    if out.is_empty() {
        // The fail-closed empty scope: give it a real directory name so even
        // a misconfigured write cannot land in the tenants root itself.
        out.push_str("%00empty");
    }
    out
}

/// Invert [`encode_tenant_dir`] for startup discovery. Unknown escapes decode
/// as-is rather than erroring — a hand-made directory should still open.
fn decode_tenant_dir(dir: &str) -> String {
    if dir == "%00empty" {
        return String::new();
    }
    let bytes = dir.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let Some(hex) = dir.get(i + 1..i + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The default per-tenant cold backend: honor `TAEL_COLD_DIR` by giving each
/// tenant its own subtree of it (each engine would otherwise resolve the env
/// var to the *same* directory, silently un-isolating the cold tier); without
/// the override, the engine default (`<tenant_dir>/cold`) is already private.
fn default_cold_factory() -> ColdBackendFactory {
    Box::new(|tenant: &str| match std::env::var("TAEL_COLD_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            let root = Path::new(&dir)
                .join("tenants")
                .join(encode_tenant_dir(tenant));
            Ok(Some(
                Arc::new(super::FsBackend::new(root)?) as DynObjectBackend
            ))
        }
        _ => Ok(None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{SpanKind, SpanStatus};
    use crate::tenancy::TENANT_ATTRIBUTE;
    use chrono::Utc;

    /// Removes every walrus namespace under a per-test WAL-key prefix on
    /// drop. Walrus keys are process-global and it sanitizes characters in
    /// directory names, so cleanup matches on the prefix instead of trying to
    /// reproduce exact names.
    struct NsGuard(String);
    impl Drop for NsGuard {
        fn drop(&mut self) {
            let Ok(entries) = std::fs::read_dir("wal_files") else {
                return;
            };
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(&self.0))
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    /// A store with a unique WAL prefix, so tests neither collide with each
    /// other in-process nor replay a previous run's WAL.
    fn open(dir: &tempfile::TempDir) -> (TenantShardedStore, NsGuard) {
        open_prefixed(dir, &format!("tael-test-ts-{}", uuid::Uuid::new_v4()))
    }

    fn open_prefixed(dir: &tempfile::TempDir, prefix: &str) -> (TenantShardedStore, NsGuard) {
        let store = TenantShardedStore::open_with_wal_prefix(
            dir.path().to_str().unwrap(),
            prefix,
            Vec::new(),
            None,
            None,
        )
        .unwrap();
        (store, NsGuard(prefix.to_string()))
    }

    fn span(trace: &str, sid: &str, tenant: Option<&str>) -> Span {
        let now = Utc::now();
        let mut attributes = HashMap::new();
        if let Some(t) = tenant {
            attributes.insert(TENANT_ATTRIBUTE.to_string(), t.to_string());
        }
        Span {
            trace_id: trace.into(),
            span_id: sid.into(),
            parent_span_id: None,
            service: "api".into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms: 1.0,
            status: SpanStatus::Ok,
            attributes,
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        }
    }

    #[test]
    fn writes_land_in_physically_separate_tenant_engines() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _g) = open(&dir);
        store
            .insert_spans(&[
                span("ta1", "s1", Some("team-a")),
                span("tb1", "s2", Some("team-b")),
                span("tleg", "s3", None),
            ])
            .unwrap();

        // Isolation is a property of the disk layout, not of query filtering:
        // each tenant's rows are under its own directory tree.
        for t in ["team-a", "team-b", "default"] {
            assert!(
                dir.path().join("tenants").join(t).join("hot").exists(),
                "tenant {t} should have its own hot tier"
            );
        }

        // And each engine holds only its own tenant's data.
        let a = store.backend_for("team-a").unwrap();
        assert_eq!(a.get_trace("ta1").unwrap().len(), 1);
        assert!(a.get_trace("tb1").unwrap().is_empty());
        assert!(a.get_trace("tleg").unwrap().is_empty());
    }

    #[test]
    fn scoped_reads_touch_only_the_scoped_tenants_engine() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _g) = open(&dir);
        store
            .insert_spans(&[
                span("ta1", "s1", Some("team-a")),
                span("ta2", "s2", Some("team-a")),
                span("tb1", "s3", Some("team-b")),
            ])
            .unwrap();

        let scoped = |tenant: Option<&str>| {
            store
                .query_traces(&TraceQuery {
                    limit: Some(100),
                    tenant: tenant.map(str::to_string),
                    ..Default::default()
                })
                .unwrap()
        };
        assert_eq!(scoped(Some("team-a")).len(), 2);
        assert_eq!(scoped(Some("team-b")).len(), 1);
        // A tenant that never wrote gets nothing — and no directory appears.
        assert!(scoped(Some("ghost")).is_empty());
        assert!(!dir.path().join("tenants").join("ghost").exists());
        // Unscoped (admin) fans out across every tenant.
        assert_eq!(scoped(None).len(), 3);
    }

    #[test]
    fn traces_comments_and_services_resolve_across_tenants() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _g) = open(&dir);
        store
            .insert_spans(&[
                span("ta1", "s1", Some("team-a")),
                span("tb1", "s2", Some("team-b")),
            ])
            .unwrap();

        // Trace-addressed lookups find the owning engine.
        assert_eq!(store.get_trace("tb1").unwrap().len(), 1);
        assert!(store.get_trace("missing").unwrap().is_empty());

        // A comment lands with the trace's engine, not in a shared file.
        store.add_comment("tb1", None, "agent", "suspect").unwrap();
        assert_eq!(store.get_comments("tb1").unwrap().len(), 1);
        let b = store.backend_for("team-b").unwrap();
        assert_eq!(b.get_comments("tb1").unwrap().len(), 1);
        let a = store.backend_for("team-a").unwrap();
        assert!(a.get_comments("tb1").unwrap().is_empty());

        // Aggregates merge across engines.
        let services = store.list_services().unwrap();
        let api = services.iter().find(|s| s.name == "api").unwrap();
        assert_eq!(api.span_count, 2);
        let summary = store.query_summary(3_600, None).unwrap();
        assert_eq!(summary.traces.span_count, 2);
    }

    #[test]
    fn existing_tenants_reopen_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = format!("tael-test-ts-{}", uuid::Uuid::new_v4());
        {
            let (store, _g) = open_prefixed(&dir, &prefix);
            store
                .insert_spans(&[span("ta1", "s1", Some("team-a"))])
                .unwrap();
            store.flush().unwrap();
        }
        let (store, _g) = open_prefixed(&dir, &prefix);
        assert_eq!(
            store.get_trace("ta1").unwrap().len(),
            1,
            "startup discovery must re-open tenant engines found on disk"
        );
    }

    #[test]
    fn hostile_tenant_names_cannot_escape_the_tenants_root() {
        for hostile in ["../evil", "a/b", "..", ".", "", "a\\b", "%2e%2e"] {
            let encoded = encode_tenant_dir(hostile);
            assert!(
                !encoded.contains('/') && !encoded.contains('\\'),
                "{hostile:?} encoded to {encoded:?}"
            );
            assert!(encoded != ".." && encoded != "." && !encoded.is_empty());
            assert_eq!(
                decode_tenant_dir(&encoded),
                hostile,
                "encoding must round-trip {hostile:?}"
            );
        }
        // Ordinary names stay readable on disk.
        assert_eq!(encode_tenant_dir("team-a"), "team-a");
        assert_eq!(decode_tenant_dir("team-a"), "team-a");
    }

    #[test]
    fn sql_is_refused_under_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _g) = open(&dir);
        let err = store.query_sql("SELECT 1").unwrap_err().to_string();
        assert!(err.contains("tenant isolation"), "{err}");
    }

    #[test]
    fn shipped_wal_records_route_to_the_owning_tenant_on_a_standby() {
        // Leader side: an isolated store whose tenant engines ship to a
        // standby that is itself tenant-isolated. The shipped frame carries
        // the stamped attribute, so the standby re-routes it. WAL keys are
        // process-global, so leader and standby get distinct prefixes.
        let standby_dir = tempfile::tempdir().unwrap();
        let standby_prefix = format!("tael-test-standby-{}", uuid::Uuid::new_v4());
        let _g1 = NsGuard(standby_prefix.clone());
        let standby = Arc::new(
            TenantShardedStore::open_with_wal_prefix(
                standby_dir.path().to_str().unwrap(),
                &standby_prefix,
                Vec::new(),
                None,
                None,
            )
            .unwrap(),
        );

        struct ReplicaSink(Arc<TenantShardedStore>);
        impl WalSink for ReplicaSink {
            fn append_framed(&self, framed: &[u8]) -> Result<()> {
                self.0.apply_framed_wal(framed)
            }
        }

        let leader_dir = tempfile::tempdir().unwrap();
        let leader_prefix = format!("tael-test-leader-{}", uuid::Uuid::new_v4());
        let _g2 = NsGuard(leader_prefix.clone());
        let leader = TenantShardedStore::open_with_wal_prefix(
            leader_dir.path().to_str().unwrap(),
            &leader_prefix,
            vec![Arc::new(ReplicaSink(Arc::clone(&standby)))],
            None, // synchronous: the (one) standby must ack
            None,
        )
        .unwrap();
        leader
            .insert_spans(&[span("ta1", "s1", Some("team-a"))])
            .unwrap();

        assert_eq!(standby.get_trace("ta1").unwrap().len(), 1);
        let b = standby.backend_for("team-a").unwrap();
        assert_eq!(
            b.get_trace("ta1").unwrap().len(),
            1,
            "the standby must hold the record in team-a's engine, not a shared one"
        );
    }
}
