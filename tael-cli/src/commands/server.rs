use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;

pub async fn status(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    match client.healthz().await {
        Ok(resp) => match format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({ "status": "healthy", "response": resp })
                );
            }
            OutputFormat::Table => {
                println!("Server: healthy");
            }
        },
        Err(e) => match format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({ "status": "unreachable", "error": e.to_string() })
                );
            }
            OutputFormat::Table => {
                eprintln!("Server unreachable: {e}");
            }
        },
    }
    Ok(())
}
