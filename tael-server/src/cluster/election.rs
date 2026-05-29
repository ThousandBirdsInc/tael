//! Pure leader-election and fencing logic, independent of the gossip transport.
//! Kept free of chitchat so it's deterministic and unit-testable; the
//! [`ClusterCoordinator`](super::ClusterCoordinator) feeds it a membership view.

use std::sync::atomic::{AtomicU64, Ordering};

/// Elect the leader of a replication group from its **live** members: the
/// smallest node id wins. This needs no coordination beyond an (eventually
/// consistent) membership view — every node that sees the same live set picks
/// the same leader, and when the leader dies it drops out of the live set so
/// the next-smallest id is elected automatically.
///
/// Returns `None` only when there are no live members.
pub fn elect_leader(live_node_ids: &[String]) -> Option<&String> {
    live_node_ids.iter().min()
}

/// Monotonic epoch gate that fences a deposed leader out of the WAL stream.
///
/// Each leadership reign carries a strictly higher epoch (a freshly promoted
/// leader bumps past every epoch it has seen). A standby records the highest
/// epoch it has accepted and **rejects any record stamped with a lower epoch**,
/// so a stale leader that kept shipping after losing leadership can't corrupt
/// the new leader's replicas. Records at the current epoch (the same leader's
/// ongoing stream) are accepted; a higher epoch advances the gate.
///
/// This is best-effort fencing layered on an eventually-consistent membership
/// view — it closes the dangerous split-brain window but is not the linearizable
/// guarantee a full consensus log (e.g. Raft) would provide. See
/// `docs/tael-server-scaling-ha.md` §5.1 / Open Q #2.
#[derive(Debug, Default)]
pub struct EpochFencer {
    highest: AtomicU64,
}

impl EpochFencer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a record stamped with `epoch`? Accepts when `epoch >= highest`
    /// seen (advancing the gate on a newer epoch); rejects a strictly older one.
    pub fn check_and_advance(&self, epoch: u64) -> bool {
        let mut cur = self.highest.load(Ordering::Acquire);
        loop {
            if epoch < cur {
                return false;
            }
            if epoch == cur {
                return true;
            }
            match self
                .highest
                .compare_exchange(cur, epoch, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// The highest epoch accepted so far.
    pub fn highest(&self) -> u64 {
        self.highest.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn elects_lowest_live_node_id() {
        assert_eq!(
            elect_leader(&ids(&["node-c", "node-a", "node-b"])),
            Some(&"node-a".to_string())
        );
    }

    #[test]
    fn no_leader_without_live_members() {
        assert_eq!(elect_leader(&[]), None);
    }

    #[test]
    fn failover_picks_next_node_when_leader_leaves() {
        // Leader "node-a" is alive → it leads.
        let full = ids(&["node-a", "node-b", "node-c"]);
        assert_eq!(elect_leader(&full), Some(&"node-a".to_string()));
        // node-a dies (drops from the live set) → node-b is elected, no other
        // coordination needed.
        let after = ids(&["node-b", "node-c"]);
        assert_eq!(elect_leader(&after), Some(&"node-b".to_string()));
    }

    #[test]
    fn fencer_accepts_equal_and_higher_rejects_lower() {
        let f = EpochFencer::new();
        assert!(f.check_and_advance(5), "first record sets the gate");
        assert!(
            f.check_and_advance(5),
            "same epoch (same leader) is accepted"
        );
        assert!(f.check_and_advance(7), "newer epoch advances the gate");
        assert_eq!(f.highest(), 7);
        assert!(
            !f.check_and_advance(6),
            "a deposed leader's older epoch is fenced out"
        );
        assert!(!f.check_and_advance(5));
        assert_eq!(f.highest(), 7, "rejected records don't move the gate");
    }
}
