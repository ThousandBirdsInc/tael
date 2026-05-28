use std::path::PathBuf;

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
