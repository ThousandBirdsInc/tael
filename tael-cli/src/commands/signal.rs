use std::collections::BTreeMap;

use anyhow::Result;
use comfy_table::{Cell, Table};
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind, short_trace};
use crate::output;

pub async fn create(
    client: &TaelClient,
    format: &OutputFormat,
    from_trace: &str,
    name: &str,
    query: Option<String>,
    failure_mode: Option<String>,
    summary: Option<String>,
    author: Option<String>,
) -> Result<()> {
    let signal_id = format!("signal_{}", uuid::Uuid::new_v4().simple());
    let mut body = serde_json::json!({
        "kind": "signal_definition",
        "status": "signal",
        "signal_id": signal_id,
        "name": name,
    });
    if let Some(v) = query.filter(|s| !s.is_empty()) {
        body["query"] = Value::String(v);
    }
    if let Some(v) = failure_mode.filter(|s| !s.is_empty()) {
        body["failure_mode"] = Value::String(v);
    }
    if let Some(v) = summary.filter(|s| !s.is_empty()) {
        body["summary"] = Value::String(v);
    }

    let result = client
        .add_comment(
            from_trace,
            &serde_json::to_string(&body)?,
            Some(author.as_deref().unwrap_or("tael:signal")),
            None,
        )
        .await?;
    output::render(format, &result, print_signal_create);
    Ok(())
}

pub async fn trend(
    client: &TaelClient,
    format: &OutputFormat,
    name: &str,
    limit: u32,
) -> Result<()> {
    let mut definitions = Vec::new();
    let mut matches = Vec::new();
    let mut buckets: BTreeMap<String, usize> = BTreeMap::new();

    for row in comment_rows(client, limit).await? {
        let Some(comment) = enriched_comment(&row) else {
            continue;
        };
        let is_definition = kind(&comment) == Some("signal_definition")
            && comment.get("name").and_then(|v| v.as_str()) == Some(name);
        let is_failure_signal = kind(&comment) == Some("failure_review")
            && (comment.get("signal").and_then(|v| v.as_str()) == Some(name)
                || comment.get("failure_mode").and_then(|v| v.as_str()) == Some(name)
                || comment.get("status").and_then(|v| v.as_str()) == Some("signal"));
        let is_self_diag = kind(&comment) == Some("self_diagnostic")
            && comment.get("category").and_then(|v| v.as_str()) == Some(name);

        if is_definition {
            definitions.push(comment.clone());
        }
        if is_definition || is_failure_signal || is_self_diag {
            let day = field(&comment, "created_at")
                .chars()
                .take(10)
                .collect::<String>();
            *buckets.entry(day).or_insert(0) += 1;
            matches.push(comment);
        }
    }

    let bucket_rows: Vec<Value> = buckets
        .into_iter()
        .map(|(date, count)| serde_json::json!({ "date": date, "count": count }))
        .collect();
    let result = serde_json::json!({
        "signal": name,
        "definitions": definitions,
        "matches": matches,
        "buckets": bucket_rows,
        "count": matches.len(),
    });
    output::render(format, &result, print_signal_trend);
    Ok(())
}

fn print_signal_create(value: &Value) {
    if let Some(comment) = value.get("comment") {
        let body: Value = comment["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        println!(
            "Signal {} ({}) added to trace {}",
            field(&body, "signal_id"),
            field(&body, "name"),
            comment["trace_id"].as_str().unwrap_or("-")
        );
    }
}

fn print_signal_trend(value: &Value) {
    let signal = value["signal"].as_str().unwrap_or("-");
    println!("Signal trend: {signal}");
    let buckets = value["buckets"].as_array().cloned().unwrap_or_default();
    if buckets.is_empty() {
        println!("No signal matches found.");
        return;
    }
    let mut table = Table::new();
    table.set_header(vec!["Date", "Count"]);
    for bucket in &buckets {
        table.add_row(vec![
            Cell::new(field(bucket, "date")),
            Cell::new(bucket["count"].as_u64().unwrap_or(0).to_string()),
        ]);
    }
    println!("{table}");

    if let Some(matches) = value["matches"].as_array() {
        println!();
        let mut examples = Table::new();
        examples.set_header(vec!["Trace", "Kind", "Summary"]);
        for item in matches.iter().take(10) {
            examples.add_row(vec![
                Cell::new(short_trace(field(item, "trace_id"))),
                Cell::new(field(item, "kind")),
                Cell::new(field(item, "summary")),
            ]);
        }
        println!("{examples}");
    }
}
