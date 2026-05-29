//! Cluster coordination for WAL replication HA: chitchat-based failure
//! detection + deterministic leader election + epoch fencing
//! (`docs/tael-server-scaling-ha.md` §5.1, Open Q #2).
//!
//! A replication group (a shard's leader + standbys) forms one chitchat cluster
//! (gossip over UDP, phi-accrual failure detection). The leader is the live
//! member with the smallest node id ([`election::elect_leader`]) — no external
//! coordinator, no quorum service. When the leader dies it drops out of the
//! live set and the next-smallest id is elected automatically.
//!
//! To keep a deposed leader from corrupting replicas (split-brain), each
//! leadership reign carries a strictly increasing **epoch**: a freshly promoted
//! leader bumps past every epoch it has seen and stamps it on shipped WAL
//! records; standbys fence out anything older ([`EpochFencer`]). This is
//! best-effort fencing on an eventually-consistent membership view — it closes
//! the dangerous window but isn't the linearizable guarantee a consensus log
//! (Raft) gives. That tradeoff is the reason chitchat, not a broker/Raft, was
//! chosen: embedded, no external infra, good enough for leader/standby failover.

mod election;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use chitchat::transport::UdpTransport;
use chitchat::{ChitchatConfig, ChitchatHandle, ChitchatId, FailureDetectorConfig};

pub use election::EpochFencer;
use election::elect_leader;

const EPOCH_KEY: &str = "epoch";
const ROLE_KEY: &str = "role";
/// How often we recompute leadership from the gossip membership view.
const ELECTION_TICK: Duration = Duration::from_millis(1000);

/// Configuration for joining a replication group's gossip cluster.
pub struct ClusterConfig {
    /// Stable, unique node id within the group (election orders on this).
    pub node_id: String,
    /// UDP address to bind the gossip listener.
    pub listen_addr: SocketAddr,
    /// Address peers should reach this node on (defaults to `listen_addr`).
    pub advertise_addr: SocketAddr,
    /// Seed peers' gossip addresses to bootstrap membership.
    pub seeds: Vec<String>,
    /// Replication-group id — peers must share it to form one cluster.
    pub cluster_id: String,
}

/// Live coordination state for this node within its replication group.
pub struct ClusterCoordinator {
    node_id: String,
    _handle: ChitchatHandle,
    /// This node's current leader epoch (stamped on records it ships). Bumped
    /// on promotion; read by the WAL sink at ship time.
    leader_epoch: Arc<AtomicU64>,
    /// Standby-side gate: highest epoch accepted from a leader.
    fencer: Arc<EpochFencer>,
    is_leader: Arc<AtomicBool>,
}

impl ClusterCoordinator {
    /// Join the group's gossip cluster and start the election loop.
    pub async fn start(cfg: ClusterConfig) -> Result<Arc<Self>> {
        // Generation = restart counter: a fresh boot looks like a new incarnation
        // to peers so stale state from a prior run is superseded.
        let generation = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let chitchat_id = ChitchatId::new(cfg.node_id.clone(), generation, cfg.advertise_addr);
        let config = ChitchatConfig {
            chitchat_id,
            cluster_id: cfg.cluster_id,
            gossip_interval: Duration::from_millis(1000),
            listen_addr: cfg.listen_addr,
            seed_nodes: cfg.seeds,
            failure_detector_config: FailureDetectorConfig::default(),
            marked_for_deletion_grace_period: Duration::from_secs(3600),
            catchup_callback: None,
            extra_liveness_predicate: None,
        };
        let handle = chitchat::spawn_chitchat(
            config,
            vec![
                (EPOCH_KEY.to_string(), "0".to_string()),
                (ROLE_KEY.to_string(), "standby".to_string()),
            ],
            &UdpTransport,
        )
        .await
        .context("starting chitchat gossip")?;

        let coord = Arc::new(Self {
            node_id: cfg.node_id,
            leader_epoch: Arc::new(AtomicU64::new(0)),
            fencer: Arc::new(EpochFencer::new()),
            is_leader: Arc::new(AtomicBool::new(false)),
            _handle: handle,
        });

        coord.spawn_election_loop();
        tracing::info!(node = %coord.node_id, "joined cluster (chitchat); election loop running");
        Ok(coord)
    }

    /// This node's id.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Whether this node is the current elected leader of its group.
    pub fn is_leader(&self) -> bool {
        self.is_leader.load(Ordering::Acquire)
    }

    /// This node's current leader epoch (0 until first promotion).
    pub fn current_epoch(&self) -> u64 {
        self.leader_epoch.load(Ordering::Acquire)
    }

    /// Handle to this node's leader epoch — the WAL sink stamps it on records.
    pub fn leader_epoch_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.leader_epoch)
    }

    /// The standby-side fencer the WAL replication endpoint checks records against.
    pub fn fencer(&self) -> Arc<EpochFencer> {
        Arc::clone(&self.fencer)
    }

    fn spawn_election_loop(&self) {
        let chitchat = self._handle.chitchat();
        let node_id = self.node_id.clone();
        let leader_epoch = Arc::clone(&self.leader_epoch);
        let is_leader = Arc::clone(&self.is_leader);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(ELECTION_TICK);
            loop {
                tick.tick().await;
                let mut cc = chitchat.lock().await;
                let live_ids: Vec<String> = cc.live_nodes().map(|id| id.node_id.clone()).collect();
                // The highest epoch any live member has advertised — a freshly
                // promoted leader must bump past all of them.
                let mut max_epoch = 0u64;
                for id in cc.live_nodes() {
                    if let Some(epoch) = cc
                        .node_state(id)
                        .and_then(|ns| ns.get(EPOCH_KEY))
                        .and_then(|v| v.parse::<u64>().ok())
                    {
                        max_epoch = max_epoch.max(epoch);
                    }
                }
                let am_leader = elect_leader(&live_ids)
                    .map(|l| *l == node_id)
                    .unwrap_or(false);
                let was_leader = is_leader.swap(am_leader, Ordering::AcqRel);
                if am_leader && !was_leader {
                    let new_epoch = max_epoch + 1;
                    leader_epoch.store(new_epoch, Ordering::Release);
                    cc.self_node_state().set(EPOCH_KEY, new_epoch.to_string());
                    cc.self_node_state().set(ROLE_KEY, "leader");
                    tracing::info!(node = %node_id, epoch = new_epoch, "promoted to group leader");
                } else if !am_leader && was_leader {
                    cc.self_node_state().set(ROLE_KEY, "standby");
                    tracing::warn!(node = %node_id, "stepped down as group leader");
                }
            }
        });
    }
}
