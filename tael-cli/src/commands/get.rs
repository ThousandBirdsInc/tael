use anyhow::Result;
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn trace(client: &TaelClient, format: &OutputFormat, trace_id: &str) -> Result<()> {
    let result = client.get_trace(trace_id).await?;
    output::render(format, &result, print_trace_with_correlation);
    Ok(())
}

/// The spans table plus a Chidori correlation footer: when the trace was
/// emitted by a Chidori agent run, print the run id / checkpoint path /
/// branch labels so the round-trip (trace → `chidori resume <run_id>`,
/// trace → `tael experiment compare <run_id>`) is one copy-paste away.
fn print_trace_with_correlation(value: &Value) {
    output::print_spans_table(value);

    let spans = value
        .get("spans")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    let attr = |key: &str| -> Option<String> {
        spans
            .iter()
            .find_map(|s| s.get("attributes")?.get(key)?.as_str().map(str::to_string))
    };
    let Some(run_id) = attr("chidori.run_id") else {
        return;
    };
    println!();
    println!("Chidori run: {run_id}");
    if let Some(path) = attr("chidori.checkpoint_path") {
        println!("Checkpoint:  {path}");
    }
    let mut labels: Vec<String> = spans
        .iter()
        .filter_map(|s| {
            s.get("attributes")?
                .get("chidori.branch_label")?
                .as_str()
                .map(str::to_string)
        })
        .collect();
    labels.sort();
    labels.dedup();
    if !labels.is_empty() {
        println!("Branches:    {}", labels.join(", "));
        println!("Compare:     tael experiment compare {run_id}");
    }
    println!("Replay ($0): chidori resume <agent.ts> {run_id} --ci");
}
