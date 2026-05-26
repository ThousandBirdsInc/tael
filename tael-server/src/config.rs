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
    pub storage: StorageBackend,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        let mut config = Self {
            otlp_grpc_addr: std::env::var("TAEL_OTLP_GRPC_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:4317".into()),
            rest_api_addr: std::env::var("TAEL_REST_API_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:7701".into()),
            data_dir: std::env::var("TAEL_DATA_DIR").unwrap_or_else(|_| "./data".into()),
            // Default to the tael-backend engine; `TAEL_STORAGE` can override.
            storage: std::env::var("TAEL_STORAGE")
                .map(|s| StorageBackend::parse(&s))
                .unwrap_or(StorageBackend::TaelBackend),
        };
        // A `--storage <duckdb|tael-backend>` flag (or `--storage=…`) takes
        // precedence over the env var.
        if let Some(s) = storage_flag() {
            config.storage = s;
        }
        config
    }
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
