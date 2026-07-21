//! tael-server: OTLP ingest, tiered storage, and the REST/gRPC query API.
//!
//! Shipped as a library so the `tael` binary can embed it behind `tael serve`
//! (a single `cargo install`), while still being usable as a standalone crate.
//! [`run`] is the default CLI-style entrypoint; [`run_embedded`] starts the same
//! server in quiet mode for in-process integrations. [`ServerConfig`] configures
//! the listeners and storage.

mod api;
mod cluster;
mod config;
mod ingest;
mod log_bus;
mod promql;
mod span_bus;
mod storage;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

pub use config::{DEFAULT_DD_AGENT_ADDR, ServerConfig, StorageBackend, parse_dd_agent_addr};
#[cfg(feature = "duckdb")]
pub use storage::DuckDbStore;
pub use storage::models::{
    LogRecord, LogSeverity, MetricPoint, MetricType, Span, SpanEvent, SpanKind, SpanStatus,
    TraceQuery,
};
pub use storage::{
    BlobStore, FanoutStore, RemoteStore, RemoteWalSink, Store, TaelBackend, WalSink,
};
use storage::{StoreLocation, open_comments, open_object_backend};

use log_bus::LogBus;
use span_bus::SpanBus;

/// Controls output that the tael-server library owns directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutputMode {
    /// Install the default tracing subscriber when possible and print the
    /// startup banner to stdout. This is the right mode for `tael serve`.
    Default,
    /// Do not install a tracing subscriber and do not print the startup banner.
    /// Existing application-level tracing subscribers may still receive Tael
    /// events; this only prevents the library from claiming stdout/stderr on
    /// its own.
    Quiet,
}

/// Options for running the server process beyond listener/storage config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerRunOptions {
    pub output: ServerOutputMode,
}

impl Default for ServerRunOptions {
    fn default() -> Self {
        Self {
            output: ServerOutputMode::Default,
        }
    }
}

impl ServerRunOptions {
    /// Quiet options for embedding Tael inside another CLI/TUI process.
    pub fn quiet() -> Self {
        Self {
            output: ServerOutputMode::Quiet,
        }
    }

    fn is_quiet(self) -> bool {
        matches!(self.output, ServerOutputMode::Quiet)
    }
}

/// Periodically roll spans older than the hot-tier window into the cold tier.
/// Runs the (blocking) compaction off the async executor. The window
/// (`retention.traces.hot_tier`, default 24h) and interval are env-tunable
/// (`TAEL_HOT_TIER_HOURS`, `TAEL_COMPACT_INTERVAL_SECS`) until retention config
/// lands (Phase 7); a 0-hour window compacts everything (used in tests).
fn spawn_span_compactor(backend: Arc<TaelBackend>, blobs: Arc<BlobStore>, blob_gc_enabled: bool) {
    let window_hours: i64 = std::env::var("TAEL_HOT_TIER_HOURS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let interval_secs: u64 = std::env::var("TAEL_COMPACT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    // Span metadata retention (`retention.traces.metadata`, default 365d).
    let retention_days: i64 = std::env::var("TAEL_TRACE_RETENTION_DAYS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(365);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            tick.tick().await;
            let backend = Arc::clone(&backend);
            let blobs = Arc::clone(&blobs);
            let result = tokio::task::spawn_blocking(move || {
                let now = chrono::Utc::now();
                let hot_cutoff = now - chrono::Duration::hours(window_hours);
                let mut compacted = backend.compact_spans(hot_cutoff)?;
                compacted += backend.compact_logs_metrics(hot_cutoff)?;
                let dropped =
                    backend.enforce_span_retention(now - chrono::Duration::days(retention_days))?;
                // Payload blob GC: drop blobs no live row references (e.g. rows
                // just removed by retention). Runs after partition drops. Skipped
                // when this node doesn't own GC over a shared blob store (the
                // single-owner guard), to avoid deleting blobs other shards
                // reference.
                let blobs_gcd = if blob_gc_enabled {
                    let live = backend.collect_live_blob_hashes()?;
                    blobs.gc(&live)?
                } else {
                    0
                };
                anyhow::Ok((compacted, dropped, blobs_gcd))
            })
            .await;
            match result {
                Ok(Ok((c, d, g))) if c > 0 || d > 0 || g > 0 => tracing::info!(
                    compacted = c,
                    partitions_dropped = d,
                    blobs_gcd = g,
                    "tael-backend maintenance"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "maintenance failed"),
                Err(e) => tracing::warn!(error = %e, "maintenance task panicked"),
            }
        }
    });
}

/// Start the server with the default user-facing output behavior.
///
/// This is the right entrypoint for binaries such as `tael serve`: it installs a
/// default tracing subscriber if the process has not already done so and prints
/// a startup banner to stdout.
pub async fn run(config: ServerConfig) -> Result<()> {
    run_with_options(config, ServerRunOptions::default()).await
}

/// Start the server in quiet mode for in-process integrations.
///
/// Quiet mode avoids Tael-owned stdout/stderr setup so one-shot commands and
/// TUIs embedding the server can preserve their own output contract.
pub async fn run_embedded(config: ServerConfig) -> Result<()> {
    run_with_options(config, ServerRunOptions::quiet()).await
}

/// Start the server with explicit run options.
///
/// Runs until both listeners receive shutdown. The configured storage backend is
/// shared by OTLP ingest and REST query APIs, with the background maintenance
/// task enabled when running on tael-backend.
pub async fn run_with_options(mut config: ServerConfig, options: ServerRunOptions) -> Result<()> {
    // Initialize tracing for the server process in the default CLI mode.
    // `try_init` keeps embedding in a binary that already set a subscriber from
    // panicking. Quiet mode leaves all tracing ownership to the host process.
    if !options.is_quiet() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .try_init();
    }

    configure_walrus_data_dir(&config.wal_dir);

    // Blob store: local filesystem by default; GCS when configured (opt-in,
    // requires the `cloud` feature — otherwise this fails loudly).
    let blobs = Arc::new(match config.object_store.blobs {
        StoreLocation::Fs => BlobStore::new(&config.data_dir)?,
        StoreLocation::Gcs => {
            let backend = open_object_backend(
                StoreLocation::Gcs,
                Path::new(&config.data_dir).join("blobs").as_path(),
                config.object_store.blob_bucket.as_deref(),
            )?;
            BlobStore::with_backend(backend)?
        }
    });

    // Cluster coordination (chitchat): automatic leader election + epoch fencing
    // of WAL replication (§5.1). On when TAEL_CLUSTER_LISTEN is set.
    let coordinator = match &config.cluster {
        Some(cs) => {
            let coord = cluster::ClusterCoordinator::start(cluster::ClusterConfig {
                node_id: cs.node_id.clone(),
                listen_addr: cs
                    .listen_addr
                    .parse()
                    .context("parsing TAEL_CLUSTER_LISTEN")?,
                advertise_addr: cs
                    .advertise_addr
                    .parse()
                    .context("parsing TAEL_CLUSTER_ADVERTISE")?,
                seeds: cs.seeds.clone(),
                cluster_id: cs.cluster_id.clone(),
            })
            .await?;
            Some(coord)
        }
        None => None,
    };

    // The payload search index is shared between the ingest path (writes) and
    // the tael-backend query path (reads); present only when that engine runs.
    let mut search: Option<Arc<storage::SearchIndex>> = None;
    let store: Arc<dyn Store> = if !config.query_shards.is_empty() {
        // Stateless query-tier mode: serve reads by scatter-gather over remote
        // shards, no local engine (`docs/tael-server-scaling-ha.md` §3, Phase 2).
        let shards = config
            .query_shards
            .iter()
            .map(|url| RemoteStore::new(url).map(|s| Arc::new(s) as Arc<dyn Store>))
            .collect::<Result<Vec<_>>>()?;
        tracing::info!(
            shards = shards.len(),
            "query fan-out mode: reads scatter-gather across remote shards (no local engine)"
        );
        Arc::new(FanoutStore::new(shards)?)
    } else {
        match config.storage {
            #[cfg(feature = "duckdb")]
            StorageBackend::Duckdb => Arc::new(DuckDbStore::new(&config.data_dir)?),
            #[cfg(not(feature = "duckdb"))]
            StorageBackend::Duckdb => {
                bail!(
                    "DuckDB storage is not included in this build; reinstall with `--features duckdb` to use --storage duckdb"
                )
            }
            StorageBackend::TaelBackend => {
                // WAL replication: when standbys are configured, this node is a
                // leader that ships every appended record to them before acking
                // (§5.1). With no standbys the write path is unchanged.
                let sinks: Vec<Arc<dyn WalSink>> = config
                    .wal_standbys
                    .iter()
                    .map(|url| {
                        // Stamp the leader epoch (for standby fencing) when a
                        // coordinator is running; otherwise ship unfenced.
                        let sink = match &coordinator {
                            Some(c) => RemoteWalSink::with_epoch(url, c.leader_epoch_handle()),
                            None => RemoteWalSink::new(url),
                        };
                        sink.map(|s| Arc::new(s) as Arc<dyn WalSink>)
                    })
                    .collect::<Result<Vec<_>>>()?;
                if !sinks.is_empty() {
                    tracing::info!(
                        standbys = sinks.len(),
                        required_acks = ?config.wal_required_acks,
                        "WAL replication enabled: shipping to standbys (leader)"
                    );
                }
                // Cold tier: local filesystem by default; GCS when configured
                // (opt-in, requires the `cloud` feature).
                let cold_backend = match config.object_store.cold {
                    StoreLocation::Fs => None,
                    StoreLocation::Gcs => Some(open_object_backend(
                        StoreLocation::Gcs,
                        Path::new(&config.data_dir).join("cold").as_path(),
                        config.object_store.cold_bucket.as_deref(),
                    )?),
                };
                // Comments: local JSONL by default; Postgres when configured.
                let comments = open_comments(&config.comments, &config.data_dir)?;
                let backend = Arc::new(TaelBackend::with_components(
                    &config.data_dir,
                    "tael-backend",
                    sinks,
                    config.wal_required_acks,
                    cold_backend,
                    comments,
                )?);
                search = Some(backend.search_index());
                // Blob GC single-owner guard: on a shared (GCS) blob store,
                // per-node mark-and-sweep would delete blobs other shards still
                // reference, so it only runs on the designated coordinator. On a
                // node-local store every node owns its own blobs and GCs freely.
                let blob_gc_enabled =
                    !config.object_store.blobs_shared() || config.object_store.blob_gc_coordinator;
                if !blob_gc_enabled {
                    tracing::info!(
                        "blob GC disabled on this node: shared blob store, not the GC coordinator \
                         (set TAEL_BLOB_GC_ROLE=coordinator on exactly one node)"
                    );
                }
                spawn_span_compactor(Arc::clone(&backend), Arc::clone(&blobs), blob_gc_enabled);
                backend as Arc<dyn Store>
            }
        }
    };
    let bus = Arc::new(SpanBus::new()?);
    let log_bus = Arc::new(LogBus::new()?);

    tracing::info!(
        otlp_grpc = %config.otlp_grpc_addr,
        rest_api = %config.rest_api_addr,
        rest_api_socket = ?config.rest_api_socket,
        dd_agent = ?config.dd_agent_addr,
        data_dir = %config.data_dir,
        wal_dir = %config.wal_dir,
        storage = ?config.storage,
        "starting tael server"
    );

    let grpc_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let blobs = Arc::clone(&blobs);
        let bus = Arc::clone(&bus);
        let log_bus = Arc::clone(&log_bus);
        let addr = config.otlp_grpc_addr.parse()?;
        async move {
            let trace_service = ingest::otlp::OtlpTraceService::new(
                Arc::clone(&store),
                Arc::clone(&blobs),
                search.clone(),
                bus,
            );
            let logs_service = ingest::otlp_logs::OtlpLogsService::new(
                Arc::clone(&store),
                Arc::clone(&blobs),
                log_bus,
            );
            let metrics_service = ingest::otlp_metrics::OtlpMetricsService::new(store);
            TonicServer::builder()
                .add_service(
                    opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer::new(trace_service),
                )
                .add_service(
                    opentelemetry_proto::tonic::collector::logs::v1::logs_service_server::LogsServiceServer::new(logs_service),
                )
                .add_service(
                    opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::MetricsServiceServer::new(metrics_service),
                )
                .serve_with_shutdown(addr, shutdown_signal())
                .await
                .expect("gRPC server failed");
        }
    });

    let rest_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let blobs = Arc::clone(&blobs);
        let bus = Arc::clone(&bus);
        let log_bus = Arc::clone(&log_bus);
        let cluster = coordinator.clone();
        let addr = config.rest_api_addr.clone();
        let socket = config.rest_api_socket.clone();
        async move {
            let app = api::rest::router(store, blobs, bus, log_bus, cluster);
            if let Some(socket) = socket {
                #[cfg(unix)]
                {
                    prepare_unix_socket_path(&socket)?;
                    let listener = tokio::net::UnixListener::bind(&socket)
                        .with_context(|| format!("binding REST Unix socket {socket}"))?;
                    tracing::info!(%socket, "REST API listening on Unix socket");
                    let result = axum::serve(listener, app)
                        .with_graceful_shutdown(shutdown_signal())
                        .await
                        .context("REST server failed");
                    cleanup_unix_socket_path(&socket);
                    result?;
                }
                #[cfg(not(unix))]
                {
                    bail!("REST Unix sockets are only supported on Unix platforms");
                }
            } else {
                let listener = TcpListener::bind(&addr)
                    .await
                    .with_context(|| format!("binding REST addr {addr}"))?;
                tracing::info!(%addr, "REST API listening");
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .context("REST server failed")?;
            }
            Ok::<(), anyhow::Error>(())
        }
    });

    // Dedicated Datadog trace-agent listener on the agent's default port, so
    // dd-trace clients work with no configuration at all. Bound here (not in
    // the task) so the startup banner reflects reality: a bind failure (most
    // likely a real Datadog agent already on 8126) is a warning, not fatal —
    // the same endpoints stay available on the REST listener via
    // DD_TRACE_AGENT_URL, and the banner falls back to that advice.
    let dd_listener = match &config.dd_agent_addr {
        Some(addr) => match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(%addr, "Datadog trace-agent intake listening");
                Some(listener)
            }
            Err(e) => {
                tracing::warn!(
                    %addr, error = %e,
                    "Datadog agent port unavailable (another agent running?); \
                     dd-trace intake stays available on the REST listener"
                );
                config.dd_agent_addr = None;
                None
            }
        },
        None => None,
    };
    let dd_handle = dd_listener.map(|listener| {
        tokio::spawn({
            let store = Arc::clone(&store);
            let blobs = Arc::clone(&blobs);
            let bus = Arc::clone(&bus);
            let log_bus = Arc::clone(&log_bus);
            async move {
                let app = api::rest::dd_router(store, blobs, bus, log_bus);
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .context("Datadog trace-agent server failed")
            }
        })
    });

    if !options.is_quiet() {
        print_startup_banner(&config);
    }

    // All listeners drain on SIGTERM/Ctrl-C; await them so in-flight requests
    // finish before we flush and exit (`docs/tael-server-scaling-ha.md` §5.4).
    let (grpc_res, rest_res) = tokio::join!(grpc_handle, rest_handle);
    grpc_res?;
    rest_res??;
    if let Some(dd_handle) = dd_handle {
        dd_handle.await??;
    }

    // Best-effort flush so a restart/standby replays less WAL. Durability is
    // already guaranteed by the per-write WAL fsync.
    if let Err(e) = store.flush() {
        tracing::warn!(error = %e, "flush on shutdown failed");
    }
    tracing::info!("tael server stopped");

    Ok(())
}

fn configure_walrus_data_dir(wal_dir: &str) {
    // walrus-rust currently exposes its storage root through process env only.
    // Tael owns the server process and sets this once before opening the WAL.
    unsafe {
        std::env::set_var("WALRUS_DATA_DIR", wal_dir);
    }
}

#[cfg(unix)]
fn prepare_unix_socket_path(socket: &str) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let path = std::path::Path::new(socket);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating REST socket directory {}", parent.display()))?;
    }

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            bail!(
                "REST Unix socket path already exists: {}. Remove it if no server is running.",
                path.display()
            );
        }
        Ok(_) => {
            bail!(
                "REST Unix socket path exists and is not a socket: {}",
                path.display()
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("checking REST socket path {}", path.display())),
    }
}

#[cfg(unix)]
fn cleanup_unix_socket_path(socket: &str) {
    use std::os::unix::fs::FileTypeExt;

    let path = std::path::Path::new(socket);
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::warn!(socket = %path.display(), error = %e, "failed to remove REST Unix socket");
            }
        }
        Ok(_) | Err(_) => {}
    }
}

/// Friendly stdout banner shown on startup so a user running `tael serve`
/// (with or without `--port`) immediately sees where to connect a CLI and
/// where to point an OTLP exporter. Goes through `println!` so it's visible
/// regardless of `RUST_LOG`.
fn print_startup_banner(config: &ServerConfig) {
    let rest = rest_endpoint_label(config);
    let otlp = &config.otlp_grpc_addr;
    let connect_flag = cli_connect_flag(config);

    println!("tael server starting");
    println!("  REST API     {rest}");
    println!("  OTLP gRPC    {otlp}");
    if let Some(addr) = &config.dd_agent_addr {
        println!("  dd-trace     {addr}");
    }
    println!("  data dir     {}", config.data_dir);
    println!("  WAL dir      {}", config.wal_dir);
    println!("  storage      {:?}", config.storage);
    println!();
    println!("Connect a CLI from this machine:");
    println!("  tael{connect_flag} services");
    println!("  tael{connect_flag} live");
    println!();
    println!("Point a service at this server (OTLP):");
    println!("  export OTEL_EXPORTER_OTLP_ENDPOINT=http://{otlp}");
    println!("  export OTEL_EXPORTER_OTLP_PROTOCOL=grpc");
    println!("  export OTEL_SERVICE_NAME=<your-service>");
    println!();
    println!("Or a Datadog-instrumented service (dd-trace):");
    match &config.dd_agent_addr {
        // Listening on the agent's default port: dd-trace clients find it
        // with no configuration at all.
        Some(addr) if addr == config::DEFAULT_DD_AGENT_ADDR => {
            println!("  no exports needed — listening on the default agent port ({addr})");
        }
        Some(addr) => {
            println!("  export DD_TRACE_AGENT_URL=http://{addr}");
        }
        // Dedicated listener disabled: the REST listener still serves the
        // trace-agent endpoints.
        None => {
            println!("  export DD_TRACE_AGENT_URL={}", dd_agent_url(config));
        }
    }
    println!();
}

/// The `DD_TRACE_AGENT_URL` value that reaches this server's REST listener,
/// where the Datadog trace-agent endpoints are also mounted (used when the
/// dedicated agent-port listener is disabled). dd-trace clients accept both
/// `http://` and `unix://` agent URLs.
fn dd_agent_url(config: &ServerConfig) -> String {
    match &config.rest_api_socket {
        Some(socket) => format!("unix://{socket}"),
        None => format!("http://{}", config.rest_api_addr),
    }
}

/// Pick the CLI flag (if any) needed to reach this REST listener. Empty when
/// REST is on the CLI default `127.0.0.1:7701`; `--port-rest N` when only the
/// port differs; full `--server …` otherwise.
fn cli_connect_flag(config: &ServerConfig) -> String {
    if let Some(socket) = &config.rest_api_socket {
        return format!(" --unix-socket {socket}");
    }

    let rest_addr = &config.rest_api_addr;
    let (host, port) = match rest_addr.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => return String::new(),
    };
    let local = matches!(
        host,
        "127.0.0.1" | "localhost" | "0.0.0.0" | "::1" | "[::1]"
    );
    match (local, port) {
        (true, "7701") => String::new(),
        (true, p) => format!(" --port-rest {p}"),
        (false, _) => format!(" --server http://{rest_addr}"),
    }
}

fn rest_endpoint_label(config: &ServerConfig) -> String {
    match &config.rest_api_socket {
        Some(socket) => format!("unix://{socket}"),
        None => format!("http://{}", config.rest_api_addr),
    }
}

/// Resolve when the process is asked to stop: Ctrl-C, or SIGTERM on Unix
/// (the orchestrator's graceful-stop signal). Both listeners await their own
/// copy; the OS delivers the signal to every registered handler.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining listeners");
}
