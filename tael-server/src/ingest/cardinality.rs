//! Metric series cardinality guard.
//!
//! Every distinct `(name, service, label set)` is a series the store must
//! track forever after; one misbehaving exporter interpolating a request ID
//! into a label can mint unbounded series and quietly eat the node. This guard
//! caps the number of distinct series the process will accept: points on
//! series already seen always pass, points that would mint a series beyond the
//! cap are dropped and counted, and the drop is visible in `ingest status`.
//!
//! The cap is `TAEL_METRIC_SERIES_LIMIT` (default 100k distinct series,
//! `0` disables the guard). The seen-set is process-local and resets on
//! restart — the guard bounds growth, it is not an exact registry.

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::storage::models::MetricPoint;

const DEFAULT_SERIES_LIMIT: usize = 100_000;

static SEEN: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static LIMIT: OnceLock<usize> = OnceLock::new();

fn limit() -> usize {
    *LIMIT.get_or_init(|| {
        std::env::var("TAEL_METRIC_SERIES_LIMIT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_SERIES_LIMIT)
    })
}

/// The identity of a point's series: name, service, and full label set,
/// order-independent.
fn series_hash(point: &MetricPoint) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    point.name.hash(&mut hasher);
    point.service.hash(&mut hasher);
    let mut labels: Vec<_> = point.attributes.iter().collect();
    labels.sort();
    labels.hash(&mut hasher);
    hasher.finish()
}

/// Drop points that would mint a series beyond the cap. Returns how many were
/// dropped; the first drop of a batch logs a warning naming the metric so the
/// offender is findable.
pub fn admit(points: &mut Vec<MetricPoint>) -> u64 {
    let limit = limit();
    if limit == 0 || points.is_empty() {
        return 0;
    }
    let mut guard = SEEN.lock().unwrap();
    let seen = guard.get_or_insert_with(HashSet::new);
    let before = points.len();
    let mut first_dropped: Option<String> = None;
    points.retain(|p| {
        let h = series_hash(p);
        if seen.contains(&h) {
            return true;
        }
        if seen.len() >= limit {
            if first_dropped.is_none() {
                first_dropped = Some(p.name.clone());
            }
            return false;
        }
        seen.insert(h);
        true
    });
    let dropped = (before - points.len()) as u64;
    if let Some(name) = first_dropped {
        DROPPED.fetch_add(dropped, Ordering::Relaxed);
        tracing::warn!(
            metric = %name,
            dropped,
            series_limit = limit,
            "metric series cap reached: dropping points that would mint new series \
             (raise TAEL_METRIC_SERIES_LIMIT or fix the high-cardinality label)"
        );
    }
    dropped
}

/// Distinct series seen by this process.
pub fn tracked_series() -> usize {
    SEEN.lock().unwrap().as_ref().map_or(0, HashSet::len)
}

/// Total points dropped by the cap since the process started.
pub fn dropped_points() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// The configured cap (`0` = unbounded).
pub fn series_limit() -> usize {
    limit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    fn point(name: &str, label: &str) -> MetricPoint {
        MetricPoint {
            timestamp: Utc::now(),
            service: "svc".into(),
            name: name.into(),
            metric_type: crate::storage::models::MetricType::Gauge,
            value: 1.0,
            unit: String::new(),
            attributes: HashMap::from([("k".to_string(), label.to_string())]),
            histogram: None,
        }
    }

    #[test]
    fn same_series_hashes_equal_and_label_order_is_irrelevant() {
        let mut a = point("m", "v");
        a.attributes.insert("z".into(), "1".into());
        let mut b = point("m", "v");
        b.attributes.insert("z".into(), "1".into());
        assert_eq!(series_hash(&a), series_hash(&b));
        assert_ne!(series_hash(&a), series_hash(&point("m", "other")));
        assert_ne!(series_hash(&point("m", "v")), series_hash(&point("n", "v")));
    }
}
