use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn run(
    client: &TaelClient,
    format: &OutputFormat,
    last: Option<String>,
    service: Option<String>,
) -> Result<()> {
    let result = client.summary(last.as_deref(), service.as_deref()).await?;
    output::render(format, &result, output::print_summary);
    Ok(())
}
