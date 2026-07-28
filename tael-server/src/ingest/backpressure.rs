//! Bounded admission for the ingest paths.
//!
//! Every accept path runs its (blocking) store insert on the request task, so
//! with no bound an ingest burst faster than storage can absorb just stacks
//! requests until memory or file handles run out. This gate caps concurrent
//! in-flight ingest batches process-wide; a batch that can't get a permit is
//! shed immediately with a retryable status (gRPC `RESOURCE_EXHAUSTED`, HTTP
//! `429`) instead of degrading every other request. Well-behaved OTLP
//! exporters retry with backoff, so shedding under pressure loses less data
//! than falling over does.
//!
//! The cap is `TAEL_INGEST_MAX_IN_FLIGHT` (batches; `0` disables the gate).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const DEFAULT_MAX_IN_FLIGHT: usize = 512;

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static LIMIT: OnceLock<usize> = OnceLock::new();

fn limit() -> usize {
    *LIMIT.get_or_init(|| {
        std::env::var("TAEL_INGEST_MAX_IN_FLIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_IN_FLIGHT)
    })
}

/// An admitted batch. Dropping it releases the slot.
pub struct Permit {
    counted: bool,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.counted {
            IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// Try to admit one ingest batch. `None` means the node is at capacity and the
/// caller must shed with a retryable status.
pub fn try_acquire() -> Option<Permit> {
    let limit = limit();
    if limit == 0 {
        return Some(Permit { counted: false });
    }
    let mut current = IN_FLIGHT.load(Ordering::Relaxed);
    loop {
        if current >= limit {
            return None;
        }
        match IN_FLIGHT.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(Permit { counted: true }),
            Err(observed) => current = observed,
        }
    }
}

/// Currently admitted batches (for status reporting).
pub fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::Relaxed)
}

/// The configured cap (`0` = unbounded).
pub fn max_in_flight() -> usize {
    limit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_release_on_drop() {
        let before = in_flight();
        {
            let _permits: Vec<Permit> = (0..3).filter_map(|_| try_acquire()).collect();
            assert!(in_flight() >= before);
        }
        assert_eq!(in_flight(), before);
    }
}
