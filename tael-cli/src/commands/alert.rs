//! `tael alert` (manage rules) and `tael alerts` (read the feed).
//!
//! The split mirrors how they are used: rules are configured once, while the
//! feed is what an agent blocks on. `tael alerts --follow` is the long-poll
//! primitive — connect and be woken when something changes, rather than
//! polling `summarize` and diffing it yourself.

use anyhow::Result;
use serde_json::json;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::exit::{CategorizedError, ExitCategory};
use crate::output::print_json;

/// Create a rule. The server parses the query, so a typo fails here rather
/// than becoming a rule that silently never fires.
#[allow(clippy::too_many_arguments)]
pub async fn create(
    client: &TaelClient,
    format: &OutputFormat,
    name: &str,
    query: &str,
    for_duration: Option<&str>,
    window: Option<&str>,
    sinks: &[String],
    description: Option<&str>,
) -> Result<()> {
    let parsed_sinks = sinks
        .iter()
        .map(|s| parse_sink(s))
        .collect::<Result<Vec<_>>>()
        .map_err(|e| CategorizedError::new(ExitCategory::BadQuery, e.to_string()))?;

    let payload = json!({
        "name": name,
        "query": query,
        "for_seconds": for_duration.map(parse_duration_secs).transpose()?.unwrap_or(0),
        "window_seconds": window.map(parse_duration_secs).transpose()?,
        "sinks": parsed_sinks,
        "description": description,
    });

    let result = client.create_alert(&payload).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::BadQuery, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!(
            "Created alert `{}`: {}",
            result["created"].as_str().unwrap_or(name),
            result["query"].as_str().unwrap_or(query)
        ),
    }
    Ok(())
}

pub async fn list(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.list_alerts().await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let alerts = result["alerts"].as_array().cloned().unwrap_or_default();
            if alerts.is_empty() {
                println!("No alert rules. Create one with `tael alert create`.");
                return Ok(());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["NAME", "STATE", "QUERY", "FOR", "SINKS"]);
            for a in &alerts {
                table.add_row(vec![
                    a["name"].as_str().unwrap_or("-").to_string(),
                    a["state"].as_str().unwrap_or("ok").to_string(),
                    a["query"].as_str().unwrap_or("-").to_string(),
                    format!("{}s", a["for_seconds"].as_i64().unwrap_or(0)),
                    a["sinks"]
                        .as_array()
                        .map(|s| s.len().to_string())
                        .unwrap_or_else(|| "0".into()),
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

pub async fn delete(client: &TaelClient, format: &OutputFormat, name: &str) -> Result<()> {
    let result = client.delete_alert(name).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::NoResults, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!("Deleted alert `{name}`"),
    }
    Ok(())
}

/// Read the alert feed. With `--follow` this blocks, printing each transition
/// as it happens; without it, the recent buffer is printed and the command
/// returns.
pub async fn feed(
    client: &TaelClient,
    format: &OutputFormat,
    limit: u32,
    follow: bool,
) -> Result<()> {
    let recent = client.alert_events(limit).await?;
    let events = recent["events"].as_array().cloned().unwrap_or_default();

    match format {
        OutputFormat::Json => print_json(&recent),
        OutputFormat::Table => {
            if events.is_empty() && !follow {
                println!("No alert events.");
            }
            for e in &events {
                print_event(e);
            }
        }
    }

    if !follow {
        // Nothing has fired yet is a legitimate answer, distinguishable from
        // an error by the exit code.
        if events.is_empty() {
            return Err(CategorizedError::no_results());
        }
        return Ok(());
    }

    let mut stream = client.subscribe_alerts();
    while let Some(line) = stream.recv().await {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        match format {
            OutputFormat::Json => println!("{line}"),
            OutputFormat::Table => print_event(&event),
        }
    }
    Ok(())
}

fn print_event(event: &serde_json::Value) {
    let state = event["state"].as_str().unwrap_or("?");
    let marker = match state {
        "firing" => "FIRING ",
        "ok" => "RESOLVED",
        other => other,
    };
    println!(
        "[{}] {marker} {} — {}",
        event["at"].as_str().unwrap_or("-"),
        event["rule"].as_str().unwrap_or("?"),
        event["query"].as_str().unwrap_or("?"),
    );
    for m in event["matched"].as_array().into_iter().flatten() {
        let labels = m["labels"]
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        println!(
            "    {} {{{labels}}} = {}",
            m["metric"].as_str().unwrap_or("?"),
            m["value"].as_f64().unwrap_or(0.0)
        );
    }
}

/// Parse a `webhook=<url>` / `exec=<command>` sink into the server's shape.
fn parse_sink(spec: &str) -> Result<serde_json::Value> {
    match spec.split_once('=') {
        Some(("webhook", url)) if !url.trim().is_empty() => {
            Ok(json!({ "kind": "webhook", "url": url.trim() }))
        }
        Some(("exec", command)) if !command.trim().is_empty() => {
            Ok(json!({ "kind": "exec", "command": command.trim() }))
        }
        _ => anyhow::bail!("sink must be `webhook=<url>` or `exec=<command>`, got `{spec}`"),
    }
}

/// Parse `5m`, `30s`, `1h`, or a bare number of seconds.
fn parse_duration_secs(raw: &str) -> Result<i64> {
    let s = raw.trim();
    let (value, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let value: i64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("`{raw}` is not a valid duration"))?;
    Ok(match unit {
        "" | "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        "d" => value * 86400,
        other => anyhow::bail!("unknown duration unit `{other}` in `{raw}`"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_their_units() {
        assert_eq!(parse_duration_secs("30").unwrap(), 30);
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("5m").unwrap(), 300);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert!(parse_duration_secs("5y").is_err());
    }

    #[test]
    fn sinks_parse_into_the_servers_shape() {
        assert_eq!(
            parse_sink("webhook=https://example.test/h").unwrap(),
            json!({ "kind": "webhook", "url": "https://example.test/h" })
        );
        assert_eq!(
            parse_sink("exec=./notify.sh").unwrap(),
            json!({ "kind": "exec", "command": "./notify.sh" })
        );
        assert!(parse_sink("exec=").is_err());
        assert!(parse_sink("smoke-signal=up").is_err());
    }
}
