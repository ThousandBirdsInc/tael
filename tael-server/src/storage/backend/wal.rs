//! Write-ahead log for `TaelBackend`, built on `walrus-rust`.
//!
//! Every insert is framed `[version: u8][signal tag: u8][JSON batch]` and
//! appended to a single topic, so one WAL covers all three signals and replay
//! routes each record by tag. We use the standard redo-log discipline:
//!
//!   append (durable) → apply to the projection → consume (advance cursor)
//!
//! walrus persists the read cursor, so at rest the WAL has nothing unconsumed.
//! After a crash, any entry that was appended but not yet consumed (the
//! in-flight window) is replayed on the next boot via [`WalLog::drain`]. Span
//! re-apply is idempotent (`INSERT OR REPLACE`); logs/metrics carry a small
//! double-apply risk only in the apply→consume crash window (acceptable for
//! v1; eliminated once the LSM hot tier owns the projection in Phase 4).

use std::sync::Mutex;

use anyhow::{Result, bail};
use walrus_rust::Walrus;

use crate::storage::models::{LogRecord, MetricPoint, Span};

const TOPIC: &str = "tael_wal";
const WAL_VERSION: u8 = 1;

const TAG_SPANS: u8 = 1;
const TAG_LOGS: u8 = 2;
const TAG_METRICS: u8 = 3;

/// A decoded WAL record — one signal's batch.
#[derive(Debug)]
pub enum WalRecord {
    Spans(Vec<Span>),
    Logs(Vec<LogRecord>),
    Metrics(Vec<MetricPoint>),
}

pub struct WalLog {
    wal: Mutex<Walrus>,
}

impl WalLog {
    /// Open (or create) a WAL namespaced by `key`, isolated from the span/log
    /// buses and from other instances/tests.
    pub fn new_for_key(key: &str) -> Result<Self> {
        let wal = Walrus::new_for_key(key)?;
        Ok(Self {
            wal: Mutex::new(wal),
        })
    }

    fn append(&self, tag: u8, payload: &[u8]) -> Result<()> {
        let mut framed = Vec::with_capacity(payload.len() + 2);
        framed.push(WAL_VERSION);
        framed.push(tag);
        framed.extend_from_slice(payload);
        let wal = self.wal.lock().unwrap();
        wal.append_for_topic(TOPIC, &framed)?;
        Ok(())
    }

    pub fn append_spans(&self, spans: &[Span]) -> Result<()> {
        self.append(TAG_SPANS, &serde_json::to_vec(spans)?)
    }

    pub fn append_logs(&self, logs: &[LogRecord]) -> Result<()> {
        self.append(TAG_LOGS, &serde_json::to_vec(logs)?)
    }

    pub fn append_metrics(&self, metrics: &[MetricPoint]) -> Result<()> {
        self.append(TAG_METRICS, &serde_json::to_vec(metrics)?)
    }

    /// Consume one entry (advance the durable read cursor past an applied
    /// record). Call after a successful apply.
    pub fn mark_applied(&self) -> Result<()> {
        let wal = self.wal.lock().unwrap();
        wal.read_next(TOPIC, true)?;
        Ok(())
    }

    /// Drain and decode every currently-unconsumed entry, advancing the cursor.
    /// Used on startup to replay the crash-gap.
    pub fn drain(&self) -> Result<Vec<WalRecord>> {
        let wal = self.wal.lock().unwrap();
        let mut out = Vec::new();
        while let Some(entry) = wal.read_next(TOPIC, true)? {
            out.push(decode(&entry.data)?);
        }
        Ok(out)
    }
}

fn decode(bytes: &[u8]) -> Result<WalRecord> {
    if bytes.len() < 2 {
        bail!("WAL record too short: {} bytes", bytes.len());
    }
    let version = bytes[0];
    if version != WAL_VERSION {
        bail!("unsupported WAL record version {version}");
    }
    let tag = bytes[1];
    let payload = &bytes[2..];
    Ok(match tag {
        TAG_SPANS => WalRecord::Spans(serde_json::from_slice(payload)?),
        TAG_LOGS => WalRecord::Logs(serde_json::from_slice(payload)?),
        TAG_METRICS => WalRecord::Metrics(serde_json::from_slice(payload)?),
        other => bail!("unknown WAL signal tag {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{SpanKind, SpanStatus};
    use chrono::Utc;

    fn test_span(id: &str) -> Span {
        let now = Utc::now();
        Span {
            trace_id: id.into(),
            span_id: format!("{id}-s"),
            parent_span_id: None,
            service: "svc".into(),
            operation: "op".into(),
            start_time: now,
            end_time: now,
            duration_ms: 1.0,
            status: SpanStatus::Ok,
            attributes: Default::default(),
            events: vec![],
            kind: SpanKind::Internal,
            llm: None,
        }
    }

    /// A unique key per test run so walrus namespaces don't collide. The
    /// returned guard removes the on-disk namespace (`wal_files/<key>`) on drop.
    fn unique_key(name: &str) -> KeyGuard {
        KeyGuard(format!("tael-test-{name}-{}", uuid::Uuid::new_v4()))
    }

    struct KeyGuard(String);
    impl std::ops::Deref for KeyGuard {
        type Target = str;
        fn deref(&self) -> &str {
            &self.0
        }
    }
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.0));
        }
    }

    #[test]
    fn appended_records_replay_after_reopen() {
        let key = unique_key("replay");
        {
            let wal = WalLog::new_for_key(&key).unwrap();
            wal.append_spans(&[test_span("a"), test_span("b")]).unwrap();
            wal.append_spans(&[test_span("c")]).unwrap();
            // Simulate a crash: never mark_applied, drop the handle.
        }
        let wal = WalLog::new_for_key(&key).unwrap();
        let records = wal.drain().unwrap();
        let total: usize = records
            .iter()
            .map(|r| match r {
                WalRecord::Spans(s) => s.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total, 3, "all appended spans should replay");
    }

    #[test]
    fn applied_records_are_not_replayed() {
        let key = unique_key("applied");
        {
            let wal = WalLog::new_for_key(&key).unwrap();
            wal.append_spans(&[test_span("a")]).unwrap();
            wal.mark_applied().unwrap(); // consumed after a (simulated) apply
        }
        let wal = WalLog::new_for_key(&key).unwrap();
        assert!(
            wal.drain().unwrap().is_empty(),
            "consumed records must not replay"
        );
    }

    #[test]
    fn tagged_records_decode_to_their_signal() {
        let key = unique_key("tags");
        let wal = WalLog::new_for_key(&key).unwrap();
        wal.append_spans(&[test_span("a")]).unwrap();
        wal.append_logs(&[]).unwrap();
        wal.append_metrics(&[]).unwrap();
        let records = wal.drain().unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], WalRecord::Spans(_)));
        assert!(matches!(records[1], WalRecord::Logs(_)));
        assert!(matches!(records[2], WalRecord::Metrics(_)));
    }
}
