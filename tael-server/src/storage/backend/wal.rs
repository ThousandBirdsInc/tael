//! Write-ahead log for `TaelBackend`, built on `walrus-rust`.
//!
//! Every insert is framed `[version: u8][signal tag: u8][MessagePack batch]`
//! and appended to a single topic, so one WAL covers all three signals and
//! replay routes each record by tag. The discipline is the standard redo log:
//!
//!   append (durable) → apply to the projection → consume (advance cursor)
//!
//! **The cursor advance is not on the write path.** walrus persists the read
//! cursor by rewriting and fsyncing an index file, and doing that per insert
//! cost ~1.9ms against ~16µs for the append and apply combined — it *was* the
//! write path. Instead every applied record is counted, and once enough have
//! accumulated a [`WalLog::checkpoint`] consumes them all at once. Between
//! checkpoints the WAL holds records that are already applied; a crash replays
//! them, which is why apply must be idempotent for all three signals (span,
//! log, and metric hot-tier keys are all functions of record content — see
//! `hot.rs`). Replaying at most [`CHECKPOINT_INTERVAL`] already-applied records
//! on boot is the price of not fsyncing an index file per write, and it is a
//! good trade.
//!
//! Checkpointing is exact rather than best-effort: it gates new appends, waits
//! for every in-flight record to finish applying, durably flushes the
//! projection, and only then advances the cursor. Consuming a record that was
//! appended but not yet applied would lose it on a crash, so the wait is the
//! correctness argument and not an optimization.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use walrus_rust::{ReadConsistency, Walrus};

use super::codec;
use crate::storage::models::{LogRecord, MetricPoint, Span};

const TOPIC: &str = "tael_wal";
const WAL_VERSION: u8 = 2;

/// How many applied records may accumulate before a checkpoint is due.
///
/// Sets both how much WAL a crash replays and how often a writer pays the
/// checkpoint's drain. Low enough that boot replay stays imperceptible, high
/// enough that the fsync it exists to avoid is amortized ~1000×.
pub(super) const CHECKPOINT_INTERVAL: u64 = 1024;

/// How many payload bytes one batched cursor advance may cover.
///
/// A checkpoint consumes with `batch_read_for_topic` rather than a `read_next`
/// loop for two reasons. It persists the read cursor once per batch instead of
/// once per entry, which is the difference between one fsync per checkpoint and
/// a thousand. And a `read_next` that runs off the end of the log re-persists
/// the *start* of the current block — silently undoing everything the drain
/// just consumed — whereas an empty batch read persists nothing at all.
const DRAIN_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// How long a checkpoint waits for in-flight writes to finish applying before
/// giving up and leaving the cursor where it is.
///
/// A writer whose replication failed leaves its record appended and never
/// applied on purpose, so that record must never be consumed. Waiting forever
/// would wedge every subsequent write behind the append gate; giving up leaves
/// the WAL longer than we would like and retries at the next checkpoint, which
/// is the right way round.
const QUIESCE_TIMEOUT: Duration = Duration::from_secs(5);

/// Receipt for an appended record, redeemed by [`WalLog::mark_applied`] once
/// the record is in the projection.
///
/// Not `Copy` and not constructible outside this module: the in-flight count it
/// represents has to be decremented exactly once, or a checkpoint waits on a
/// write that already finished.
#[must_use = "an appended record must be marked applied or checkpoints will stall"]
pub struct WalTicket(());

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
    /// Durably accept one framed WAL record (`[version][tag][msgpack]`).
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
    /// Decode a framed record (`[version][tag][msgpack]`) produced by a
    /// leader's [`WalLog`] append/ship path — the standby half of the one
    /// shared codec.
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
    /// Records appended but not yet applied, and records applied but not yet
    /// consumed. A checkpoint needs the first to be zero and drains the second.
    progress: Mutex<Progress>,
    /// Signalled whenever `in_flight` reaches zero, so a waiting checkpoint
    /// wakes immediately instead of polling.
    quiesced: Condvar,
}

#[derive(Default)]
struct Progress {
    /// Appended, not yet applied. Consuming any of these would lose them.
    in_flight: u64,
    /// Applied, not yet consumed. This is what a checkpoint drains.
    applied: u64,
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
        // `StrictlyAtOnce` persists the cursor on every consuming read, which
        // sounds expensive and is not: consumption only happens in checkpoints,
        // and those consume in batches that persist once each. The relaxed mode
        // would batch the persist a second time and lose the exact position.
        let wal = Walrus::with_consistency_for_key(key, ReadConsistency::StrictlyAtOnce)?;
        let required_acks = sinks.len();
        Ok(Self {
            wal: Mutex::new(wal),
            sinks,
            required_acks,
            progress: Mutex::new(Progress::default()),
            quiesced: Condvar::new(),
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
    pub fn append_framed(&self, framed: &[u8]) -> Result<WalTicket> {
        self.write_local(framed)
    }

    fn write_local(&self, framed: &[u8]) -> Result<WalTicket> {
        // The append gate: a checkpoint holds this lock for its whole drain, so
        // no record can be appended while the cursor is moving.
        let wal = self.wal.lock().unwrap();
        wal.append_for_topic(TOPIC, framed)?;
        self.progress.lock().unwrap().in_flight += 1;
        Ok(WalTicket(()))
    }

    fn append(&self, tag: u8, payload: &[u8]) -> Result<WalTicket> {
        let framed = frame(tag, payload);
        // Local durability first, then ship to standbys before returning.
        let ticket = self.write_local(&framed)?;
        if self.sinks.is_empty() {
            return Ok(ticket);
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
            // The record is locally durable but the caller will not apply it,
            // so its ticket is never redeemed and it stays in flight — exactly
            // the state that must block a checkpoint from consuming it. It
            // clears when the process restarts and replay applies it.
            bail!(
                "WAL replication underreplicated: {acks}/{} standbys acked, need {}",
                self.sinks.len(),
                self.required_acks
            );
        }
        Ok(ticket)
    }

    pub fn append_spans(&self, spans: &[Span]) -> Result<WalTicket> {
        self.append(TAG_SPANS, &codec::encode(&spans)?)
    }

    pub fn append_logs(&self, logs: &[LogRecord]) -> Result<WalTicket> {
        self.append(TAG_LOGS, &codec::encode(&logs)?)
    }

    pub fn append_metrics(&self, metrics: &[MetricPoint]) -> Result<WalTicket> {
        self.append(TAG_METRICS, &codec::encode(&metrics)?)
    }

    /// Redeem a ticket: the record is in the projection and may be consumed by
    /// the next checkpoint. Returns whether enough have accumulated that a
    /// checkpoint is now due.
    ///
    /// Deliberately does no I/O — that is the whole point of the split.
    pub fn mark_applied(&self, ticket: WalTicket) -> bool {
        let _ = ticket;
        let mut p = self.progress.lock().unwrap();
        p.in_flight -= 1;
        p.applied += 1;
        let applied = p.applied;
        if p.in_flight == 0 {
            self.quiesced.notify_all();
        }
        applied >= CHECKPOINT_INTERVAL
    }

    /// Advance the durable read cursor past everything that has been applied.
    ///
    /// `flush_projection` runs after writes are gated and quiesced but *before*
    /// the cursor moves — it is the caller's chance to make the applied state
    /// durable. Ordering it any other way would leave a window where a record
    /// is no longer in the WAL and not yet on disk in the projection.
    ///
    /// Returns the number of entries consumed. Zero means there was nothing to
    /// do, or that an in-flight write did not finish within
    /// [`QUIESCE_TIMEOUT`]; both are safe and retried next time.
    pub fn checkpoint<F>(&self, flush_projection: F) -> Result<usize>
    where
        F: FnOnce() -> Result<()>,
    {
        // Held for the whole checkpoint: new appends block here, so once the
        // in-flight count hits zero it stays zero.
        let wal = self.wal.lock().unwrap();

        let mut progress = self.progress.lock().unwrap();
        while progress.in_flight > 0 {
            let (guard, timeout) = self
                .quiesced
                .wait_timeout(progress, QUIESCE_TIMEOUT)
                .unwrap();
            progress = guard;
            if timeout.timed_out() && progress.in_flight > 0 {
                tracing::warn!(
                    in_flight = progress.in_flight,
                    "tael-backend: WAL checkpoint skipped, writes still in flight"
                );
                return Ok(0);
            }
        }
        if progress.applied == 0 {
            return Ok(0);
        }
        // Release before the drain: `mark_applied` takes this lock, and while
        // no *new* record can be appended, keeping it would serialize nothing
        // useful and widens the window for a lock-order mistake later.
        drop(progress);

        flush_projection()?;

        // Everything ever appended is now applied and durable, so consuming to
        // the end of the log is exactly right.
        let mut consumed = 0usize;
        loop {
            let batch = wal.batch_read_for_topic(TOPIC, DRAIN_BATCH_BYTES, true)?;
            if batch.is_empty() {
                break;
            }
            consumed += batch.len();
        }

        let mut progress = self.progress.lock().unwrap();
        progress.applied = 0;
        Ok(consumed)
    }

    /// Drain and decode every currently-unconsumed entry, advancing the cursor.
    /// Used on startup to replay the crash-gap.
    pub fn drain(&self) -> Result<Vec<WalRecord>> {
        let wal = self.wal.lock().unwrap();
        let mut out = Vec::new();
        loop {
            let batch = wal.batch_read_for_topic(TOPIC, DRAIN_BATCH_BYTES, true)?;
            if batch.is_empty() {
                break;
            }
            for entry in batch {
                out.push(decode(&entry.data)?);
            }
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
        TAG_SPANS => WalRecord::Spans(codec::decode(payload)?),
        TAG_LOGS => WalRecord::Logs(codec::decode(payload)?),
        TAG_METRICS => WalRecord::Metrics(codec::decode(payload)?),
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
            let _ = wal.append_spans(&[test_span("a"), test_span("b")]).unwrap();
            let _ = wal.append_spans(&[test_span("c")]).unwrap();
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
    fn applied_records_replay_until_a_checkpoint_consumes_them() {
        // The core of taking the cursor advance off the write path: applying a
        // record is not what makes it stop replaying, checkpointing is.
        let key = unique_key("applied");
        {
            let wal = WalLog::new_for_key(&key).unwrap();
            let ticket = wal.append_spans(&[test_span("a")]).unwrap();
            wal.mark_applied(ticket);
        }
        {
            let wal = WalLog::new_for_key(&key).unwrap();
            assert_eq!(
                wal.drain().unwrap().len(),
                1,
                "an applied but un-checkpointed record still replays"
            );
        }

        // Checkpointing consumes them, durably. The count is well past a single
        // batch so this also covers the multi-batch drain, where an earlier
        // implementation lost the cursor on the read that found the end.
        let total = CHECKPOINT_INTERVAL as usize;
        let key = unique_key("checkpointed");
        {
            let wal = WalLog::new_for_key(&key).unwrap();
            for i in 0..total {
                let ticket = wal.append_spans(&[test_span(&i.to_string())]).unwrap();
                wal.mark_applied(ticket);
            }
            assert_eq!(wal.checkpoint(|| Ok(())).unwrap(), total);
        }
        let wal = WalLog::new_for_key(&key).unwrap();
        assert!(
            wal.drain().unwrap().is_empty(),
            "checkpointed records must not replay after a restart"
        );
    }

    #[test]
    fn a_checkpoint_flushes_the_projection_before_moving_the_cursor() {
        // Ordering is the durability argument: if the cursor moved first, a
        // crash in between would drop a record the projection had not yet
        // written. The flush failing must therefore abort the whole checkpoint.
        let key = unique_key("flush-order");
        let wal = WalLog::new_for_key(&key).unwrap();
        let ticket = wal.append_spans(&[test_span("a")]).unwrap();
        wal.mark_applied(ticket);

        assert!(
            wal.checkpoint(|| bail!("projection flush failed")).is_err(),
            "a failed flush must fail the checkpoint"
        );
        assert_eq!(
            wal.drain().unwrap().len(),
            1,
            "the record must still be in the WAL after a failed checkpoint"
        );
    }

    #[test]
    fn a_checkpoint_will_not_consume_a_record_that_is_still_in_flight() {
        // A record that has been appended but not applied lives only in the
        // WAL. Consuming it would lose it outright, so the checkpoint has to
        // decline — and this is the one case where doing nothing is correct.
        let key = unique_key("in-flight");
        let wal = WalLog::new_for_key(&key).unwrap();
        let applied = wal.append_spans(&[test_span("a")]).unwrap();
        wal.mark_applied(applied);
        // Appended and deliberately never marked applied.
        let _in_flight = wal.append_spans(&[test_span("b")]).unwrap();

        let waited = std::time::Instant::now();
        assert_eq!(
            wal.checkpoint(|| Ok(())).unwrap(),
            0,
            "a checkpoint must not consume past an in-flight write"
        );
        assert!(
            waited.elapsed() >= QUIESCE_TIMEOUT,
            "it should wait for the write before giving up"
        );
        assert_eq!(
            wal.drain().unwrap().len(),
            2,
            "both records must survive to be replayed"
        );
    }

    #[test]
    fn concurrent_writers_checkpoint_without_losing_records() {
        // The append gate and the quiesce wait have to hold under the access
        // pattern they exist for: many threads appending while one of them
        // trips a checkpoint.
        let key = unique_key("concurrent");
        let wal = Arc::new(WalLog::new_for_key(&key).unwrap());
        let checkpointed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for t in 0..8 {
                let wal = Arc::clone(&wal);
                let checkpointed = Arc::clone(&checkpointed);
                scope.spawn(move || {
                    for i in 0..64 {
                        let ticket = wal.append_spans(&[test_span(&format!("{t}-{i}"))]).unwrap();
                        if wal.mark_applied(ticket) {
                            let n = wal.checkpoint(|| Ok(())).unwrap();
                            checkpointed.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        // Whatever a checkpoint did not consume is still in the WAL, and every
        // record must be in exactly one of those two places.
        let remaining = wal.drain().unwrap().len();
        assert_eq!(
            checkpointed.load(std::sync::atomic::Ordering::Relaxed) + remaining,
            8 * 64,
            "no record may be consumed twice or lost"
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
        let _ = wal.append_spans(&[test_span("a"), test_span("b")]).unwrap();
        let _ = wal.append_logs(&[]).unwrap();
        let _ = wal.append_metrics(&[]).unwrap();

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
        let _ = wal.append_spans(&[test_span("a")]).unwrap();
        let _ = wal.append_logs(&[]).unwrap();
        let _ = wal.append_metrics(&[]).unwrap();
        let records = wal.drain().unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0], WalRecord::Spans(_)));
        assert!(matches!(records[1], WalRecord::Logs(_)));
        assert!(matches!(records[2], WalRecord::Metrics(_)));
    }
}
