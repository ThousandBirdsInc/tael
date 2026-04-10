mod scenarios;
mod span_builder;

use anyhow::Result;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;

#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::var("TAEL_OTLP_GRPC_ADDR")
        .unwrap_or_else(|_| "http://127.0.0.1:4317".into());

    println!("connecting to tael server at {addr}");
    let mut client = TraceServiceClient::connect(addr).await?;

    let batches: Vec<(&str, ExportTraceServiceRequest)> = vec![
        ("healthy API request", scenarios::healthy_api_request()),
        ("slow database query", scenarios::slow_db_query()),
        ("error in payment service", scenarios::payment_error()),
        ("fan-out to downstream services", scenarios::fanout_request()),
        ("burst of fast requests", scenarios::fast_burst()),
    ];

    for (name, request) in batches {
        let span_count: usize = request
            .resource_spans
            .iter()
            .map(|rs| rs.scope_spans.iter().map(|ss| ss.spans.len()).sum::<usize>())
            .sum();

        client.export(request).await?;
        println!("  sent: {name} ({span_count} spans)");
    }

    println!("\ndone — use `tael query traces` or `tael services` to inspect");
    Ok(())
}
