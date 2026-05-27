//! `FanoutStore` — the scatter-gather query layer for the sharded topology
//! (`docs/tael-server-scaling-ha.md` §3, Phase 2). It implements [`Store`] over
//! N shard `Store`s (typically [`RemoteStore`](super::RemoteStore)s, one per
//! `tael-server` shard) so the REST/gRPC/CLI layers above the trait are
//! unchanged — they see one logical store.
//!
//! ## Routing vs. fan-out
//!
//! The shard key is `trace_id` (the design's choice — it keeps `get_trace` and
//! `correlate` local). Operations split two ways:
//!
//! - **Routed** to the single owning shard via `hash(trace_id) % N`: comment
//!   reads/writes and the write path (`insert_*`). A batch is grouped by shard
//!   and each group dispatched to its owner — the routing layer, in code.
//! - **Fanned out** to all shards then merged: the core reads. `query_*`
//!   concatenate and re-limit (each shard already returns newest-first, so a
//!   sort+truncate is a k-way merge); `list_services`/`query_summary`/
//!   `query_anomalies` re-aggregate, because counts sum but rates/averages do
//!   not. `get_trace`/`correlate` fan out for correctness under routing-hash
//!   skew or rebalancing windows (design Open Q #1), short-circuiting once the
//!   owning shard answers.
//!
//! `query_sql` is deliberately **not** distributed (design §3 recommendation
//! (c)): arbitrary SQL over per-shard DuckDB projections can't be merged
//! soundly, so it returns an error pointing at a single-node endpoint.
//!
//! ## Partial availability
//!
//! Fan-out reads tolerate a down shard: results from healthy shards are
//! returned and the failure logged, matching the HA goal that losing one shard
//! degrades rather than fails queries. A read errors only when *every* shard
//! fails. (Routed ops have a single owner, so they surface that owner's error.)

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use anyhow::{Result, bail};
use serde_json::Value;

use super::Store;
use super::models::{
    Anomaly, AnomalyReport, CorrelateReport, ErrorOperation, LogQuery, LogRecord, LogSummary,
    MetricPoint, MetricQuery, MetricSummary, ServiceInfo, ServiceSummary, Span, SummaryReport,
    TraceComment, TraceQuery, TraceSummary,
};

/// A [`Store`] that scatters reads across N shard stores and gathers/merges the
/// results. See the module docs for routing vs. fan-out semantics.
pub struct FanoutStore {
    shards: Vec<Arc<dyn Store>>,
}

/// Deterministic shard selection. `DefaultHasher::new()` is seeded with fixed
/// keys (unlike `RandomState`), so the same key maps to the same shard across
/// processes and restarts — a prerequisite for `get_trace` to find the shard
/// the ingest router wrote to.
fn shard_index(key: &str, n: usize) -> usize {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    (h.finish() % n as u64) as usize
}

impl FanoutStore {
    /// Build a fan-out over the given shards. Requires at least one shard.
    pub fn new(shards: Vec<Arc<dyn Store>>) -> Result<Self> {
        if shards.is_empty() {
            bail!("FanoutStore requires at least one shard");
        }
        Ok(Self { shards })
    }

    fn shard_for(&self, key: &str) -> &Arc<dyn Store> {
        &self.shards[shard_index(key, self.shards.len())]
    }

    /// Run `f` against every shard, collecting successes. Logs and tolerates
    /// per-shard failures; errors only if *all* shards fail.
    fn fan_out<T>(
        &self,
        op: &str,
        f: impl Fn(&Arc<dyn Store>) -> Result<T>,
    ) -> Result<Vec<T>> {
        let mut results = Vec::with_capacity(self.shards.len());
        let mut last_err = None;
        for (i, shard) in self.shards.iter().enumerate() {
            match f(shard) {
                Ok(v) => results.push(v),
                Err(e) => {
                    tracing::warn!(shard = i, op, error = %e, "shard failed; serving partial results");
                    last_err = Some(e);
                }
            }
        }
        if results.is_empty()
            && let Some(e) = last_err
        {
            return Err(e.context(format!("all {} shards failed for {op}", self.shards.len())));
        }
        Ok(results)
    }
}

impl Store for FanoutStore {
    // ── Spans / traces ──────────────────────────────────────────────
    fn insert_spans(&self, spans: &[Span]) -> Result<()> {
        // Route each span to its trace's owning shard. A trace's spans thus all
        // land together, keeping get_trace/correlate single-shard.
        route_and_insert(&self.shards, spans, |s| &s.trace_id, |store, batch| {
            store.insert_spans(batch)
        })
    }

    fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let mut all: Vec<Span> = self
            .fan_out("query_traces", |s| s.query_traces(query))?
            .into_iter()
            .flatten()
            .collect();
        // Each shard returned newest-first; merge by re-sorting and re-limiting.
        all.sort_by(|a, b| b.start_time.cmp(&a.start_time));
        all.truncate(limit);
        Ok(all)
    }

    fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
        // A trace lives on its owning shard, but fan out (short-circuiting on a
        // hit) so a routing-hash mismatch or a rebalancing window can't drop it.
        let mut spans = self.shard_for(trace_id).get_trace(trace_id)?;
        if spans.is_empty() {
            for (i, shard) in self.shards.iter().enumerate() {
                match shard.get_trace(trace_id) {
                    Ok(s) if !s.is_empty() => {
                        spans = s;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(shard = i, error = %e, "get_trace shard failed"),
                }
            }
        }
        // Dedup by span_id in case spans transiently overlap shards.
        let mut seen = std::collections::HashSet::new();
        spans.retain(|s| seen.insert(s.span_id.clone()));
        spans.sort_by_key(|s| s.start_time);
        Ok(spans)
    }

    fn list_services(&self) -> Result<Vec<ServiceInfo>> {
        let per_shard = self.fan_out("list_services", |s| s.list_services())?;
        Ok(merge_services(per_shard))
    }

    // ── Comments ── routed to the trace's owning shard ──────────────
    fn add_comment(
        &self,
        trace_id: &str,
        span_id: Option<&str>,
        author: &str,
        body: &str,
    ) -> Result<TraceComment> {
        self.shard_for(trace_id)
            .add_comment(trace_id, span_id, author, body)
    }

    fn get_comments(&self, trace_id: &str) -> Result<Vec<TraceComment>> {
        self.shard_for(trace_id).get_comments(trace_id)
    }

    // ── Logs ────────────────────────────────────────────────────────
    fn insert_logs(&self, logs: &[LogRecord]) -> Result<()> {
        // Logs carry trace_id and route the same way; orphan logs (no trace)
        // shard by service so a service's logs stay co-located-ish.
        route_and_insert(
            &self.shards,
            logs,
            |l| l.trace_id.as_deref().unwrap_or(&l.service),
            |store, batch| store.insert_logs(batch),
        )
    }

    fn query_logs(&self, query: &LogQuery) -> Result<Vec<LogRecord>> {
        let limit = query.limit.unwrap_or(100) as usize;
        let mut all: Vec<LogRecord> = self
            .fan_out("query_logs", |s| s.query_logs(query))?
            .into_iter()
            .flatten()
            .collect();
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        all.truncate(limit);
        Ok(all)
    }

    // ── Metrics ─────────────────────────────────────────────────────
    fn insert_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        // Metrics carry no trace; shard by name (design §3) so a series stays on
        // one shard and unique-name counts merge by simple sum.
        route_and_insert(&self.shards, metrics, |m| &m.name, |store, batch| {
            store.insert_metrics(batch)
        })
    }

    fn query_metrics(&self, query: &MetricQuery) -> Result<Vec<MetricPoint>> {
        let limit = query.limit.unwrap_or(500) as usize;
        let mut all: Vec<MetricPoint> = self
            .fan_out("query_metrics", |s| s.query_metrics(query))?
            .into_iter()
            .flatten()
            .collect();
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        all.truncate(limit);
        Ok(all)
    }

    // ── Cross-signal analytics ──────────────────────────────────────
    fn query_summary(&self, last_seconds: i64, service: Option<&str>) -> Result<SummaryReport> {
        let per_shard = self.fan_out("query_summary", |s| s.query_summary(last_seconds, service))?;
        Ok(merge_summaries(per_shard, last_seconds, service))
    }

    fn query_anomalies(
        &self,
        current_seconds: i64,
        baseline_seconds: i64,
        service: Option<&str>,
    ) -> Result<AnomalyReport> {
        let per_shard = self.fan_out("query_anomalies", |s| {
            s.query_anomalies(current_seconds, baseline_seconds, service)
        })?;
        Ok(merge_anomalies(
            per_shard,
            current_seconds,
            baseline_seconds,
            service,
        ))
    }

    fn query_correlate(&self, trace_id: &str) -> Result<Option<CorrelateReport>> {
        // The trace is single-shard; try its owner, then fall back to a fan-out.
        if let Some(r) = self.shard_for(trace_id).query_correlate(trace_id)? {
            return Ok(Some(r));
        }
        for (i, shard) in self.shards.iter().enumerate() {
            match shard.query_correlate(trace_id) {
                Ok(Some(r)) => return Ok(Some(r)),
                Ok(None) => {}
                Err(e) => tracing::warn!(shard = i, error = %e, "correlate shard failed"),
            }
        }
        Ok(None)
    }

    fn query_sql(&self, _sql: &str) -> Result<Vec<Value>> {
        // Arbitrary SQL over per-shard DuckDB projections doesn't distribute
        // soundly (cross-shard GROUP BY/aggregates). Design §3 recommendation
        // (c): keep it a single-node power tool.
        bail!(
            "query_sql is not distributed across shards; run it directly against a single shard's /api/v1/sql"
        );
    }

    // ── Lifecycle ───────────────────────────────────────────────────
    fn health(&self) -> Result<()> {
        // Ready if at least one shard answers — the node can still serve partial
        // results, consistent with fan-out read tolerance. Not-ready only when
        // every shard is unreachable.
        let mut healthy = 0usize;
        for (i, shard) in self.shards.iter().enumerate() {
            match shard.health() {
                Ok(()) => healthy += 1,
                Err(e) => tracing::warn!(shard = i, error = %e, "shard unhealthy"),
            }
        }
        if healthy == 0 {
            bail!("no healthy shards ({} total)", self.shards.len());
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        for shard in &self.shards {
            shard.flush()?;
        }
        Ok(())
    }
}

/// Group `items` by the shard their key hashes to, then hand each group to its
/// owning shard in a single call.
fn route_and_insert<T: Clone>(
    shards: &[Arc<dyn Store>],
    items: &[T],
    key: impl Fn(&T) -> &str,
    insert: impl Fn(&Arc<dyn Store>, &[T]) -> Result<()>,
) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let n = shards.len();
    let mut buckets: HashMap<usize, Vec<T>> = HashMap::new();
    for item in items {
        let idx = shard_index(key(item), n);
        buckets.entry(idx).or_default().push(item.clone());
    }
    for (idx, batch) in buckets {
        insert(&shards[idx], &batch)?;
    }
    Ok(())
}

/// Sum disjoint per-shard service rollups. `span_count`/`trace_count` add
/// directly (trace_ids are disjoint across shards); `avg_duration_ms` and
/// `error_rate` are recomputed as span-count-weighted means.
fn merge_services(per_shard: Vec<Vec<ServiceInfo>>) -> Vec<ServiceInfo> {
    struct Acc {
        span_count: i64,
        trace_count: i64,
        dur_weighted: f64,
        err_weighted: f64,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    for shard in per_shard {
        for s in shard {
            let e = acc.entry(s.name).or_insert(Acc {
                span_count: 0,
                trace_count: 0,
                dur_weighted: 0.0,
                err_weighted: 0.0,
            });
            e.span_count += s.span_count;
            e.trace_count += s.trace_count;
            e.dur_weighted += s.avg_duration_ms * s.span_count as f64;
            e.err_weighted += s.error_rate * s.span_count as f64;
        }
    }
    let mut out: Vec<ServiceInfo> = acc
        .into_iter()
        .map(|(name, a)| {
            let w = a.span_count.max(1) as f64;
            ServiceInfo {
                name,
                span_count: a.span_count,
                trace_count: a.trace_count,
                avg_duration_ms: a.dur_weighted / w,
                error_rate: a.err_weighted / w,
            }
        })
        .collect();
    out.sort_by(|a, b| b.span_count.cmp(&a.span_count));
    out
}

/// Span-count-weighted mean of a per-shard value.
fn weighted_mean(values: impl Iterator<Item = (f64, i64)>) -> f64 {
    let mut num = 0.0;
    let mut den = 0i64;
    for (v, w) in values {
        num += v * w as f64;
        den += w;
    }
    if den == 0 { 0.0 } else { num / den as f64 }
}

/// Merge per-shard summaries. Counts sum; rates and averages recompute from
/// component sums. Percentiles are span-count-weighted approximations — exact
/// cross-shard percentiles need a mergeable sketch (t-digest), tracked as
/// future work; they are an estimate, not a true global quantile.
fn merge_summaries(
    per_shard: Vec<SummaryReport>,
    window_seconds: i64,
    service: Option<&str>,
) -> SummaryReport {
    let mut traces = TraceSummary::default();
    let mut logs = LogSummary::default();
    let mut metrics = MetricSummary::default();

    // Re-aggregate top_services and top_error_operations across shards.
    struct SvcAcc {
        span_count: i64,
        err_weighted: f64,
        p95_weighted: f64,
    }
    let mut svc: HashMap<String, SvcAcc> = HashMap::new();
    let mut errops: HashMap<(String, String), i64> = HashMap::new();

    // Collect (value, weight) pairs for the weighted trace fields.
    let mut avg_pairs = Vec::new();
    let mut p50_pairs = Vec::new();
    let mut p95_pairs = Vec::new();
    let mut p99_pairs = Vec::new();

    for r in &per_shard {
        let t = &r.traces;
        traces.span_count += t.span_count;
        traces.trace_count += t.trace_count;
        traces.error_count += t.error_count;
        traces.max_ms = traces.max_ms.max(t.max_ms);
        avg_pairs.push((t.avg_ms, t.span_count));
        p50_pairs.push((t.p50_ms, t.span_count));
        p95_pairs.push((t.p95_ms, t.span_count));
        p99_pairs.push((t.p99_ms, t.span_count));

        logs.total += r.logs.total;
        logs.error += r.logs.error;
        logs.warn += r.logs.warn;
        logs.info += r.logs.info;
        logs.debug += r.logs.debug;

        metrics.point_count += r.metrics.point_count;
        // Metrics shard by name → names disjoint → unique counts sum.
        metrics.unique_names += r.metrics.unique_names;

        for s in &r.top_services {
            let e = svc.entry(s.service.clone()).or_insert(SvcAcc {
                span_count: 0,
                err_weighted: 0.0,
                p95_weighted: 0.0,
            });
            e.span_count += s.span_count;
            e.err_weighted += s.error_rate * s.span_count as f64;
            e.p95_weighted += s.p95_ms * s.span_count as f64;
        }
        for o in &r.top_error_operations {
            *errops
                .entry((o.service.clone(), o.operation.clone()))
                .or_insert(0) += o.error_count;
        }
    }

    traces.avg_ms = weighted_mean(avg_pairs.into_iter());
    traces.p50_ms = weighted_mean(p50_pairs.into_iter());
    traces.p95_ms = weighted_mean(p95_pairs.into_iter());
    traces.p99_ms = weighted_mean(p99_pairs.into_iter());
    traces.error_rate = if traces.span_count > 0 {
        traces.error_count as f64 / traces.span_count as f64
    } else {
        0.0
    };

    let mut top_services: Vec<ServiceSummary> = svc
        .into_iter()
        .map(|(service, a)| {
            let w = a.span_count.max(1) as f64;
            ServiceSummary {
                service,
                span_count: a.span_count,
                error_rate: a.err_weighted / w,
                p95_ms: a.p95_weighted / w,
            }
        })
        .collect();
    top_services.sort_by(|a, b| b.span_count.cmp(&a.span_count));
    top_services.truncate(10);

    let mut top_error_operations: Vec<ErrorOperation> = errops
        .into_iter()
        .map(|((service, operation), error_count)| ErrorOperation {
            service,
            operation,
            error_count,
        })
        .collect();
    top_error_operations.sort_by(|a, b| b.error_count.cmp(&a.error_count));
    top_error_operations.truncate(10);

    SummaryReport {
        window_seconds,
        service_filter: service.map(str::to_string),
        traces,
        top_services,
        top_error_operations,
        logs,
        metrics,
    }
}

/// Merge per-shard anomaly reports. Because trace_id sharding spreads a single
/// service's traffic across all shards, each shard sees only a slice, so these
/// are merged best-effort: anomalies for the same (service, kind) are collapsed
/// to the one with the largest |delta| (most significant signal). This is an
/// approximation — a precise version would recompute current/baseline from raw
/// per-shard partials.
fn merge_anomalies(
    per_shard: Vec<AnomalyReport>,
    current_seconds: i64,
    baseline_seconds: i64,
    service: Option<&str>,
) -> AnomalyReport {
    let mut best: HashMap<(String, String), Anomaly> = HashMap::new();
    for r in per_shard {
        for a in r.anomalies {
            let key = (a.service.clone(), a.kind.clone());
            match best.get(&key) {
                Some(existing) if existing.delta.abs() >= a.delta.abs() => {}
                _ => {
                    best.insert(key, a);
                }
            }
        }
    }
    let mut anomalies: Vec<Anomaly> = best.into_values().collect();
    anomalies.sort_by(|a, b| {
        b.delta
            .abs()
            .partial_cmp(&a.delta.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    AnomalyReport {
        current_seconds,
        baseline_seconds,
        service_filter: service.map(str::to_string),
        anomalies,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{SpanKind, SpanStatus};
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Minimal in-memory `Store` for exercising fan-out routing and merge logic
    /// without a network. Implements the span surface the tests touch; the rest
    /// return empty/default.
    #[derive(Default)]
    struct MockStore {
        spans: Mutex<Vec<Span>>,
    }

    impl Store for MockStore {
        fn insert_spans(&self, spans: &[Span]) -> Result<()> {
            self.spans.lock().unwrap().extend_from_slice(spans);
            Ok(())
        }
        fn query_traces(&self, query: &TraceQuery) -> Result<Vec<Span>> {
            let mut v = self.spans.lock().unwrap().clone();
            v.sort_by(|a, b| b.start_time.cmp(&a.start_time)); // newest-first contract
            v.truncate(query.limit.unwrap_or(100) as usize);
            Ok(v)
        }
        fn get_trace(&self, trace_id: &str) -> Result<Vec<Span>> {
            Ok(self
                .spans
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.trace_id == trace_id)
                .cloned()
                .collect())
        }
        fn list_services(&self) -> Result<Vec<ServiceInfo>> {
            let spans = self.spans.lock().unwrap();
            let mut by: HashMap<String, (i64, f64, i64)> = HashMap::new();
            for s in spans.iter() {
                let e = by.entry(s.service.clone()).or_insert((0, 0.0, 0));
                e.0 += 1;
                e.1 += s.duration_ms;
                if s.status == SpanStatus::Error {
                    e.2 += 1;
                }
            }
            Ok(by
                .into_iter()
                .map(|(name, (n, dur, err))| ServiceInfo {
                    name,
                    span_count: n,
                    trace_count: n,
                    avg_duration_ms: dur / n as f64,
                    error_rate: err as f64 / n as f64,
                })
                .collect())
        }
        fn add_comment(
            &self,
            _t: &str,
            _s: Option<&str>,
            _a: &str,
            _b: &str,
        ) -> Result<TraceComment> {
            bail!("unused")
        }
        fn get_comments(&self, _t: &str) -> Result<Vec<TraceComment>> {
            Ok(vec![])
        }
        fn insert_logs(&self, _l: &[LogRecord]) -> Result<()> {
            Ok(())
        }
        fn query_logs(&self, _q: &LogQuery) -> Result<Vec<LogRecord>> {
            Ok(vec![])
        }
        fn insert_metrics(&self, _m: &[MetricPoint]) -> Result<()> {
            Ok(())
        }
        fn query_metrics(&self, _q: &MetricQuery) -> Result<Vec<MetricPoint>> {
            Ok(vec![])
        }
        fn query_summary(&self, _l: i64, _s: Option<&str>) -> Result<SummaryReport> {
            bail!("unused")
        }
        fn query_anomalies(&self, _c: i64, _b: i64, _s: Option<&str>) -> Result<AnomalyReport> {
            bail!("unused")
        }
        fn query_correlate(&self, _t: &str) -> Result<Option<CorrelateReport>> {
            Ok(None)
        }
        fn query_sql(&self, _s: &str) -> Result<Vec<Value>> {
            bail!("unused")
        }
    }

    fn span(trace: &str, sid: &str, svc: &str, secs: i64, status: SpanStatus) -> Span {
        let t = Utc.timestamp_opt(secs, 0).unwrap();
        Span {
            trace_id: trace.into(),
            span_id: sid.into(),
            parent_span_id: None,
            service: svc.into(),
            operation: "op".into(),
            start_time: t,
            end_time: t,
            duration_ms: 10.0,
            status,
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    fn fanout(n: usize) -> (FanoutStore, Vec<Arc<MockStore>>) {
        let mocks: Vec<Arc<MockStore>> = (0..n).map(|_| Arc::new(MockStore::default())).collect();
        let shards: Vec<Arc<dyn Store>> = mocks.iter().map(|m| m.clone() as Arc<dyn Store>).collect();
        (FanoutStore::new(shards).unwrap(), mocks)
    }

    #[test]
    fn new_rejects_empty_shards() {
        assert!(FanoutStore::new(vec![]).is_err());
    }

    #[test]
    fn shard_index_is_deterministic_and_bounded() {
        for key in ["trace-abc", "trace-def", "svc-1", ""] {
            let a = shard_index(key, 4);
            let b = shard_index(key, 4);
            assert_eq!(a, b, "hashing must be stable across calls");
            assert!(a < 4);
        }
    }

    #[test]
    fn insert_spans_routes_whole_trace_to_one_shard() {
        let (fo, mocks) = fanout(3);
        // Two traces, each with two spans, in one mixed batch.
        fo.insert_spans(&[
            span("t1", "a", "api", 1, SpanStatus::Ok),
            span("t2", "b", "api", 2, SpanStatus::Ok),
            span("t1", "c", "db", 3, SpanStatus::Ok),
            span("t2", "d", "db", 4, SpanStatus::Ok),
        ])
        .unwrap();

        // Every span of a trace lands on exactly one shard (the trace's owner).
        for tid in ["t1", "t2"] {
            let owners: Vec<usize> = mocks
                .iter()
                .enumerate()
                .filter(|(_, m)| m.spans.lock().unwrap().iter().any(|s| s.trace_id == tid))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(owners.len(), 1, "trace {tid} split across shards: {owners:?}");
            let owned = mocks[owners[0]].spans.lock().unwrap();
            assert_eq!(owned.iter().filter(|s| s.trace_id == tid).count(), 2);
        }
    }

    #[test]
    fn query_traces_merges_newest_first_and_re_limits() {
        let (fo, _m) = fanout(3);
        // Insert 6 spans across distinct traces at increasing timestamps.
        for i in 0..6 {
            fo.insert_spans(&[span(&format!("t{i}"), "s", "api", i, SpanStatus::Ok)])
                .unwrap();
        }
        let q = TraceQuery {
            limit: Some(3),
            ..Default::default()
        };
        let got = fo.query_traces(&q).unwrap();
        assert_eq!(got.len(), 3, "must re-limit after gathering");
        // Newest-first global order: t5, t4, t3.
        let starts: Vec<i64> = got.iter().map(|s| s.start_time.timestamp()).collect();
        assert_eq!(starts, vec![5, 4, 3]);
    }

    #[test]
    fn get_trace_finds_trace_on_its_owning_shard() {
        let (fo, _m) = fanout(4);
        fo.insert_spans(&[
            span("trace-x", "1", "api", 1, SpanStatus::Ok),
            span("trace-x", "2", "db", 2, SpanStatus::Ok),
        ])
        .unwrap();
        let spans = fo.get_trace("trace-x").unwrap();
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|s| s.trace_id == "trace-x"));
        assert!(fo.get_trace("nope").unwrap().is_empty());
    }

    #[test]
    fn list_services_aggregates_across_shards() {
        let (fo, _m) = fanout(3);
        // 3 "api" spans (1 error) spread by trace_id across shards.
        fo.insert_spans(&[
            span("ta", "1", "api", 1, SpanStatus::Ok),
            span("tb", "2", "api", 2, SpanStatus::Ok),
            span("tc", "3", "api", 3, SpanStatus::Error),
        ])
        .unwrap();
        let svcs = fo.list_services().unwrap();
        let api = svcs.iter().find(|s| s.name == "api").unwrap();
        assert_eq!(api.span_count, 3, "counts must sum across shards");
        assert_eq!(api.trace_count, 3);
        assert!((api.error_rate - 1.0 / 3.0).abs() < 1e-9, "error_rate recomputed from sums");
        assert!((api.avg_duration_ms - 10.0).abs() < 1e-9);
    }

    #[test]
    fn query_sql_is_not_distributed() {
        let (fo, _m) = fanout(2);
        assert!(fo.query_sql("SELECT 1").is_err());
    }

    #[test]
    fn merge_services_sums_counts_and_weights_rates() {
        let shards = vec![
            vec![ServiceInfo {
                name: "api".into(),
                span_count: 10,
                trace_count: 4,
                avg_duration_ms: 20.0,
                error_rate: 0.1,
            }],
            vec![ServiceInfo {
                name: "api".into(),
                span_count: 30,
                trace_count: 6,
                avg_duration_ms: 40.0,
                error_rate: 0.5,
            }],
        ];
        let merged = merge_services(shards);
        let api = &merged[0];
        assert_eq!(api.span_count, 40);
        assert_eq!(api.trace_count, 10);
        // (20*10 + 40*30) / 40 = 35
        assert!((api.avg_duration_ms - 35.0).abs() < 1e-9);
        // (0.1*10 + 0.5*30) / 40 = 0.4
        assert!((api.error_rate - 0.4).abs() < 1e-9);
    }
}
