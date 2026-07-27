//! `tael topology` and `tael diff` — the two comparison/structure commands
//! DESIGN.md specified at M3.
//!
//! `topology` answers "what calls what", reconstructed from spans rather than
//! declared anywhere, so it reflects what the system actually did.
//! `diff` is the unopinionated sibling of `anomalies`: it reports every delta
//! between two windows and leaves the judgement to the caller.

use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::exit::CategorizedError;
use crate::output::print_json;

pub async fn run(
    client: &TaelClient,
    format: &OutputFormat,
    last: Option<String>,
    limit: u32,
) -> Result<()> {
    let result = client.topology(last.as_deref(), limit).await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let edges = result["edges"].as_array().cloned().unwrap_or_default();
            if edges.is_empty() {
                println!(
                    "No cross-service calls found in {} spans examined.",
                    result["spans_examined"].as_u64().unwrap_or(0)
                );
            } else {
                let mut table = comfy_table::Table::new();
                table.set_header(vec!["FROM", "TO", "CALLS", "ERRORS", "ERR RATE", "AVG MS"]);
                for e in &edges {
                    table.add_row(vec![
                        e["from"].as_str().unwrap_or("-").to_string(),
                        e["to"].as_str().unwrap_or("-").to_string(),
                        e["calls"].as_i64().unwrap_or(0).to_string(),
                        e["errors"].as_i64().unwrap_or(0).to_string(),
                        format!("{:.1}%", e["error_rate"].as_f64().unwrap_or(0.0) * 100.0),
                        format!("{:.1}", e["avg_duration_ms"].as_f64().unwrap_or(0.0)),
                    ]);
                }
                println!("{table}");
            }
            // A high dangling count means the window cut through live traces,
            // so the graph is missing edges rather than the callers not existing.
            let dangling = result["spans_with_parent_outside_window"]
                .as_u64()
                .unwrap_or(0);
            if dangling > 0 {
                println!(
                    "\n{dangling} span(s) had a parent outside the window; widen --last for a \
                     complete graph."
                );
            }
        }
    }
    if result["edges"].as_array().is_none_or(|e| e.is_empty()) {
        return Err(CategorizedError::no_results());
    }
    Ok(())
}

pub async fn diff(
    client: &TaelClient,
    format: &OutputFormat,
    last: Option<String>,
    baseline: Option<String>,
    service: Option<String>,
) -> Result<()> {
    let result = client
        .diff(last.as_deref(), baseline.as_deref(), service.as_deref())
        .await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            println!(
                "current {}s vs baseline {}s{}",
                result["current_window_seconds"].as_i64().unwrap_or(0),
                result["baseline_window_seconds"].as_i64().unwrap_or(0),
                result["service_filter"]
                    .as_str()
                    .map(|s| format!(" (service {s})"))
                    .unwrap_or_default(),
            );
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["METRIC", "CURRENT", "BASELINE", "DELTA", "RATIO"]);
            for key in [
                "spans_per_second",
                "error_rate",
                "p50_ms",
                "p95_ms",
                "p99_ms",
                "avg_ms",
                "log_errors_per_second",
            ] {
                let d = &result[key];
                let ratio = d["ratio"].as_f64().unwrap_or(f64::NAN);
                table.add_row(vec![
                    key.to_string(),
                    format!("{:.4}", d["current"].as_f64().unwrap_or(0.0)),
                    format!("{:.4}", d["baseline"].as_f64().unwrap_or(0.0)),
                    format!("{:+.4}", d["delta"].as_f64().unwrap_or(0.0)),
                    if ratio.is_finite() {
                        format!("{ratio:.2}x")
                    } else {
                        "-".to_string()
                    },
                ]);
            }
            println!("{table}");
            if let Some(note) = result["note"].as_str() {
                println!("\nnote: {note}");
            }
        }
    }
    Ok(())
}
