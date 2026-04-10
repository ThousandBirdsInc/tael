mod api;
mod config;
mod ingest;
mod storage;

use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tonic::transport::Server as TonicServer;
use tracing_subscriber::EnvFilter;

use config::ServerConfig;
use storage::DuckDbStore;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = ServerConfig::from_env();
    let store = Arc::new(DuckDbStore::new(&config.data_dir)?);

    tracing::info!(
        otlp_grpc = %config.otlp_grpc_addr,
        rest_api = %config.rest_api_addr,
        data_dir = %config.data_dir,
        "starting tael server"
    );

    let grpc_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let addr = config.otlp_grpc_addr.parse()?;
        async move {
            let trace_service = ingest::otlp::OtlpTraceService::new(store);
            TonicServer::builder()
                .add_service(
                    opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceServiceServer::new(trace_service),
                )
                .serve(addr)
                .await
                .expect("gRPC server failed");
        }
    });

    let rest_handle = tokio::spawn({
        let store = Arc::clone(&store);
        let addr = config.rest_api_addr.clone();
        async move {
            let app = api::rest::router(store);
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
