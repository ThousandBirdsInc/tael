mod api;
mod config;
mod ingest;
mod log_bus;
mod promql;
mod span_bus;
mod storage;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

use config::{ServerConfig, StorageBackend};
use log_bus::LogBus;
use span_bus::SpanBus;
use storage::{BlobStore, DuckDbStore, Store, TaelBackend};

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = ServerConfig::from_env();
    let blobs = Arc::new(BlobStore::new(&config.data_dir)?);
    // The payload search index is shared between the ingest path (writes) and
    // the tael-backend query path (reads); present only when that engine runs.
    let mut search: Option<Arc<storage::SearchIndex>> = None;
    let store: Arc<dyn Store> = match config.storage {
        StorageBackend::Duckdb => Arc::new(DuckDbStore::new(&config.data_dir)?),
        StorageBackend::TaelBackend => {
            let backend = Arc::new(TaelBackend::new(&config.data_dir)?);
            search = Some(backend.search_index());
            spawn_span_compactor(Arc::clone(&backend), Arc::clone(&blobs));
            backend as Arc<dyn Store>
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
                .serve(addr)
                .await
                .expect("gRPC server failed");
        }
    });

    let rest_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let blobs = Arc::clone(&blobs);
        let bus = Arc::clone(&bus);
        let log_bus = Arc::clone(&log_bus);
        let addr = config.rest_api_addr.clone();
        async move {
            let app = api::rest::router(store, blobs, bus, log_bus);
            let listener = TcpListener::bind(&addr).await.expect("failed to bind REST addr");
            tracing::info!(%addr, "REST API listening");
            axum::serve(listener, app).await.expect("REST server failed");
        }
    });

    tokio::select! {
        r = grpc_handle => r?,
        r = rest_handle => r?,
    }

    Ok(())
}
