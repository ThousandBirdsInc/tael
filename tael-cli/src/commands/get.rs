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

/// Describe one metric. Answers "what is this and can I query it" before an
/// agent guesses at a filter that returns nothing — including whether the
/// points retained histogram buckets, which decides if `histogram_quantile`
/// will work.
pub async fn metric(
    client: &TaelClient,
    format: &OutputFormat,
    name: &str,
    last: Option<String>,
    limit: u32,
) -> Result<()> {
    let result = client.get_metric(name, last.as_deref(), limit).await?;
    if let Some(error) = result["error"].as_str() {
        match format {
            OutputFormat::Json => output::print_json(&result),
            OutputFormat::Table => println!("{error}"),
        }
        return Err(crate::exit::CategorizedError::no_results());
    }
    match format {
        OutputFormat::Json => output::print_json(&result),
        OutputFormat::Table => {
            println!("metric      {}", result["metric"].as_str().unwrap_or(name));
            println!("type        {}", result["type"].as_str().unwrap_or("-"));
            println!("unit        {}", result["unit"].as_str().unwrap_or("-"));
            println!(
                "points      {} across {} series",
                result["point_count"].as_u64().unwrap_or(0),
                result["series_count"].as_u64().unwrap_or(0)
            );
            println!(
                "value range {} .. {}",
                result["value_min"].as_f64().unwrap_or(0.0),
                result["value_max"].as_f64().unwrap_or(0.0)
            );
            let list = |key: &str| {
                result[key]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default()
            };
            println!("services    {}", list("services"));
            println!("label keys  {}", list("label_keys"));
            if result["histogram_quantile_available"].as_bool() == Some(true) {
                println!(
                    "quantiles   available — histogram_quantile(0.95, {}) will work",
                    result["metric"].as_str().unwrap_or(name)
                );
            }
        }
    }
    Ok(())
}
