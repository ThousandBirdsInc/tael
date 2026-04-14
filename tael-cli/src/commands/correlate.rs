use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn run(client: &TaelClient, format: &OutputFormat, trace: &str) -> Result<()> {
    let result = client.correlate(trace).await?;
    output::render(format, &result, output::print_correlate);
    Ok(())
}
