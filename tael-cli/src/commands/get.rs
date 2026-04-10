use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn trace(client: &TaelClient, format: &OutputFormat, trace_id: &str) -> Result<()> {
    let result = client.get_trace(trace_id).await?;
    output::render(format, &result, output::print_spans_table);
    Ok(())
}
