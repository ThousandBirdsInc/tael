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

use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use walrus_rust::Walrus;

use crate::storage::models::{LogRecord, MetricPoint, Span};

const TOPIC: &str = "tael_wal";
const WAL_VERSION: u8 = 1;

const TAG_SPANS: u8 = 1;
const TAG_LOGS: u8 = 2;
const TAG_METRICS: u8 = 3;

/// A replication target for the WAL. The leader hands each appended record's
/// framed bytes (the wire format below) to every registered sink before its
/// write returns — the replicate-before-ack guarantee that lets a standby
/// survive the leader's *loss*, not just its crash
/// (`docs/tael-server-scaling-ha.md` §5.1). A sink's `append_framed` must only
/// return once the record is durable at the sink (a standby ack, an fsync).
///
/// This is the seam the WAL-shipping layer plugs into; the network transport
/// and failover/promotion live above it and are intentionally not defined here.
pub trait WalSink: Send + Sync {
    /// Durably accept one framed WAL record (`[version][tag][json]`).
    fn append_framed(&self, framed: &[u8]) -> Result<()>;
    /// Human-readable name for diagnostics (e.g. the standby's address).
    fn name(&self) -> &str {
        "wal-sink"
    }
}

/// A decoded WAL record — one signal's batch.
#[derive(Debug)]
pub enum WalRecord {
    Spans(Vec<Span>),
    Logs(Vec<LogRecord>),
    Metrics(Vec<MetricPoint>),
}

impl WalRecord {
    /// Decode a framed record (`[version][tag][json]`) produced by a leader's
    /// [`WalLog`] append/ship path — the standby half of the one shared codec.
    pub fn decode(bytes: &[u8]) -> Result<WalRecord> {
        decode(bytes)
    }
}

/// Prepend the version + signal tag to a serialized batch.
fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(payload.len() + 2);
    framed.push(WAL_VERSION);
    framed.push(tag);
    framed.extend_from_slice(payload);
    framed
}

pub struct WalLog {
    wal: Mutex<Walrus>,
    /// Replication targets. Empty by default → no behavior change; populated to
    /// turn on WAL shipping (§5.1).
    sinks: Vec<Arc<dyn WalSink>>,
    /// How many sinks must ack an append before it returns. Defaults to all
    /// sinks (fully synchronous replication: a write survives node loss because
    /// every standby has it before ack). Lower it for semi-sync (ack after a
    /// subset) or set 0 for async best-effort (never block on a standby).
    required_acks: usize,
}

impl WalLog {
    /// Open (or create) a WAL namespaced by `key`, isolated from the span/log
    /// buses and from other instances/tests.
    pub fn new_for_key(key: &str) -> Result<Self> {
        Self::new_for_key_with_sinks(key, Vec::new())
    }

    /// Like [`Self::new_for_key`] but with replication sinks attached — the
    /// leader ships every appended record to each sink before acking. Defaults
    /// to fully synchronous (`required_acks` = all sinks); adjust with
    /// [`Self::with_required_acks`].
    pub fn new_for_key_with_sinks(key: &str, sinks: Vec<Arc<dyn WalSink>>) -> Result<Self> {
        let wal = Walrus::new_for_key(key)?;
        let required_acks = sinks.len();
        Ok(Self {
            wal: Mutex::new(wal),
            sinks,
            required_acks,
        })
    }

    /// Set how many sinks must ack each append. Clamped to the number of sinks.
    pub fn with_required_acks(mut self, n: usize) -> Self {
        self.required_acks = n.min(self.sinks.len());
        self
    }

    /// Append framed bytes to the local walrus namespace only (no sink
    /// fan-out). The standby path: persist a record shipped from a leader
    /// verbatim, so the standby's own WAL stays a faithful, replayable copy.
    pub fn append_framed(&self, framed: &[u8]) -> Result<()> {
        self.write_local(framed)
    }

    fn write_local(&self, framed: &[u8]) -> Result<()> {
        let wal = self.wal.lock().unwrap();
        wal.append_for_topic(TOPIC, framed)?;
        Ok(())
    }

    fn append(&self, tag: u8, payload: &[u8]) -> Result<()> {
        let framed = frame(tag, payload);
        // Local durability first, then ship to standbys before returning.
        self.write_local(&framed)?;
        if self.sinks.is_empty() {
            return Ok(());
        }
        // Ship to every standby; tolerate individual failures and only fail the
        // write if fewer than `required_acks` standbys confirmed (a down standby
        // under semi-sync/async must not take down the leader). On failure the
        // record stays in the local WAL un-applied, so a retry/restart replays
        // it — no data loss.
        let mut acks = 0usize;
        for sink in &self.sinks {
            match sink.append_framed(&framed) {
                Ok(()) => acks += 1,
                Err(e) => {
                    tracing::warn!(sink = sink.name(), error = %e, "WAL ship to standby failed")
                }
            }
        }
        if acks < self.required_acks {
            bail!(
                "WAL replication underreplicated: {acks}/{} standbys acked, need {}",
                self.sinks.len(),
                self.required_acks
            );
        }
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
    fn sink_receives_framed_bytes_that_decode_per_signal() {
        use std::sync::Mutex as StdMutex;
        // A sink that captures every framed record the leader ships. This also
        // exercises the production framing path end to end: append → frame →
        // ship → decode round-trips for each signal.
        struct CaptureSink(Arc<StdMutex<Vec<Vec<u8>>>>);
        impl WalSink for CaptureSink {
            fn append_framed(&self, framed: &[u8]) -> Result<()> {
                self.0.lock().unwrap().push(framed.to_vec());
                Ok(())
            }
        }
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let key = unique_key("sink");
        let wal =
            WalLog::new_for_key_with_sinks(&key, vec![Arc::new(CaptureSink(captured.clone()))])
                .unwrap();
        wal.append_spans(&[test_span("a"), test_span("b")]).unwrap();
        wal.append_logs(&[]).unwrap();
        wal.append_metrics(&[]).unwrap();

        let frames = captured.lock().unwrap();
        assert_eq!(frames.len(), 3, "every append ships to the sink");
        match WalRecord::decode(&frames[0]).unwrap() {
            WalRecord::Spans(s) => assert_eq!(s.len(), 2),
            other => panic!("expected spans, got {other:?}"),
        }
        assert!(matches!(
            WalRecord::decode(&frames[1]).unwrap(),
            WalRecord::Logs(_)
        ));
        assert!(matches!(
            WalRecord::decode(&frames[2]).unwrap(),
            WalRecord::Metrics(_)
        ));
    }

    #[test]
    fn required_acks_governs_whether_a_down_standby_blocks_writes() {
        // A sink that always fails, standing in for an unreachable standby.
        struct DeadSink;
        impl WalSink for DeadSink {
            fn append_framed(&self, _framed: &[u8]) -> Result<()> {
                bail!("standby unreachable")
            }
        }

        // Synchronous (required_acks defaults to all = 1): the down standby
        // fails the write.
        let key_sync = unique_key("acks-sync");
        let sync = WalLog::new_for_key_with_sinks(&key_sync, vec![Arc::new(DeadSink)]).unwrap();
        assert!(
            sync.append_spans(&[test_span("a")]).is_err(),
            "synchronous replication must fail when the only standby is down"
        );

        // Async best-effort (required_acks = 0): the write still succeeds; the
        // record stays locally durable for later replay.
        let key_async = unique_key("acks-async");
        let r#async = WalLog::new_for_key_with_sinks(&key_async, vec![Arc::new(DeadSink)])
            .unwrap()
            .with_required_acks(0);
        assert!(
            r#async.append_spans(&[test_span("a")]).is_ok(),
            "async replication must not block on a down standby"
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
