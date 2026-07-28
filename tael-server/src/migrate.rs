//! One-shot DuckDB → tael-backend data migration (`tael server migrate`).
//!
//! The storage default flipped to the tael-backend engine, but a site with an
//! existing DuckDB datastore had no path onto it besides starting over — the
//! blocker the backend plan tracks under B5. This copies every span, log,
//! metric point, and trace comment out of the legacy DuckDB file and inserts
//! them through the tael-backend write path (WAL → hot tier), so the migrated
//! rows age into the cold tier exactly like freshly ingested ones.
//!
//! Blobs need no migration: both engines share the same content-addressed
//! blob store under the data directory, so `prompt_sha256`-style references
//! resolve unchanged.
//!
//! Two fidelity caveats, both reported rather than hidden:
//! - Comment `created_at`/`id` are regenerated on insert (the comments store
//!   has no timestamped import seam); bodies, authors, and span pins survive.
//! - The full-text search index is built at ingest from payload text that was
//!   already blobbed away, so migrated spans are not text-searchable.
//!
//! Idempotent: hot-tier keys derive from record identity, so a re-run
//! overwrites rather than duplicates (comments excepted — they are
//! append-only, so only re-run comment migration once).

use anyhow::Result;
use serde::Serialize;

use crate::storage::models::{LogQuery, MetricQuery, TraceQuery};
use crate::storage::{DuckDbStore, Store, TaelBackend};

/// What was (or would be, under `dry_run`) migrated.
#[derive(Debug, Default, Serialize)]
pub struct MigrationReport {
    pub spans: usize,
    pub logs: usize,
    pub metrics: usize,
    pub comments: usize,
    pub dry_run: bool,
}

const BATCH: usize = 1_000;
/// DuckDB `LIMIT` accepts i64; this reads "everything" without overflow.
const ALL: u32 = u32::MAX;

/// Copy all telemetry from the DuckDB store in `source_dir` into a
/// tael-backend engine in `target_dir` (the same directory is fine — the two
/// engines use disjoint files). `dry_run` only counts.
pub fn migrate_duckdb(
    source_dir: &str,
    target_dir: &str,
    dry_run: bool,
) -> Result<MigrationReport> {
    let source = DuckDbStore::new(source_dir)?;

    let spans = source.query_traces(&TraceQuery {
        limit: Some(ALL),
        ..Default::default()
    })?;
    let logs = source.query_logs(&LogQuery {
        limit: Some(ALL),
        ..Default::default()
    })?;
    let metrics = source.query_metrics(&MetricQuery {
        limit: Some(ALL),
        ..Default::default()
    })?;
    // DuckDB has no cross-trace comment listing; enumerate via the migrated
    // spans' trace ids.
    let mut trace_ids: Vec<&str> = spans.iter().map(|s| s.trace_id.as_str()).collect();
    trace_ids.sort_unstable();
    trace_ids.dedup();
    let mut comments = Vec::new();
    for trace_id in &trace_ids {
        comments.extend(source.get_comments(trace_id)?);
    }

    let report = MigrationReport {
        spans: spans.len(),
        logs: logs.len(),
        metrics: metrics.len(),
        comments: comments.len(),
        dry_run,
    };
    if dry_run {
        return Ok(report);
    }

    let target = TaelBackend::new(target_dir)?;
    for chunk in spans.chunks(BATCH) {
        target.insert_spans(chunk)?;
    }
    for chunk in logs.chunks(BATCH) {
        target.insert_logs(chunk)?;
    }
    for chunk in metrics.chunks(BATCH) {
        target.insert_metrics(chunk)?;
    }
    for c in &comments {
        target.add_comment(&c.trace_id, c.span_id.as_deref(), &c.author, &c.body)?;
    }
    // Tighten on-disk state so a server started right after sees everything
    // without a WAL replay.
    target.flush()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{
        LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanKind, SpanStatus,
    };
    use chrono::Utc;
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
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    #[test]
    fn migrates_all_signals_and_comments() {
        let source_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let source = DuckDbStore::new(source_dir.path().to_str().unwrap()).unwrap();
        source
            .insert_spans(&[span("t1", "s1"), span("t1", "s2"), span("t2", "s3")])
            .unwrap();
        source
            .insert_logs(&[LogRecord {
                timestamp: Utc::now(),
                observed_timestamp: Utc::now(),
                trace_id: Some("t1".into()),
                span_id: None,
                severity: LogSeverity::Error,
                severity_text: "ERROR".into(),
                body: "boom".into(),
                service: "svc".into(),
                attributes: HashMap::new(),
                body_sha256: None,
            }])
            .unwrap();
        source
            .insert_metrics(&[MetricPoint {
                timestamp: Utc::now(),
                service: "svc".into(),
                name: "m".into(),
                metric_type: MetricType::Gauge,
                value: 1.0,
                unit: String::new(),
                attributes: HashMap::new(),
                histogram: None,
            }])
            .unwrap();
        source.add_comment("t1", None, "tester", "note").unwrap();

        let dry = migrate_duckdb(
            source_dir.path().to_str().unwrap(),
            target_dir.path().to_str().unwrap(),
            true,
        )
        .unwrap();
        assert_eq!(
            (dry.spans, dry.logs, dry.metrics, dry.comments),
            (3, 1, 1, 1)
        );

        let report = migrate_duckdb(
            source_dir.path().to_str().unwrap(),
            target_dir.path().to_str().unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(report.spans, 3);

        let target = TaelBackend::new(target_dir.path().to_str().unwrap()).unwrap();
        assert_eq!(target.get_trace("t1").unwrap().len(), 2);
        assert_eq!(
            target
                .query_logs(&LogQuery::default())
                .unwrap()
                .first()
                .map(|l| l.body.clone()),
            Some("boom".into())
        );
        assert_eq!(
            target.query_metrics(&MetricQuery::default()).unwrap().len(),
            1
        );
        assert_eq!(target.get_comments("t1").unwrap().len(), 1);
    }
}
