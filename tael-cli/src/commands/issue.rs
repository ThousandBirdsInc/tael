use anyhow::Result;
use comfy_table::{Cell, Table};
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind, short_trace};
use crate::output;

#[allow(clippy::too_many_arguments)]
pub async fn create(
    client: &TaelClient,
    format: &OutputFormat,
    from_trace: &str,
    failure_mode: &str,
    impact: &str,
    summary: &str,
    last_successful_step: Option<String>,
    first_failure: Option<String>,
    author: Option<String>,
) -> Result<()> {
    let issue_id = format!("issue_{}", uuid::Uuid::new_v4().simple());
    let mut body = serde_json::json!({
        "kind": "failure_review",
        "status": "issue",
        "issue_id": issue_id,
        "failure_mode": failure_mode,
        "impact": impact,
        "summary": summary,
        "representative": true,
    });
    if let Some(v) = last_successful_step.filter(|s| !s.is_empty()) {
        body["last_successful_step"] = Value::String(v);
    }
    if let Some(v) = first_failure.filter(|s| !s.is_empty()) {
        body["first_failure"] = Value::String(v);
    }

    let result = client
        .add_comment(
            from_trace,
            &serde_json::to_string(&body)?,
            Some(author.as_deref().unwrap_or("tael:issue")),
            None,
        )
        .await?;
    output::render(format, &result, print_issue_create);
    Ok(())
}

pub async fn list(client: &TaelClient, format: &OutputFormat, limit: u32) -> Result<()> {
    let issues: Vec<Value> = comment_rows(client, limit)
        .await?
        .iter()
        .filter_map(enriched_comment)
        .filter(|v| kind(v) == Some("failure_review"))
        .filter(|v| field(v, "status") == "issue")
        .collect();
    let result = serde_json::json!({ "issues": issues, "count": issues.len() });
    output::render(format, &result, print_issue_list);
    Ok(())
}

pub async fn examples(
    client: &TaelClient,
    format: &OutputFormat,
    issue_id: &str,
    limit: u32,
) -> Result<()> {
    let examples: Vec<Value> = comment_rows(client, limit)
        .await?
        .iter()
        .filter_map(enriched_comment)
        .filter(|v| {
            v.get("issue_id").and_then(|x| x.as_str()) == Some(issue_id)
                || v.get("source_issue_id").and_then(|x| x.as_str()) == Some(issue_id)
        })
        .collect();
    let result = serde_json::json!({
        "issue_id": issue_id,
        "examples": examples,
        "count": examples.len(),
    });
    output::render(format, &result, print_issue_examples);
    Ok(())
}

fn print_issue_create(value: &Value) {
    if let Some(comment) = value.get("comment") {
        let body: Value = comment["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        println!(
            "Issue {} added to trace {}: {}",
            field(&body, "issue_id"),
            comment["trace_id"].as_str().unwrap_or("-"),
            field(&body, "summary")
        );
    }
}

fn print_issue_list(value: &Value) {
    let issues = match value.get("issues").and_then(|v| v.as_array()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            println!("No issues found.");
            return;
        }
    };
    let mut table = Table::new();
    table.set_header(vec![
        "Issue", "Mode", "Impact", "Trace", "Summary", "Created",
    ]);
    for issue in issues {
        table.add_row(vec![
            Cell::new(field(issue, "issue_id")),
            Cell::new(field(issue, "failure_mode")),
            Cell::new(field(issue, "impact")),
            Cell::new(short_trace(field(issue, "trace_id"))),
            Cell::new(field(issue, "summary")),
            Cell::new(field(issue, "created_at")),
        ]);
    }
    println!("{table}");
}

fn print_issue_examples(value: &Value) {
    let examples = match value.get("examples").and_then(|v| v.as_array()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            println!("No examples found for this issue.");
            return;
        }
    };
    let mut table = Table::new();
    table.set_header(vec!["Trace", "Kind", "Mode", "Summary", "Created"]);
    for example in examples {
        table.add_row(vec![
            Cell::new(short_trace(field(example, "trace_id"))),
            Cell::new(field(example, "kind")),
            Cell::new(field(example, "failure_mode")),
            Cell::new(field(example, "summary")),
            Cell::new(field(example, "created_at")),
        ]);
    }
    println!("{table}");
}
