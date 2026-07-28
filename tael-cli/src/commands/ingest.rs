//! `tael ingest status` — health of the server's ingestion pipelines.
//!
//! The first question when "nothing shows up" is whether data is arriving at
//! all, and on which pipeline. This surfaces the server's per-pipeline accept
//! counters so an agent can tell "exporter misconfigured" (no batches) from
//! "storage failing" (errors climbing) without log access.

use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output::print_json;

pub async fn status(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.ingest_status().await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let pipelines = result["pipelines"].as_array().cloned().unwrap_or_default();
            let mut table = comfy_table::Table::new();
            table.set_header(vec![
                "PIPELINE",
                "BATCHES",
                "RECORDS",
                "ERRORS",
                "LAST ACCEPTED",
            ]);
            for p in &pipelines {
                table.add_row(vec![
                    p["pipeline"].as_str().unwrap_or("-").to_string(),
                    p["batches"].as_u64().unwrap_or(0).to_string(),
                    p["records"].as_u64().unwrap_or(0).to_string(),
                    p["errors"].as_u64().unwrap_or(0).to_string(),
                    p["last_accepted_at"]
                        .as_str()
                        .unwrap_or("never")
                        .to_string(),
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}
