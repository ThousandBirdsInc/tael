use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn list(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.list_services().await?;
    output::render(format, &result, output::print_services_table);
    Ok(())
}
