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
    metric_sum: f64,
    metric_count: usize,
}

pub async fn compare(
    client: &TaelClient,
    format: &OutputFormat,
    experiment_id: Option<&str>,
    group_by: Option<&str>,
    signal: Option<String>,
    metric: Option<String>,
    last: Option<String>,
) -> Result<()> {
    if experiment_id.is_none() && group_by.is_none() {
        return Err(crate::exit::CategorizedError::new(
            crate::exit::ExitCategory::BadQuery,
            "pass an experiment id, or --group-by <span-attr> (e.g. --group-by git.commit) \
             to compare across attribute values"
                .to_string(),
        )
        .into());
    }
    let group_by_prefixed = group_by.map(|g| format!("tael.{g}"));
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
            false,
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
        // With an experiment id, only that experiment's spans participate.
        if let Some(experiment_id) = experiment_id {
            let in_experiment = attrs.get("tael.experiment.id").and_then(|v| v.as_str())
                == Some(experiment_id)
                || attrs.get("chidori.run_id").and_then(|v| v.as_str()) == Some(experiment_id);
            if !in_experiment {
                continue;
            }
        }
        let variant = if let Some(group_attr) = group_by {
            // `--group-by git.commit` groups by any span attribute; the
            // conventional `tael.`-prefixed form also matches. Spans without
            // the attribute belong to no group.
            match attrs
                .get(group_attr)
                .or_else(|| group_by_prefixed.as_deref().and_then(|g| attrs.get(g)))
                .and_then(|v| v.as_str())
            {
                Some(value) => value.to_string(),
                None => continue,
            }
        } else {
            // Two instrumentation paths resolve to a variant:
            //   1. explicit `tael.experiment.variant` attrs;
            //   2. a Chidori `chidori.branch` fan-out, where each variant's
            //      spans carry `chidori.branch_label` — so
            //      `tael experiment compare <chidori_run_id>` works with no
            //      extra instrumentation.
            if let Some(v) = attrs
                .get("tael.experiment.variant")
                .and_then(|v| v.as_str())
            {
                v.to_string()
            } else if let Some(label) = attrs.get("chidori.branch_label").and_then(|v| v.as_str()) {
                label.to_string()
            } else if attrs.contains_key("tael.experiment.id") {
                "unknown".to_string()
            } else {
                // Non-branch spans of a chidori run aren't part of any variant.
                continue;
            }
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
        // `--metric task_completion` averages a numeric span attribute of that
        // name (or `tael.metric.<name>`) across each variant's spans, so a
        // task-level outcome stamped on spans compares directly.
        if let Some(metric_name) = &metric {
            let value = attrs
                .get(metric_name.as_str())
                .or_else(|| attrs.get(&format!("tael.metric.{metric_name}")))
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                });
            if let Some(value) = value {
                entry.metric_sum += value;
                entry.metric_count += 1;
            }
        }
    }

    if let Some(signal_name) = &signal {
        for row in comment_rows(client, 50_000).await? {
            let Some(comment) = enriched_comment(&row) else {
                continue;
            };
            if !comment_matches_signal(&comment, signal_name) {
                continue;
            }
            if let Some(variant) = trace_to_variant.get(field(&comment, "trace_id"))
                && let Some(stats) = variants.get_mut(variant)
            {
                stats.signal_count += 1;
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
                "metric": metric,
                "metric_count": stats.metric_count,
                "metric_avg": if stats.metric_count > 0 {
                    Value::from(stats.metric_sum / stats.metric_count as f64)
                } else {
                    Value::Null
                },
            })
        })
        .collect();
    let result = serde_json::json!({
        "experiment_id": experiment_id,
        "group_by": group_by,
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
    match (value["experiment_id"].as_str(), value["group_by"].as_str()) {
        (Some(id), Some(by)) => println!("Experiment {id} by {by}"),
        (Some(id), None) => println!("Experiment {id}"),
        (None, Some(by)) => println!("Grouped by {by}"),
        (None, None) => {}
    }
    let has_metric = variants
        .iter()
        .any(|v| v.get("metric").is_some_and(|m| !m.is_null()));
    let group_label = if value["group_by"].is_string() {
        "Group"
    } else {
        "Variant"
    };
    let mut header = vec![
        group_label,
        "Traces",
        "Spans",
        "Errors",
        "Error %",
        "Avg ms",
        "Signal",
        "Signal %",
    ];
    if has_metric {
        header.push("Metric avg");
    }
    let mut table = Table::new();
    table.set_header(header);
    for variant in variants {
        let mut row = vec![
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
        ];
        if has_metric {
            row.push(Cell::new(
                variant["metric_avg"]
                    .as_f64()
                    .map(|v| format!("{v:.3}"))
                    .unwrap_or_else(|| "-".to_string()),
            ));
        }
        table.add_row(row);
    }
    println!("{table}");
}
