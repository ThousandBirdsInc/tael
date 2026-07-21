use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use comfy_table::{Cell, Table};
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind};
use crate::output;

#[derive(Default)]
struct VariantStats {
    span_count: usize,
    trace_ids: HashSet<String>,
    error_count: usize,
    duration_sum: f64,
    signal_count: usize,
}

pub async fn compare(
    client: &TaelClient,
    format: &OutputFormat,
    experiment_id: &str,
    signal: Option<String>,
    last: Option<String>,
) -> Result<()> {
    let traces = client
        .query_traces(
            None,
            None,
            None,
            None,
            None,
            last.as_deref(),
            50_000,
            &[],
            None,
        )
        .await?;
    let mut variants: BTreeMap<String, VariantStats> = BTreeMap::new();
    let mut trace_to_variant: HashMap<String, String> = HashMap::new();

    for span in traces
        .get("spans")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let attrs = span.get("attributes").and_then(|v| v.as_object());
        let Some(attrs) = attrs else {
            continue;
        };
        // Two instrumentation paths resolve to (experiment, variant):
        //   1. explicit `tael.experiment.id` / `tael.experiment.variant` attrs;
        //   2. a Chidori `chidori.branch` fan-out — the run id is the
        //      experiment and each variant's spans carry `chidori.branch_label`,
        //      so `tael experiment compare <chidori_run_id>` works with no
        //      extra instrumentation.
        let variant =
            if attrs.get("tael.experiment.id").and_then(|v| v.as_str()) == Some(experiment_id) {
                attrs
                    .get("tael.experiment.variant")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string()
            } else if attrs.get("chidori.run_id").and_then(|v| v.as_str()) == Some(experiment_id) {
                match attrs.get("chidori.branch_label").and_then(|v| v.as_str()) {
                    Some(label) => label.to_string(),
                    // Non-branch spans of the run aren't part of any variant.
                    None => continue,
                }
            } else {
                continue;
            };
        let trace_id = span
            .get("trace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let entry = variants.entry(variant.clone()).or_default();
        entry.span_count += 1;
        if !trace_id.is_empty() {
            entry.trace_ids.insert(trace_id.clone());
            trace_to_variant.insert(trace_id, variant);
        }
        if span.get("status").and_then(|v| v.as_str()) == Some("error") {
            entry.error_count += 1;
        }
        entry.duration_sum += span
            .get("duration_ms")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
    }

    if let Some(signal_name) = &signal {
        for row in comment_rows(client, 50_000).await? {
            let Some(comment) = enriched_comment(&row) else {
                continue;
            };
            if !comment_matches_signal(&comment, signal_name) {
                continue;
            }
            if let Some(variant) = trace_to_variant.get(field(&comment, "trace_id")) {
                if let Some(stats) = variants.get_mut(variant) {
                    stats.signal_count += 1;
                }
            }
        }
    }

    let rows: Vec<Value> = variants
        .into_iter()
        .map(|(variant, stats)| {
            let trace_count = stats.trace_ids.len();
            serde_json::json!({
                "variant": variant,
                "trace_count": trace_count,
                "span_count": stats.span_count,
                "error_count": stats.error_count,
                "error_rate": if stats.span_count > 0 {
                    stats.error_count as f64 / stats.span_count as f64
                } else {
                    0.0
                },
                "avg_duration_ms": if stats.span_count > 0 {
                    stats.duration_sum / stats.span_count as f64
                } else {
                    0.0
                },
                "signal": signal,
                "signal_count": stats.signal_count,
                "signal_rate": if trace_count > 0 {
                    stats.signal_count as f64 / trace_count as f64
                } else {
                    0.0
                },
            })
        })
        .collect();
    let result = serde_json::json!({
        "experiment_id": experiment_id,
        "variants": rows,
        "count": rows.len(),
    });
    output::render(format, &result, print_experiment_compare);
    Ok(())
}

fn comment_matches_signal(comment: &Value, signal: &str) -> bool {
    (kind(comment) == Some("failure_review")
        && (comment.get("signal").and_then(|v| v.as_str()) == Some(signal)
            || comment.get("failure_mode").and_then(|v| v.as_str()) == Some(signal)))
        || (kind(comment) == Some("self_diagnostic")
            && comment.get("category").and_then(|v| v.as_str()) == Some(signal))
        || (kind(comment) == Some("signal_definition")
            && comment.get("name").and_then(|v| v.as_str()) == Some(signal))
}

fn print_experiment_compare(value: &Value) {
    let variants = match value.get("variants").and_then(|v| v.as_array()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            println!("No spans found for this experiment.");
            return;
        }
    };
    println!(
        "Experiment {}",
        value["experiment_id"].as_str().unwrap_or("-")
    );
    let mut table = Table::new();
    table.set_header(vec![
        "Variant", "Traces", "Spans", "Errors", "Error %", "Avg ms", "Signal", "Signal %",
    ]);
    for variant in variants {
        table.add_row(vec![
            Cell::new(field(variant, "variant")),
            Cell::new(variant["trace_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(variant["span_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(variant["error_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(format!(
                "{:.2}",
                variant["error_rate"].as_f64().unwrap_or(0.0) * 100.0
            )),
            Cell::new(format!(
                "{:.1}",
                variant["avg_duration_ms"].as_f64().unwrap_or(0.0)
            )),
            Cell::new(variant["signal_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(format!(
                "{:.2}",
                variant["signal_rate"].as_f64().unwrap_or(0.0) * 100.0
            )),
        ]);
    }
    println!("{table}");
}
