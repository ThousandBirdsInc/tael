// Not every bench target uses every generator in this shared module.
#![allow(dead_code)]

//! Shared fixtures for the tael-server benchmarks.
//!
//! Generators are deterministic so benchmark inputs are stable across runs.
//! Spans are grouped into traces of 10 (the `trace_id` is derived from `i / 10`)
//! so trace-assembly benchmarks operate on realistic multi-span traces.

use std::collections::HashMap;

use chrono::{Duration, TimeZone, Utc};
use tael_server::{LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanKind, SpanStatus};

/// Fixed base epoch second for deterministic timestamps.
const BASE_TS: i64 = 1_700_000_000;

/// Number of spans grouped under a single trace id.
pub const SPANS_PER_TRACE: usize = 10;

/// Build a single synthetic span.
pub fn make_span(i: usize) -> Span {
    let start = Utc.timestamp_opt(BASE_TS + i as i64, 0).single().unwrap();
    let end = start + Duration::milliseconds(5);
    Span {
        trace_id: format!("{:032x}", i / SPANS_PER_TRACE),
        span_id: format!("{:016x}", i),
        // Every trace's first span is the root (no parent).
        parent_span_id: (i % SPANS_PER_TRACE != 0).then(|| format!("{:016x}", i - 1)),
        service: format!("service-{}", i % 8),
        operation: format!("operation-{}", i % 20),
        start_time: start,
        end_time: end,
        duration_ms: 5.0,
        status: if i % 50 == 0 {
            SpanStatus::Error
        } else {
            SpanStatus::Ok
        },
        attributes: attrs([
            ("http.method", "GET".to_string()),
            ("http.route", "/api/v1/resource".to_string()),
            ("http.status_code", "200".to_string()),
            ("net.peer.name", "upstream".to_string()),
            ("index", i.to_string()),
        ]),
        events: Vec::new(),
        kind: SpanKind::Server,
        llm: None,
    }
}

/// Build a batch of `n` spans.
pub fn make_spans(n: usize) -> Vec<Span> {
    (0..n).map(make_span).collect()
}

/// Build a single synthetic log record.
pub fn make_log(i: usize) -> LogRecord {
    let ts = Utc.timestamp_opt(BASE_TS + i as i64, 0).single().unwrap();
    LogRecord {
        timestamp: ts,
        observed_timestamp: ts,
        trace_id: Some(format!("{:032x}", i / SPANS_PER_TRACE)),
        span_id: Some(format!("{:016x}", i)),
        severity: if i % 20 == 0 {
            LogSeverity::Error
        } else {
            LogSeverity::Info
        },
        severity_text: if i % 20 == 0 { "ERROR" } else { "INFO" }.to_string(),
        body: format!("request {} completed in {}ms", i, i % 100),
        service: format!("service-{}", i % 8),
        attributes: attrs([
            ("thread", (i % 4).to_string()),
            ("region", "us-east-1".to_string()),
        ]),
        body_sha256: None,
    }
}

/// Build a batch of `n` log records.
pub fn make_logs(n: usize) -> Vec<LogRecord> {
    (0..n).map(make_log).collect()
}

/// Build a single synthetic metric point.
pub fn make_metric(i: usize) -> MetricPoint {
    let ts = Utc.timestamp_opt(BASE_TS + i as i64, 0).single().unwrap();
    MetricPoint {
        timestamp: ts,
        service: format!("service-{}", i % 8),
        name: format!("metric_{}", i % 12),
        metric_type: MetricType::Gauge,
        value: (i as f64) * 1.5,
        unit: "1".to_string(),
        attributes: attrs([
            ("endpoint", "/health".to_string()),
            ("code", "200".to_string()),
        ]),
    }
}

/// Build a batch of `n` metric points.
pub fn make_metrics(n: usize) -> Vec<MetricPoint> {
    (0..n).map(make_metric).collect()
}

/// A representative LLM prompt/response payload of roughly `kb` kilobytes.
/// Deterministic given `seed`; distinct seeds produce distinct content (so the
/// blob store treats them as unique, non-deduplicating blobs).
pub fn make_payload(seed: usize, kb: usize) -> Vec<u8> {
    let unit = format!(
        "{{\"role\":\"user\",\"seed\":{seed},\"content\":\"Lorem ipsum dolor sit amet \"}} "
    );
    let target = kb * 1024;
    let mut s = String::with_capacity(target + unit.len());
    while s.len() < target {
        s.push_str(&unit);
    }
    s.into_bytes()
}

fn attrs<const N: usize>(pairs: [(&str, String); N]) -> HashMap<String, String> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}
