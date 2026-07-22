use std::path::PathBuf;

use crate::storage::StoreLocation;

/// Selected storage backend. `TaelBackend` (the purpose-built tiered engine)
/// is the default; pass `--storage duckdb` or set `TAEL_STORAGE=duckdb` to use
/// the legacy embedded-DuckDB backend instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    Duckdb,
    TaelBackend,
}

impl StorageBackend {
    /// Parse a backend name (from the `--storage` flag or `TAEL_STORAGE`).
    /// Anything that isn't explicitly `duckdb` selects the default tael-backend.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "duckdb" | "duck" => StorageBackend::Duckdb,
            _ => StorageBackend::TaelBackend,
        }
    }
}

pub struct ServerConfig {
    pub otlp_grpc_addr: String,
    pub rest_api_addr: String,
    pub rest_api_socket: Option<String>,
    /// Dedicated Datadog trace-agent listener address, so dd-trace clients
    /// work zero-config on the agent's default port (8126). `None` disables
    /// the extra listener; the same endpoints stay mounted on the REST
    /// listener either way. Set via `TAEL_DD_AGENT_ADDR` (`off` to disable).
    pub dd_agent_addr: Option<String>,
    pub data_dir: String,
    pub wal_dir: String,
    pub storage: StorageBackend,
    /// When non-empty, this process runs as a stateless **query tier**: reads
    /// are served by a `FanoutStore` that scatter-gathers across these shard
    /// base URLs (`http://shard-0:7701,...`) instead of a local engine. Set via
    /// `TAEL_QUERY_SHARDS`. See `docs/tael-server-scaling-ha.md` §3.
    pub query_shards: Vec<String>,
    /// Standby base URLs this node ships its WAL to as a **leader**
    /// (`http://standby-1:7701,...`). Set via `TAEL_WAL_STANDBYS`. Only honored
    /// by the tael-backend engine. See §5.1.
    pub wal_standbys: Vec<String>,
    /// How many standbys must ack a write before it returns. `None` = all
    /// (fully synchronous); `Some(0)` = async best-effort. Set via
    /// `TAEL_WAL_REQUIRED_ACKS`.
    pub wal_required_acks: Option<usize>,
    /// Cluster coordination (chitchat) for automatic leader election + epoch
    /// fencing of WAL replication. `Some` when `TAEL_CLUSTER_LISTEN` is set.
    /// See `docs/tael-server-scaling-ha.md` §5.1.
    pub cluster: Option<ClusterSettings>,
    /// Where the cold tier and blob store keep their objects. Defaults to local
    /// filesystem (unchanged single-binary behavior); GCS is opt-in and needs
    /// the `cloud` build feature.
    pub object_store: ObjectStoreConfig,
    /// Where trace comments are stored. Defaults to the local JSONL file;
    /// Postgres (Cloud SQL) is opt-in and needs the `cloud` build feature.
    pub comments: CommentsConfig,
}

/// Object-storage selection for the cold (Parquet) tier and the blob store.
/// All fields default to the local-filesystem behavior, so a bare run is
/// unchanged.
#[derive(Debug, Clone)]
pub struct ObjectStoreConfig {
    /// Cold-tier backend (`TAEL_COLD_STORE`, `fs` | `gcs`).
    pub cold: StoreLocation,
    /// Blob-store backend (`TAEL_BLOB_STORE`, `fs` | `gcs`).
    pub blobs: StoreLocation,
    /// Cold bucket URL when `cold == Gcs` (`TAEL_COLD_BUCKET`, `gs://b/prefix`).
    pub cold_bucket: Option<String>,
    /// Blob bucket URL when `blobs == Gcs` (`TAEL_BLOB_BUCKET`, `gs://b/prefix`).
    pub blob_bucket: Option<String>,
    /// Whether this node owns blob GC over a shared object store
    /// (`TAEL_BLOB_GC_ROLE=coordinator`). When the blob store is shared (GCS),
    /// per-node mark-and-sweep would delete blobs other shards reference, so it
    /// is disabled unless this node is the coordinator. Ignored for local FS
    /// (each node owns its own blobs).
    pub blob_gc_coordinator: bool,
}

impl ObjectStoreConfig {
    fn from_env() -> Self {
        Self {
            cold: std::env::var("TAEL_COLD_STORE")
                .map(|s| StoreLocation::parse(&s))
                .unwrap_or_default(),
            blobs: std::env::var("TAEL_BLOB_STORE")
                .map(|s| StoreLocation::parse(&s))
                .unwrap_or_default(),
            cold_bucket: non_empty_env("TAEL_COLD_BUCKET"),
            blob_bucket: non_empty_env("TAEL_BLOB_BUCKET"),
            blob_gc_coordinator: std::env::var("TAEL_BLOB_GC_ROLE")
                .map(|s| s.trim().eq_ignore_ascii_case("coordinator"))
                .unwrap_or(false),
        }
    }

    /// Whether the blob store is shared across nodes (object storage rather
    /// than a node-local directory). Drives the blob-GC single-owner guard.
    pub fn blobs_shared(&self) -> bool {
        self.blobs == StoreLocation::Gcs
    }
}

/// Where trace comments live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentsBackend {
    /// Local append-only JSONL file (`<data_dir>/trace_comments.jsonl`).
    #[default]
    Jsonl,
    /// Postgres (e.g. Cloud SQL) — requires the `cloud` feature.
    Postgres,
}

impl CommentsBackend {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => CommentsBackend::Postgres,
            _ => CommentsBackend::Jsonl,
        }
    }
}

/// Comments-store selection.
#[derive(Debug, Clone)]
pub struct CommentsConfig {
    pub backend: CommentsBackend,
    /// Postgres connection URL when `backend == Postgres`
    /// (`TAEL_COMMENTS_DATABASE_URL`).
    pub database_url: Option<String>,
}

impl CommentsConfig {
    fn from_env() -> Self {
        Self {
            backend: std::env::var("TAEL_COMMENTS_STORE")
                .map(|s| CommentsBackend::parse(&s))
                .unwrap_or_default(),
            database_url: non_empty_env("TAEL_COMMENTS_DATABASE_URL"),
        }
    }
}

/// Gossip-cluster settings for HA leader election (chitchat).
#[derive(Debug, Clone)]
pub struct ClusterSettings {
    /// Stable unique node id within the replication group (election orders on it).
    pub node_id: String,
    /// UDP gossip listen address (`TAEL_CLUSTER_LISTEN`).
    pub listen_addr: String,
    /// Address peers reach this node on (`TAEL_CLUSTER_ADVERTISE`; default = listen).
    pub advertise_addr: String,
    /// Seed peers' gossip addresses (`TAEL_CLUSTER_SEEDS`).
    pub seeds: Vec<String>,
    /// Replication-group id peers must share (`TAEL_CLUSTER_ID`, default `tael`).
    pub cluster_id: String,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let mut config = Self {
            otlp_grpc_addr: std::env::var("TAEL_OTLP_GRPC_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4317".into()),
            rest_api_addr: std::env::var("TAEL_REST_API_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:7701".into()),
            rest_api_socket: std::env::var("TAEL_REST_API_SOCKET")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            dd_agent_addr: parse_dd_agent_addr(std::env::var("TAEL_DD_AGENT_ADDR").ok()),
            data_dir: std::env::var("TAEL_DATA_DIR").unwrap_or_else(|_| default_data_dir()),
            wal_dir: std::env::var("TAEL_WAL_DIR")
                .or_else(|_| std::env::var("WALRUS_DATA_DIR"))
                .unwrap_or_else(|_| default_wal_dir()),
            // Default to the tael-backend engine; `TAEL_STORAGE` can override.
            storage: std::env::var("TAEL_STORAGE")
                .map(|s| StorageBackend::parse(&s))
                .unwrap_or(StorageBackend::TaelBackend),
            query_shards: parse_csv_env("TAEL_QUERY_SHARDS"),
            wal_standbys: parse_csv_env("TAEL_WAL_STANDBYS"),
            wal_required_acks: std::env::var("TAEL_WAL_REQUIRED_ACKS")
                .ok()
                .and_then(|s| s.trim().parse().ok()),
            cluster: cluster_from_env(),
            object_store: ObjectStoreConfig::from_env(),
            comments: CommentsConfig::from_env(),
        };
        // A `--storage <duckdb|tael-backend>` flag (or `--storage=…`) takes
        // precedence over the env var.
        if let Some(s) = storage_flag() {
            config.storage = s;
        }
        config
    }
}

fn default_data_dir() -> String {
    default_tael_home().join("data").display().to_string()
}

fn default_wal_dir() -> String {
    default_tael_home().join("wal_files").display().to_string()
}

fn default_tael_home() -> PathBuf {
    home_dir()
        .map(|home| home.join(".tael"))
        .unwrap_or_else(|| PathBuf::from(".tael"))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(
            || match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
                (Some(drive), Some(path)) => {
                    let mut home = PathBuf::from(drive);
                    home.push(path);
                    Some(home)
                }
                _ => None,
            },
        )
}

/// Build cluster settings from `TAEL_CLUSTER_*`. Returns `None` (coordination
/// off) unless `TAEL_CLUSTER_LISTEN` is set.
fn cluster_from_env() -> Option<ClusterSettings> {
    let listen_addr = std::env::var("TAEL_CLUSTER_LISTEN").ok()?;
    let advertise_addr =
        std::env::var("TAEL_CLUSTER_ADVERTISE").unwrap_or_else(|_| listen_addr.clone());
    // Default node id to the advertise address — unique per node, stable.
    let node_id = std::env::var("TAEL_NODE_ID").unwrap_or_else(|_| advertise_addr.clone());
    Some(ClusterSettings {
        node_id,
        listen_addr,
        advertise_addr,
        seeds: parse_csv_env("TAEL_CLUSTER_SEEDS"),
        cluster_id: std::env::var("TAEL_CLUSTER_ID").unwrap_or_else(|_| "tael".to_string()),
    })
}

/// Read an env var, returning `None` when unset or blank.
/// The Datadog trace-agent's default listen address; dd-trace clients send
/// here when `DD_TRACE_AGENT_URL`/`DD_AGENT_HOST` are unset.
pub const DEFAULT_DD_AGENT_ADDR: &str = "127.0.0.1:8126";

/// Resolve `TAEL_DD_AGENT_ADDR` (or the `--dd-agent-addr` flag): unset defaults
/// to the agent's standard port so dd-trace clients work zero-config;
/// `off`/`none`/`disabled`/`false`/`0` (or empty) disables the listener.
pub fn parse_dd_agent_addr(value: Option<String>) -> Option<String> {
    match value {
        None => Some(DEFAULT_DD_AGENT_ADDR.to_string()),
        Some(v) => {
            let v = v.trim().to_string();
            match v.to_lowercase().as_str() {
                "" | "off" | "none" | "disabled" | "false" | "0" => None,
                _ => Some(v),
            }
        }
    }
}

fn non_empty_env(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.trim().is_empty())
}

/// Parse a comma-separated env var into a trimmed, non-empty list.
fn parse_csv_env(var: &str) -> Vec<String> {
    std::env::var(var)
        .ok()
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Scan the process args for `--storage <value>` / `--storage=<value>`.
fn storage_flag() -> Option<StorageBackend> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--storage" {
            return args.next().map(|v| StorageBackend::parse(&v));
        }
        if let Some(v) = arg.strip_prefix("--storage=") {
            return Some(StorageBackend::parse(v));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dd_agent_addr_defaults_on_and_honors_off_values() {
        assert_eq!(
            parse_dd_agent_addr(None).as_deref(),
            Some(DEFAULT_DD_AGENT_ADDR)
        );
        assert_eq!(
            parse_dd_agent_addr(Some("0.0.0.0:8126".into())).as_deref(),
            Some("0.0.0.0:8126")
        );
        for off in ["off", "OFF", "none", "disabled", "false", "0", "  "] {
            assert_eq!(parse_dd_agent_addr(Some(off.into())), None, "{off}");
        }
    }
}
