use anyhow::Result;
use comfy_table::{Cell, Table};
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind, short_trace};
use crate::output;

pub async fn report(
    client: &TaelClient,
    format: &OutputFormat,
    trace_id: &str,
    span_id: Option<String>,
    category: &str,
    severity: &str,
    confidence: &str,
    summary: &str,
    author: Option<String>,
) -> Result<()> {
    let body = serde_json::json!({
        "kind": "self_diagnostic",
        "category": category,
        "severity": severity,
        "confidence": confidence,
        "summary": summary,
    });
    let result = client
        .add_comment(
            trace_id,
            &serde_json::to_string(&body)?,
            Some(author.as_deref().unwrap_or("tael:self-diagnostic")),
            span_id.as_deref(),
        )
        .await?;
    output::render(format, &result, print_diagnostic_report);
    Ok(())
}

pub async fn list(client: &TaelClient, format: &OutputFormat, limit: u32) -> Result<()> {
    let diagnostics: Vec<Value> = comment_rows(client, limit)
        .await?
        .iter()
        .filter_map(enriched_comment)
        .filter(|v| kind(v) == Some("self_diagnostic"))
        .collect();
    let result = serde_json::json!({ "diagnostics": diagnostics, "count": diagnostics.len() });
    output::render(format, &result, print_diagnostic_list);
    Ok(())
}

fn print_diagnostic_report(value: &Value) {
    if let Some(comment) = value.get("comment") {
        let body: Value = comment["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        println!(
            "Self diagnostic added to trace {}: {} [{} / {}]",
            comment["trace_id"].as_str().unwrap_or("-"),
            field(&body, "summary"),
            field(&body, "category"),
            field(&body, "confidence")
        );
    }
}

fn print_diagnostic_list(value: &Value) {
    let diagnostics = match value.get("diagnostics").and_then(|v| v.as_array()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            println!("No self diagnostics found.");
            return;
        }
    };
    let mut table = Table::new();
    table.set_header(vec![
        "Trace",
        "Category",
        "Severity",
        "Confidence",
        "Summary",
        "Created",
    ]);
    for diag in diagnostics {
        table.add_row(vec![
            Cell::new(short_trace(field(diag, "trace_id"))),
            Cell::new(field(diag, "category")),
            Cell::new(field(diag, "severity")),
            Cell::new(field(diag, "confidence")),
            Cell::new(field(diag, "summary")),
            Cell::new(field(diag, "created_at")),
        ]);
    }
    println!("{table}");
}
