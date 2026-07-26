//! `tael score rule` — manage online scoring of production traffic.
//!
//! The counterpart to `tael eval run`: instead of scoring a fixed set of golden
//! cases, a rule samples live traffic and runs the same scorer against it. The
//! scorer contract is identical, so one script works in both places.

use anyhow::Result;
use serde_json::json;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::exit::{CategorizedError, ExitCategory};
use crate::output::print_json;

pub async fn create(
    client: &TaelClient,
    format: &OutputFormat,
    name: &str,
    sample: f64,
    matches: &[String],
    command: &str,
    description: Option<&str>,
) -> Result<()> {
    let matcher = parse_matcher(matches)
        .map_err(|e| CategorizedError::new(ExitCategory::BadQuery, e.to_string()))?;
    let payload = json!({
        "name": name,
        "sample": sample,
        "matcher": matcher,
        "command": command,
        "description": description,
    });

    let result = client.create_score_rule(&payload).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::BadQuery, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!(
            "Created score rule `{}` sampling {:.1}% of matching traces",
            result["created"].as_str().unwrap_or(name),
            result["sample"].as_f64().unwrap_or(sample) * 100.0
        ),
    }
    Ok(())
}

/// List rules with their progress. `seen` versus `sampled` shows the effective
/// rate, and a non-zero failure count with a `last_error` is how a silently
/// broken judge gets noticed.
pub async fn list(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.list_score_rules().await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let rules = result["rules"].as_array().cloned().unwrap_or_default();
            if rules.is_empty() {
                println!("No score rules. Create one with `tael score rule create`.");
                return Ok(());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec![
                "NAME", "SAMPLE", "SEEN", "SAMPLED", "SCORES", "FAILURES",
            ]);
            for r in &rules {
                let s = &r["status"];
                table.add_row(vec![
                    r["name"].as_str().unwrap_or("-").to_string(),
                    format!("{:.1}%", r["sample"].as_f64().unwrap_or(0.0) * 100.0),
                    s["traces_seen"].as_u64().unwrap_or(0).to_string(),
                    s["traces_sampled"].as_u64().unwrap_or(0).to_string(),
                    s["scores_written"].as_u64().unwrap_or(0).to_string(),
                    s["failures"].as_u64().unwrap_or(0).to_string(),
                ]);
            }
            println!("{table}");
            for r in &rules {
                if let Some(err) = r["status"]["last_error"].as_str() {
                    println!(
                        "\n{}: last error — {err}",
                        r["name"].as_str().unwrap_or("?")
                    );
                }
            }
        }
    }
    Ok(())
}

pub async fn delete(client: &TaelClient, format: &OutputFormat, name: &str) -> Result<()> {
    let result = client.delete_score_rule(name).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::NoResults, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!("Deleted score rule `{name}`"),
    }
    Ok(())
}

/// Build the server's matcher shape from `key=value` selectors.
fn parse_matcher(specs: &[String]) -> Result<serde_json::Value> {
    let mut matcher = json!({});
    let mut attributes: Vec<(String, String)> = Vec::new();
    for spec in specs {
        let (key, value) = spec
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("match `{spec}` must be key=value"))?;
        match key.trim() {
            "service" | "operation" | "status" => {
                matcher[key.trim()] = json!(value);
            }
            "min_duration_ms" => {
                matcher["min_duration_ms"] = json!(value.parse::<f64>()?);
            }
            other => match other.strip_prefix("attribute:") {
                Some(attr) if !attr.is_empty() => {
                    attributes.push((attr.to_string(), value.to_string()))
                }
                _ => anyhow::bail!(
                    "unknown match key `{other}` \
                     (use service, operation, status, min_duration_ms, or attribute:<key>)"
                ),
            },
        }
    }
    matcher["attributes"] = json!(attributes);
    Ok(matcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matchers_build_the_servers_shape() {
        let m = parse_matcher(&[
            "service=agent-api".into(),
            "attribute:gen_ai.system=anthropic".into(),
        ])
        .unwrap();
        assert_eq!(m["service"], "agent-api");
        assert_eq!(m["attributes"][0][0], "gen_ai.system");
        assert_eq!(m["attributes"][0][1], "anthropic");
    }

    #[test]
    fn unknown_match_keys_are_rejected_with_the_valid_set() {
        let err = parse_matcher(&["colour=blue".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("attribute:"), "{err}");
    }
}
