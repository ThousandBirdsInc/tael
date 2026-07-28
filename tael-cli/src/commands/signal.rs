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
    last: Option<String>,
) -> Result<()> {
    let cutoff = window_cutoff(last.as_deref())?;
    let mut definitions = Vec::new();
    let mut matches = Vec::new();
    let mut buckets: BTreeMap<String, usize> = BTreeMap::new();

    for row in comment_rows(client, limit).await? {
        let Some(comment) = enriched_comment(&row) else {
            continue;
        };
        if outside_window(&comment, cutoff) {
            continue;
        }
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
        "window": last,
        "definitions": definitions,
        "matches": matches,
        "buckets": bucket_rows,
        "count": matches.len(),
    });
    output::render(format, &result, print_signal_trend);
    Ok(())
}

/// Compare a signal's occurrence rate across groups of traces, grouped by a
/// span attribute (`--by tael.experiment.variant`, or any attribute key).
///
/// Groups come from spans: every trace whose spans carry the attribute joins
/// that attribute value's group. The rate is signal comments per grouped trace,
/// so a variant with more traffic doesn't look worse just for being busier.
pub async fn compare(
    client: &TaelClient,
    format: &OutputFormat,
    name: &str,
    by: &str,
    last: Option<String>,
    limit: u32,
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
            false,
        )
        .await?;

    // trace_id -> group value. `--by experiment.variant` also matches the
    // conventional `tael.`-prefixed attribute so the doc-style short form works.
    let mut trace_group: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let prefixed = format!("tael.{by}");
    for span in traces
        .get("spans")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(attrs) = span.get("attributes").and_then(|v| v.as_object()) else {
            continue;
        };
        let value = attrs
            .get(by)
            .or_else(|| attrs.get(&prefixed))
            .and_then(|v| v.as_str());
        let (Some(value), Some(trace_id)) = (value, span.get("trace_id").and_then(|v| v.as_str()))
        else {
            continue;
        };
        trace_group
            .entry(trace_id.to_string())
            .or_insert_with(|| value.to_string());
    }

    #[derive(Default)]
    struct GroupStats {
        traces: std::collections::HashSet<String>,
        signal_count: usize,
    }
    let mut groups: BTreeMap<String, GroupStats> = BTreeMap::new();
    for (trace_id, group) in &trace_group {
        groups
            .entry(group.clone())
            .or_default()
            .traces
            .insert(trace_id.clone());
    }

    let cutoff = window_cutoff(last.as_deref())?;
    for row in comment_rows(client, limit).await? {
        let Some(comment) = enriched_comment(&row) else {
            continue;
        };
        if outside_window(&comment, cutoff) {
            continue;
        }
        let matches_signal = (kind(&comment) == Some("failure_review")
            && (comment.get("signal").and_then(|v| v.as_str()) == Some(name)
                || comment.get("failure_mode").and_then(|v| v.as_str()) == Some(name)))
            || (kind(&comment) == Some("self_diagnostic")
                && comment.get("category").and_then(|v| v.as_str()) == Some(name));
        if !matches_signal {
            continue;
        }
        if let Some(group) = trace_group.get(field(&comment, "trace_id"))
            && let Some(stats) = groups.get_mut(group)
        {
            stats.signal_count += 1;
        }
    }

    let rows: Vec<Value> = groups
        .into_iter()
        .map(|(group, stats)| {
            let trace_count = stats.traces.len();
            serde_json::json!({
                "group": group,
                "trace_count": trace_count,
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
        "signal": name,
        "by": by,
        "window": last,
        "groups": rows,
        "count": rows.len(),
    });
    output::render(format, &result, print_signal_compare);
    Ok(())
}

/// Resolve `--last` into a UTC cutoff. `None` means no time filter.
fn window_cutoff(last: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let Some(last) = last else {
        return Ok(None);
    };
    let secs = crate::commands::alert::parse_duration_secs(last)?;
    Ok(Some(chrono::Utc::now() - chrono::Duration::seconds(secs)))
}

/// Whether a comment's `created_at` falls before the cutoff. Comments without a
/// parsable timestamp stay in — missing data should widen results, not hide them.
fn outside_window(comment: &Value, cutoff: Option<chrono::DateTime<chrono::Utc>>) -> bool {
    let Some(cutoff) = cutoff else {
        return false;
    };
    comment
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_some_and(|t| t.with_timezone(&chrono::Utc) < cutoff)
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

fn print_signal_compare(value: &Value) {
    let signal = value["signal"].as_str().unwrap_or("-");
    let by = value["by"].as_str().unwrap_or("-");
    println!("Signal {signal} by {by}");
    let groups = value["groups"].as_array().cloned().unwrap_or_default();
    if groups.is_empty() {
        println!("No spans carry that attribute in the window.");
        return;
    }
    let mut table = Table::new();
    table.set_header(vec!["Group", "Traces", "Signal", "Signal %"]);
    for group in &groups {
        table.add_row(vec![
            Cell::new(field(group, "group")),
            Cell::new(group["trace_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(group["signal_count"].as_u64().unwrap_or(0).to_string()),
            Cell::new(format!(
                "{:.2}",
                group["signal_rate"].as_f64().unwrap_or(0.0) * 100.0
            )),
        ]);
    }
    println!("{table}");
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
