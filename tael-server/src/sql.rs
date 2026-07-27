//! SQL over the default storage engine, via DataFusion.
//!
//! `tael query sql` previously required a `--features duckdb` build, which
//! meant the escape hatch SKILL.md teaches — "drop to SQL when the structured
//! commands can't express the cut you need" — errored out on every default
//! install. This module closes that: the same four tables (`spans`, `logs`,
//! `metrics`, `trace_comments`) with the same column names, so a query written
//! against either backend runs on the other.
//!
//! Execution is in-memory. Rows are pulled through the normal hot∪cold read
//! path, converted to Arrow, and registered as tables for one query. That is a
//! deliberate ceiling rather than an oversight — this is the escape hatch, not
//! the primary read path, and a bounded scan that refuses honestly beats an
//! unbounded one that exhausts memory. [`ROW_LIMIT`] is where the ceiling
//! lives.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use datafusion::prelude::SessionContext;

use crate::storage::models::{LogQuery, LogRecord, MetricPoint, MetricQuery, Span, TraceComment};

/// Rows loaded per table for one query.
///
/// A SQL query has no time window of its own, so without a cap a `SELECT *`
/// would try to materialize the entire retained history in memory. The result
/// says how many rows each table actually contributed, so a caller can tell a
/// complete answer from a truncated one instead of quietly trusting a partial
/// aggregate.
const ROW_LIMIT: u32 = 200_000;

/// Run a read-only SQL query over the telemetry tables.
pub fn query(
    spans: Vec<Span>,
    logs: Vec<LogRecord>,
    metrics: Vec<MetricPoint>,
    comments: Vec<TraceComment>,
    sql: &str,
) -> Result<Vec<serde_json::Value>> {
    guard_read_only(sql)?;

    let ctx = SessionContext::new();
    ctx.register_batch("spans", spans_batch(&spans)?)
        .context("registering spans")?;
    ctx.register_batch("logs", logs_batch(&logs)?)
        .context("registering logs")?;
    ctx.register_batch("metrics", metrics_batch(&metrics)?)
        .context("registering metrics")?;
    ctx.register_batch("trace_comments", comments_batch(&comments)?)
        .context("registering trace_comments")?;

    // DataFusion is async; the Store trait is synchronous by design. Callers
    // reach this from inside the server's tokio runtime (a REST handler), and
    // starting a runtime from within one panics — so the query runs on a
    // dedicated OS thread, which carries no tokio context. A scoped thread
    // keeps the borrow of `ctx` and `sql` without cloning them.
    let sql_owned = sql.to_string();
    let batches = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("building SQL runtime")?;
                runtime.block_on(async {
                    let df = ctx.sql(&sql_owned).await?;
                    anyhow::Ok(df.collect().await?)
                })
            })
            .join()
    })
    .map_err(|_| anyhow::anyhow!("SQL execution thread panicked"))??;

    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(batch_to_json(batch)?);
    }
    Ok(rows)
}

/// How many rows each table can contribute to one query.
pub fn row_limit() -> u32 {
    ROW_LIMIT
}

/// Query specs that load exactly what SQL execution needs.
pub fn source_queries() -> (crate::storage::models::TraceQuery, LogQuery, MetricQuery) {
    (
        crate::storage::models::TraceQuery {
            limit: Some(ROW_LIMIT),
            ..Default::default()
        },
        LogQuery {
            limit: Some(ROW_LIMIT),
            ..Default::default()
        },
        MetricQuery {
            service: None,
            name: None,
            metric_type: None,
            last_seconds: None,
            limit: Some(ROW_LIMIT),
            tenant: None,
        },
    )
}

/// Reject anything that isn't a read.
///
/// The tables are in-memory copies, so a mutation could not corrupt stored
/// data — but it would silently succeed against a throwaway table and look
/// like it worked, which is worse than refusing. Checking the leading keyword
/// also blocks the multi-statement trick of appending a second statement after
/// a legitimate `SELECT`.
fn guard_read_only(sql: &str) -> Result<()> {
    let trimmed = sql.trim_start();
    let leading = trimmed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !matches!(leading.as_str(), "SELECT" | "WITH" | "EXPLAIN") {
        bail!("only SELECT, WITH, and EXPLAIN queries are allowed (got `{leading}`)");
    }
    // Statement separators outside of string literals would allow a second,
    // unchecked statement to ride along.
    let mut in_string = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // Doubled quotes are an escaped quote inside a literal.
                if in_string && chars.peek() == Some(&'\'') {
                    chars.next();
                } else {
                    in_string = !in_string;
                }
            }
            ';' if !in_string => {
                if chars.by_ref().any(|c| !c.is_whitespace()) {
                    bail!("multiple SQL statements are not allowed");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// ── Arrow conversion ────────────────────────────────────────────────

fn json_map(map: &HashMap<String, String>) -> String {
    serde_json::to_string(map).unwrap_or_else(|_| "{}".into())
}

/// Column names mirror the DuckDB schema exactly, so a query written for one
/// backend runs unchanged on the other. LLM fields are additionally flattened
/// into typed columns, because `llm_json` alone would force string surgery for
/// the token and cost aggregations these tables exist to answer.
fn spans_batch(spans: &[Span]) -> Result<RecordBatch> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("span_id", DataType::Utf8, false),
        Field::new("parent_span_id", DataType::Utf8, true),
        Field::new("service", DataType::Utf8, false),
        Field::new("operation", DataType::Utf8, false),
        Field::new("start_time", DataType::Utf8, false),
        Field::new("end_time", DataType::Utf8, false),
        Field::new("duration_ms", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
        Field::new("events", DataType::Utf8, false),
        Field::new("llm", DataType::Utf8, true),
        Field::new("llm_provider", DataType::Utf8, true),
        Field::new("llm_model", DataType::Utf8, true),
        Field::new("input_tokens", DataType::Int64, true),
        Field::new("output_tokens", DataType::Int64, true),
        Field::new("total_tokens", DataType::Int64, true),
        Field::new("cost_usd", DataType::Float64, true),
    ]));

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.trace_id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.span_id.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            spans.iter().map(|s| s.parent_span_id.clone()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.service.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.operation.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.start_time.to_rfc3339()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.end_time.to_rfc3339()),
        )),
        Arc::new(Float64Array::from_iter_values(
            spans.iter().map(|s| s.duration_ms),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.status.to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| s.kind.to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            spans.iter().map(|s| json_map(&s.attributes)),
        )),
        Arc::new(StringArray::from_iter_values(spans.iter().map(|s| {
            serde_json::to_string(&s.events).unwrap_or_else(|_| "[]".into())
        }))),
        Arc::new(StringArray::from_iter(spans.iter().map(|s| {
            s.llm.as_ref().and_then(|l| serde_json::to_string(l).ok())
        }))),
        Arc::new(StringArray::from_iter(
            spans
                .iter()
                .map(|s| s.llm.as_ref().map(|l| l.provider.clone())),
        )),
        Arc::new(StringArray::from_iter(
            spans
                .iter()
                .map(|s| s.llm.as_ref().map(|l| l.model.clone())),
        )),
        Arc::new(Int64Array::from_iter(spans.iter().map(|s| {
            s.llm.as_ref().and_then(|l| l.input_tokens).map(i64::from)
        }))),
        Arc::new(Int64Array::from_iter(spans.iter().map(|s| {
            s.llm.as_ref().and_then(|l| l.output_tokens).map(i64::from)
        }))),
        Arc::new(Int64Array::from_iter(spans.iter().map(|s| {
            s.llm.as_ref().and_then(|l| l.total_tokens).map(i64::from)
        }))),
        Arc::new(Float64Array::from_iter(
            spans
                .iter()
                .map(|s| s.llm.as_ref().and_then(|l| l.cost_usd)),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

fn logs_batch(logs: &[LogRecord]) -> Result<RecordBatch> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("observed_timestamp", DataType::Utf8, false),
        Field::new("trace_id", DataType::Utf8, true),
        Field::new("span_id", DataType::Utf8, true),
        Field::new("severity", DataType::Utf8, false),
        Field::new("severity_text", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
        Field::new("body_sha256", DataType::Utf8, true),
    ]));

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.timestamp.to_rfc3339()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.observed_timestamp.to_rfc3339()),
        )),
        Arc::new(StringArray::from_iter(
            logs.iter().map(|l| l.trace_id.clone()),
        )),
        Arc::new(StringArray::from_iter(
            logs.iter().map(|l| l.span_id.clone()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.severity.to_string()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.severity_text.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.body.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| l.service.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            logs.iter().map(|l| json_map(&l.attributes)),
        )),
        Arc::new(StringArray::from_iter(
            logs.iter().map(|l| l.body_sha256.clone()),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

fn metrics_batch(metrics: &[MetricPoint]) -> Result<RecordBatch> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::Utf8, false),
        Field::new("service", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("metric_type", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
        Field::new("unit", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
    ]));

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| m.timestamp.to_rfc3339()),
        )),
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| m.service.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| m.name.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| m.metric_type.to_string()),
        )),
        Arc::new(Float64Array::from_iter_values(
            metrics.iter().map(|m| m.value),
        )),
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| m.unit.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            metrics.iter().map(|m| json_map(&m.attributes)),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

fn comments_batch(comments: &[TraceComment]) -> Result<RecordBatch> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("trace_id", DataType::Utf8, false),
        Field::new("span_id", DataType::Utf8, true),
        Field::new("author", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("created_at", DataType::Utf8, false),
    ]));

    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            comments.iter().map(|c| c.id.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            comments.iter().map(|c| c.trace_id.as_str()),
        )),
        Arc::new(StringArray::from_iter(
            comments.iter().map(|c| c.span_id.clone()),
        )),
        Arc::new(StringArray::from_iter_values(
            comments.iter().map(|c| c.author.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            comments.iter().map(|c| c.body.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            comments.iter().map(|c| c.created_at.as_str()),
        )),
    ];
    Ok(RecordBatch::try_new(schema, columns)?)
}

/// Convert a result batch to JSON objects, matching the DuckDB backend's row
/// shape so both produce the same `{"rows": [...]}` payload.
fn batch_to_json(batch: &RecordBatch) -> Result<Vec<serde_json::Value>> {
    use arrow::array::{Array, BooleanArray, Int32Array, UInt64Array};

    let schema = batch.schema();
    let mut rows = Vec::with_capacity(batch.num_rows());

    for row in 0..batch.num_rows() {
        let mut obj = serde_json::Map::new();
        for (col, field) in schema.fields().iter().enumerate() {
            let array = batch.column(col);
            let value = if array.is_null(row) {
                serde_json::Value::Null
            } else if let Some(a) = array.as_any().downcast_ref::<StringArray>() {
                serde_json::Value::String(a.value(row).to_string())
            } else if let Some(a) = array.as_any().downcast_ref::<Float64Array>() {
                serde_json::Number::from_f64(a.value(row))
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
                serde_json::Value::Number(a.value(row).into())
            } else if let Some(a) = array.as_any().downcast_ref::<Int32Array>() {
                serde_json::Value::Number(a.value(row).into())
            } else if let Some(a) = array.as_any().downcast_ref::<UInt64Array>() {
                serde_json::Value::Number(a.value(row).into())
            } else if let Some(a) = array.as_any().downcast_ref::<BooleanArray>() {
                serde_json::Value::Bool(a.value(row))
            } else {
                // Anything else (timestamps from date functions, decimals)
                // renders through Arrow's own display rather than being
                // dropped.
                serde_json::Value::String(
                    arrow::util::display::array_value_to_string(array, row)
                        .unwrap_or_else(|_| String::new()),
                )
            };
            obj.insert(field.name().clone(), value);
        }
        rows.push(serde_json::Value::Object(obj));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{LlmSpan, SpanKind, SpanStatus};
    use chrono::Utc;

    fn span(trace: &str, service: &str, status: SpanStatus, ms: f64) -> Span {
        let now = Utc::now();
        Span {
            trace_id: trace.into(),
            span_id: format!("s{trace}"),
            parent_span_id: None,
            service: service.into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms: ms,
            status,
            attributes: HashMap::from([("http.method".to_string(), "GET".to_string())]),
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        }
    }

    fn run(spans: Vec<Span>, sql: &str) -> Result<Vec<serde_json::Value>> {
        query(spans, Vec::new(), Vec::new(), Vec::new(), sql)
    }

    #[test]
    fn aggregates_over_spans() {
        let spans = vec![
            span("t1", "api", SpanStatus::Error, 10.0),
            span("t2", "api", SpanStatus::Ok, 20.0),
            span("t3", "db", SpanStatus::Error, 30.0),
        ];
        let rows = run(
            spans,
            "SELECT service, COUNT(*) AS n FROM spans WHERE status = 'error' \
             GROUP BY service ORDER BY n DESC, service",
        )
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["service"], "api");
        assert_eq!(rows[0]["n"], 1);
        assert_eq!(rows[1]["service"], "db");
    }

    #[test]
    fn joins_across_signals() {
        let spans = vec![span("t1", "api", SpanStatus::Error, 10.0)];
        let logs = vec![LogRecord {
            timestamp: Utc::now(),
            observed_timestamp: Utc::now(),
            trace_id: Some("t1".into()),
            span_id: None,
            severity: crate::storage::models::LogSeverity::Error,
            severity_text: "ERROR".into(),
            body: "payment declined".into(),
            service: "api".into(),
            attributes: HashMap::new(),
            body_sha256: None,
        }];

        // Cross-signal joins are the reason the SQL escape hatch exists.
        let rows = query(
            spans,
            logs,
            Vec::new(),
            Vec::new(),
            "SELECT s.service, l.body FROM spans s JOIN logs l ON s.trace_id = l.trace_id",
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["body"], "payment declined");
    }

    #[test]
    fn llm_fields_are_queryable_without_json_surgery() {
        let mut s = span("t1", "agent", SpanStatus::Ok, 100.0);
        s.kind = SpanKind::Llm;
        s.llm = Some(LlmSpan {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            input_tokens: Some(1000),
            output_tokens: Some(250),
            total_tokens: Some(1250),
            cost_usd: Some(0.05),
            ..Default::default()
        });

        let rows = run(
            vec![s],
            "SELECT llm_model, SUM(total_tokens) AS tokens, SUM(cost_usd) AS cost \
             FROM spans WHERE llm_model IS NOT NULL GROUP BY llm_model",
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["llm_model"], "claude-opus-4-7");
        assert_eq!(rows[0]["tokens"], 1250);
    }

    #[test]
    fn empty_tables_still_answer() {
        // An aggregate over no rows is a valid answer, not an error.
        let rows = run(Vec::new(), "SELECT COUNT(*) AS n FROM spans").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["n"], 0);
    }

    #[test]
    fn all_four_tables_are_registered() {
        for table in ["spans", "logs", "metrics", "trace_comments"] {
            let sql = format!("SELECT COUNT(*) AS n FROM {table}");
            assert!(run(Vec::new(), &sql).is_ok(), "{table} should be queryable");
        }
    }

    #[test]
    fn mutations_are_rejected() {
        for sql in [
            "DROP TABLE spans",
            "DELETE FROM spans",
            "INSERT INTO spans VALUES (1)",
            "UPDATE spans SET service = 'x'",
            "CREATE TABLE evil (a INT)",
        ] {
            assert!(run(Vec::new(), sql).is_err(), "`{sql}` should be rejected");
        }
    }

    #[test]
    fn a_second_statement_cannot_ride_along() {
        // The leading-keyword check alone would pass this.
        assert!(run(Vec::new(), "SELECT 1; DROP TABLE spans").is_err());
        // A trailing semicolon on a single statement is fine.
        assert!(run(Vec::new(), "SELECT COUNT(*) AS n FROM spans;").is_ok());
        // A semicolon inside a string literal is data, not a separator.
        assert!(run(Vec::new(), "SELECT 'a;b' AS s").is_ok());
    }

    #[test]
    fn with_and_explain_are_allowed() {
        assert!(
            run(
                Vec::new(),
                "WITH t AS (SELECT 1 AS n) SELECT COUNT(*) AS c FROM t"
            )
            .is_ok()
        );
        assert!(run(Vec::new(), "EXPLAIN SELECT * FROM spans").is_ok());
    }

    #[test]
    fn a_syntax_error_is_reported_not_swallowed() {
        let err = run(Vec::new(), "SELECT FROM WHERE").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn unknown_tables_name_themselves_in_the_error() {
        let err = run(Vec::new(), "SELECT * FROM nonexistent").unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "{err}");
    }
}
