//! tael-server: OTLP ingest, tiered storage, and the REST/gRPC query API.
//!
//! Shipped as a library so the `tael` binary can embed it behind `tael serve`
//! (a single `cargo install`), while still being usable as a standalone crate.
//! [`run`] is the entrypoint; [`ServerConfig`] configures it.

mod api;
mod cluster;
mod config;
mod ingest;
mod log_bus;
mod promql;
mod span_bus;
mod storage;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

pub use config::{ServerConfig, StorageBackend};

use log_bus::LogBus;
use span_bus::SpanBus;
use storage::{
    BlobStore, DuckDbStore, FanoutStore, RemoteStore, RemoteWalSink, Store, TaelBackend, WalSink,
};

/// Periodically roll spans older than the hot-tier window into the cold tier.
/// Runs the (blocking) compaction off the async executor. The window
/// (`retention.traces.hot_tier`, default 24h) and interval are env-tunable
/// (`TAEL_HOT_TIER_HOURS`, `TAEL_COMPACT_INTERVAL_SECS`) until retention config
/// lands (Phase 7); a 0-hour window compacts everything (used in tests).
fn spawn_span_compactor(backend: Arc<TaelBackend>, blobs: Arc<BlobStore>) {
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
                // just removed by retention). Runs after partition drops.
                let live = backend.collect_live_blob_hashes()?;
                let blobs_gcd = blobs.gc(&live)?;
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

/// Start the server: OTLP gRPC + REST listeners over the configured storage
/// backend, plus the background maintenance task when running on tael-backend.
/// Runs until one of the listeners exits.
pub async fn run(config: ServerConfig) -> Result<()> {
    // Initialize tracing for the server process. `try_init` so embedding this in
    // a binary that already set a subscriber is a no-op rather than a panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    let blobs = Arc::new(BlobStore::new(&config.data_dir)?);

    // Cluster coordination (chitchat): automatic leader election + epoch fencing
    // of WAL replication (§5.1). On when TAEL_CLUSTER_LISTEN is set.
    let coordinator = match &config.cluster {
        Some(cs) => {
            let coord = cluster::ClusterCoordinator::start(cluster::ClusterConfig {
                node_id: cs.node_id.clone(),
                listen_addr: cs.listen_addr.parse().context("parsing TAEL_CLUSTER_LISTEN")?,
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
            StorageBackend::Duckdb => Arc::new(DuckDbStore::new(&config.data_dir)?),
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
                let backend = Arc::new(if sinks.is_empty() {
                    TaelBackend::new(&config.data_dir)?
                } else {
                    tracing::info!(
                        standbys = sinks.len(),
                        required_acks = ?config.wal_required_acks,
                        "WAL replication enabled: shipping to standbys (leader)"
                    );
                    TaelBackend::with_wal_key_and_sinks(
                        &config.data_dir,
                        "tael-backend",
                        sinks,
                        config.wal_required_acks,
                    )?
                });
                search = Some(backend.search_index());
                spawn_span_compactor(Arc::clone(&backend), Arc::clone(&blobs));
                backend as Arc<dyn Store>
            }
        }
    };
    let bus = Arc::new(SpanBus::new()?);
    let log_bus = Arc::new(LogBus::new()?);

    tracing::info!(
        otlp_grpc = %config.otlp_grpc_addr,
        rest_api = %config.rest_api_addr,
        data_dir = %config.data_dir,
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
            let metrics_service =
                ingest::otlp_metrics::OtlpMetricsService::new(store);
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
        async move {
            let app = api::rest::router(store, blobs, bus, log_bus, cluster);
            let listener = TcpListener::bind(&addr).await.expect("failed to bind REST addr");
            tracing::info!(%addr, "REST API listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .expect("REST server failed");
        }
    });

    // Both listeners drain on SIGTERM/Ctrl-C; await both so in-flight requests
    // finish before we flush and exit (`docs/tael-server-scaling-ha.md` §5.4).
    let (grpc_res, rest_res) = tokio::join!(grpc_handle, rest_handle);
    grpc_res?;
    rest_res?;

    // Best-effort flush so a restart/standby replays less WAL. Durability is
    // already guaranteed by the per-write WAL fsync.
    if let Err(e) = store.flush() {
        tracing::warn!(error = %e, "flush on shutdown failed");
    }
    tracing::info!("tael server stopped");

    Ok(())
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
