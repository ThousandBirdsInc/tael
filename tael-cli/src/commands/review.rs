//! `tael review` — queued questions for a human, filed by an agent.
//!
//! Braintrust's review queue is built the other way around: humans are the
//! primary reviewers and the queue is the main workflow. tael inverts it. The
//! agent triages, and a human is asked only about the residue it cannot decide
//! — an ambiguous refusal, a low-confidence self-diagnostic, a judgement call
//! about intent. That keeps human attention on the cases where it is actually
//! the scarce resource.
//!
//! Requests and answers are structured trace comments, the same convention
//! issues, eval-case provenance, and self-diagnostics already use. No new
//! tables, and the whole queue is queryable through the existing comment
//! surface.

use anyhow::Result;
use comfy_table::Table;
use serde_json::Value;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind};
use crate::exit::CategorizedError;
use crate::output;

/// File a question about a trace for a human to answer.
pub async fn request(
    client: &TaelClient,
    format: &OutputFormat,
    trace_id: &str,
    question: &str,
    options: &[String],
    span_id: Option<&str>,
    case_id: Option<&str>,
    author: Option<&str>,
) -> Result<()> {
    let review_id = format!("review_{}", uuid::Uuid::new_v4().simple());
    let mut body = serde_json::json!({
        "kind": "review_request",
        "status": "open",
        "review_id": review_id,
        "question": question,
    });
    if !options.is_empty() {
        body["options"] = Value::Array(options.iter().map(|o| Value::String(o.clone())).collect());
    }
    // Linking to an eval case is what lets an answer flow back into the suite
    // as durable expected behavior rather than staying an isolated opinion.
    if let Some(case) = case_id {
        body["case_id"] = Value::String(case.to_string());
    }

    let result = client
        .add_comment(
            trace_id,
            &serde_json::to_string(&body)?,
            Some(author.unwrap_or("tael:review")),
            span_id,
        )
        .await?;

    match format {
        OutputFormat::Json => output::print_json(&serde_json::json!({
            "review_id": review_id,
            "trace_id": trace_id,
            "question": question,
            "status": "open",
            "comment": result.get("comment").cloned().unwrap_or(Value::Null),
        })),
        OutputFormat::Table => {
            println!("Filed {review_id} on trace {trace_id}");
            println!("  {question}");
            if !options.is_empty() {
                println!("  options: {}", options.join(", "));
            }
        }
    }
    Ok(())
}

/// List review requests, optionally filtered by state.
pub async fn list(
    client: &TaelClient,
    format: &OutputFormat,
    state: Option<&str>,
    limit: u32,
) -> Result<()> {
    let comments = comment_rows(client, limit).await?;
    let enriched: Vec<Value> = comments.iter().filter_map(enriched_comment).collect();

    // An answer is a separate comment referencing the request, so the request's
    // effective state is derived rather than mutated — comments are append-only.
    let answers: std::collections::HashMap<String, &Value> = enriched
        .iter()
        .filter(|v| kind(v) == Some("review_answer"))
        .filter_map(|v| {
            let id = v.get("review_id").and_then(Value::as_str)?;
            Some((id.to_string(), v))
        })
        .collect();

    let mut reviews: Vec<Value> = Vec::new();
    for request in enriched
        .iter()
        .filter(|v| kind(v) == Some("review_request"))
    {
        let review_id = request
            .get("review_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let answer = answers.get(&review_id);
        let effective = if answer.is_some() { "answered" } else { "open" };
        if let Some(want) = state
            && !want.eq_ignore_ascii_case(effective)
            && !want.eq_ignore_ascii_case("all")
        {
            continue;
        }
        let mut row = request.clone();
        row["state"] = Value::String(effective.to_string());
        if let Some(a) = answer {
            row["answer"] = (*a).clone();
        }
        reviews.push(row);
    }

    let result = serde_json::json!({ "reviews": reviews, "count": reviews.len() });
    match format {
        OutputFormat::Json => output::print_json(&result),
        OutputFormat::Table => {
            if reviews.is_empty() {
                println!("No review requests.");
            } else {
                let mut table = Table::new();
                table.set_header(vec!["REVIEW", "STATE", "TRACE", "QUESTION", "ANSWER"]);
                for r in &reviews {
                    table.add_row(vec![
                        field(r, "review_id").to_string(),
                        field(r, "state").to_string(),
                        field(r, "trace_id").chars().take(12).collect::<String>(),
                        field(r, "question").to_string(),
                        r.get("answer")
                            .map(|a| field(a, "answer").to_string())
                            .unwrap_or_default(),
                    ]);
                }
                println!("{table}");
            }
        }
    }
    if reviews.is_empty() {
        return Err(CategorizedError::no_results());
    }
    Ok(())
}

/// Answer a queued question.
pub async fn submit(
    client: &TaelClient,
    format: &OutputFormat,
    review_id: &str,
    answer: &str,
    note: Option<&str>,
    limit: u32,
    author: Option<&str>,
) -> Result<()> {
    // The answer must land on the same trace as the request, so a reviewer
    // supplies only the review id and the trace is looked up.
    let comments = comment_rows(client, limit).await?;
    let request = comments
        .iter()
        .filter_map(enriched_comment)
        .filter(|v| kind(v) == Some("review_request"))
        .find(|v| v.get("review_id").and_then(Value::as_str) == Some(review_id))
        .ok_or_else(|| {
            CategorizedError::new(
                crate::exit::ExitCategory::NoResults,
                format!("no review request with id `{review_id}` (see `tael review list`)"),
            )
        })?;

    let trace_id = field(&request, "trace_id").to_string();
    if trace_id.is_empty() {
        anyhow::bail!("review `{review_id}` has no trace to answer against");
    }

    // A constrained question is worth constraining the answer to; a free-form
    // one accepts anything.
    if let Some(options) = request.get("options").and_then(Value::as_array)
        && !options.is_empty()
        && !options.iter().any(|o| o.as_str() == Some(answer))
    {
        let allowed: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
        return Err(CategorizedError::new(
            crate::exit::ExitCategory::BadQuery,
            format!(
                "`{answer}` is not one of this review's options ({})",
                allowed.join(", ")
            ),
        )
        .into());
    }

    let mut body = serde_json::json!({
        "kind": "review_answer",
        "review_id": review_id,
        "answer": answer,
    });
    if let Some(n) = note {
        body["note"] = Value::String(n.to_string());
    }
    if let Some(case) = request.get("case_id").and_then(Value::as_str) {
        body["case_id"] = Value::String(case.to_string());
    }

    let result = client
        .add_comment(
            &trace_id,
            &serde_json::to_string(&body)?,
            Some(author.unwrap_or("human")),
            None,
        )
        .await?;

    match format {
        OutputFormat::Json => output::print_json(&serde_json::json!({
            "review_id": review_id,
            "trace_id": trace_id,
            "answer": answer,
            "status": "answered",
            "comment": result.get("comment").cloned().unwrap_or(Value::Null),
        })),
        OutputFormat::Table => println!("Answered {review_id}: {answer}"),
    }
    Ok(())
}
