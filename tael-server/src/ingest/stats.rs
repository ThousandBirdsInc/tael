//! Process-wide ingest pipeline counters backing `tael ingest status`.
//!
//! Every accept path increments a counter here at the same place it calls the
//! store, so the numbers reflect what was actually persisted, not what arrived
//! on the wire. Counters are process-global: they reset on restart and are not
//! replicated, which is the right scope for "is this node receiving data".

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use serde::Serialize;

/// One ingest pipeline (a protocol/signal pair the server accepts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipeline {
    /// OTLP spans (gRPC :4317 and HTTP :4318 share the implementation).
    OtlpSpans,
    /// OTLP log records.
    OtlpLogs,
    /// OTLP metric points.
    OtlpMetrics,
    /// Prometheus remote-write metric points.
    RemoteWrite,
    /// Datadog trace-agent spans.
    Datadog,
}

#[derive(Default)]
struct Counter {
    batches: AtomicU64,
    records: AtomicU64,
    errors: AtomicU64,
    /// Batches refused at admission because the node was at capacity.
    shed: AtomicU64,
    /// Unix millis of the last accepted batch; 0 = never.
    last_accepted_ms: AtomicI64,
}

#[derive(Debug, Serialize)]
pub struct PipelineStatus {
    pub pipeline: &'static str,
    pub batches: u64,
    pub records: u64,
    pub errors: u64,
    pub shed: u64,
    /// RFC3339 time of the last accepted batch, absent when nothing has
    /// arrived since the process started.
    pub last_accepted_at: Option<String>,
}

static OTLP_SPANS: Counter = Counter::new();
static OTLP_LOGS: Counter = Counter::new();
static OTLP_METRICS: Counter = Counter::new();
static REMOTE_WRITE: Counter = Counter::new();
static DATADOG: Counter = Counter::new();

impl Counter {
    const fn new() -> Self {
        Self {
            batches: AtomicU64::new(0),
            records: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            shed: AtomicU64::new(0),
            last_accepted_ms: AtomicI64::new(0),
        }
    }

    fn status(&self, pipeline: &'static str) -> PipelineStatus {
        let last_ms = self.last_accepted_ms.load(Ordering::Relaxed);
        PipelineStatus {
            pipeline,
            batches: self.batches.load(Ordering::Relaxed),
            records: self.records.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            shed: self.shed.load(Ordering::Relaxed),
            last_accepted_at: (last_ms > 0)
                .then(|| chrono::DateTime::from_timestamp_millis(last_ms))
                .flatten()
                .map(|t| t.to_rfc3339()),
        }
    }
}

fn counter(pipeline: Pipeline) -> &'static Counter {
    match pipeline {
        Pipeline::OtlpSpans => &OTLP_SPANS,
        Pipeline::OtlpLogs => &OTLP_LOGS,
        Pipeline::OtlpMetrics => &OTLP_METRICS,
        Pipeline::RemoteWrite => &REMOTE_WRITE,
        Pipeline::Datadog => &DATADOG,
    }
}

/// Record a successfully persisted batch of `records` records.
pub fn record_accepted(pipeline: Pipeline, records: usize) {
    let c = counter(pipeline);
    c.batches.fetch_add(1, Ordering::Relaxed);
    c.records.fetch_add(records as u64, Ordering::Relaxed);
    c.last_accepted_ms
        .store(chrono::Utc::now().timestamp_millis(), Ordering::Relaxed);
}

/// Record a batch that failed to persist.
pub fn record_error(pipeline: Pipeline) {
    counter(pipeline).errors.fetch_add(1, Ordering::Relaxed);
}

/// Record a batch shed at admission (backpressure).
pub fn record_shed(pipeline: Pipeline) {
    counter(pipeline).shed.fetch_add(1, Ordering::Relaxed);
}

/// A snapshot of every pipeline, in a fixed order.
pub fn snapshot() -> Vec<PipelineStatus> {
    vec![
        OTLP_SPANS.status("otlp_spans"),
        OTLP_LOGS.status("otlp_logs"),
        OTLP_METRICS.status("otlp_metrics"),
        REMOTE_WRITE.status("remote_write"),
        DATADOG.status("datadog"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_batches_accumulate_and_stamp_a_time() {
        record_accepted(Pipeline::Datadog, 7);
        record_accepted(Pipeline::Datadog, 3);
        record_error(Pipeline::Datadog);
        let dd = snapshot()
            .into_iter()
            .find(|p| p.pipeline == "datadog")
            .unwrap();
        assert!(dd.batches >= 2);
        assert!(dd.records >= 10);
        assert!(dd.errors >= 1);
        assert!(dd.last_accepted_at.is_some());
    }

    #[test]
    fn untouched_counters_report_no_last_accepted_time() {
        let c = Counter::new();
        let s = c.status("fresh");
        assert_eq!(s.batches, 0);
        assert_eq!(s.last_accepted_at, None);
    }
}
