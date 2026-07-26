//! `tael eval suite push/pull/snapshot/diff` — server-managed case suites.
//!
//! JSONL stays the interchange format, so suites can still live in git; the
//! server becomes the system of record so two runs of "the same suite" are
//! provably the same data. `pull` emits canonical sorted JSONL, which means a
//! push/pull round trip is byte-stable and a suite file can be committed
//! without spurious diffs.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::exit::{CategorizedError, ExitCategory};
use crate::output::print_json;

/// Read a JSONL case file into (case_id, canonical JSON) pairs.
fn read_cases(path: &str) -> Result<Vec<(String, String)>> {
    let body = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let mut cases = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing JSONL case {} in {path}", i + 1))?;
        // `case_id` or `id`, matching what `tael eval run` accepts.
        let case_id = value
            .get("case_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("case {} in {path} has no `case_id` or `id`", i + 1))?
            .to_string();
        cases.push((case_id, canonical(&value)?));
    }
    Ok(cases)
}

/// Serialize with object keys sorted, so byte-different files holding the same
/// cases hash identically and a reformat is not mistaken for an edit.
fn canonical(value: &Value) -> Result<String> {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: BTreeMap<_, _> =
                    map.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                serde_json::to_value(sorted).unwrap_or(Value::Null)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    Ok(serde_json::to_string(&sort(value))?)
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Upload a JSONL file as a suite's working set, storing each case body as a
/// content-addressed blob so unchanged cases are shared across snapshots.
pub async fn push(
    client: &TaelClient,
    format: &OutputFormat,
    suite: &str,
    path: &str,
) -> Result<()> {
    let cases = read_cases(path)?;
    let mut refs = Vec::new();
    for (case_id, canonical) in &cases {
        // The blob is the case body; the hash is its identity on the server.
        let stored = client.put_blob(canonical).await?;
        let hash = stored["sha256"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| sha256_hex(canonical));
        refs.push(json!({ "case_id": case_id, "content_sha256": hash }));
    }

    let result = client.push_suite(suite, &json!({ "cases": refs })).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::BadQuery, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!(
            "Pushed {} case(s) to suite `{suite}`",
            result["case_count"].as_u64().unwrap_or(0)
        ),
    }
    Ok(())
}

/// Write a suite back out as canonical JSONL. `suite@snapshot` pulls a frozen
/// version rather than the working set.
pub async fn pull(
    client: &TaelClient,
    format: &OutputFormat,
    reference: &str,
    out: Option<&str>,
) -> Result<()> {
    let (name, snapshot) = match reference.split_once('@') {
        Some((n, s)) => (n, Some(s)),
        None => (reference, None),
    };
    let result = client.get_suite(name, snapshot).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::NoResults, error).into());
    }

    let mut lines = Vec::new();
    for case in result["cases"].as_array().into_iter().flatten() {
        let Some(hash) = case["content_sha256"].as_str() else {
            continue;
        };
        // The case body lives in the blob store; the suite holds only hashes.
        let body = client.get_blob(hash).await?;
        lines.push(body);
    }
    let jsonl = format!("{}\n", lines.join("\n"));

    match out {
        Some(path) => {
            std::fs::write(path, &jsonl).with_context(|| format!("writing {path}"))?;
            match format {
                OutputFormat::Json => print_json(&json!({
                    "suite": name,
                    "snapshot": result["snapshot"],
                    "case_count": lines.len(),
                    "written": path,
                })),
                OutputFormat::Table => {
                    println!("Wrote {} case(s) to {path}", lines.len())
                }
            }
        }
        None => print!("{jsonl}"),
    }
    Ok(())
}

/// Freeze the working set. Pinning a run to a snapshot is what makes two
/// experiments comparable.
pub async fn snapshot(
    client: &TaelClient,
    format: &OutputFormat,
    suite: &str,
    note: Option<&str>,
) -> Result<()> {
    let result = client.snapshot_suite(suite, note).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::BadQuery, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => println!(
            "Snapshot {} of suite `{suite}` ({} cases)\nPin a run to it with --suite {suite}@{}",
            result["snapshot"].as_str().unwrap_or("?"),
            result["case_count"].as_u64().unwrap_or(0),
            result["snapshot"].as_str().unwrap_or("?"),
        ),
    }
    Ok(())
}

pub async fn list(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.list_suites().await?;
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let suites = result["suites"].as_array().cloned().unwrap_or_default();
            if suites.is_empty() {
                println!("No suites. Push one with `tael eval suite push <name> cases.jsonl`.");
                return Ok(());
            }
            let mut table = comfy_table::Table::new();
            table.set_header(vec!["NAME", "CASES", "SNAPSHOTS", "LATEST SNAPSHOT"]);
            for s in &suites {
                table.add_row(vec![
                    s["name"].as_str().unwrap_or("-").to_string(),
                    s["case_count"].as_u64().unwrap_or(0).to_string(),
                    s["snapshot_count"].as_u64().unwrap_or(0).to_string(),
                    s["latest_snapshot"].as_str().unwrap_or("-").to_string(),
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

/// Compare two suite references. An edited case is reported as changed rather
/// than as an add plus a remove — it is still the same case.
pub async fn diff(client: &TaelClient, format: &OutputFormat, from: &str, to: &str) -> Result<()> {
    let result = client.diff_suites(from, to).await?;
    if let Some(error) = result["error"].as_str() {
        return Err(CategorizedError::new(ExitCategory::NoResults, error).into());
    }
    match format {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Table => {
            let count = |key: &str| result[key].as_array().map(|a| a.len()).unwrap_or(0);
            println!(
                "{from} -> {to}: {} added, {} removed, {} changed, {} unchanged",
                count("added"),
                count("removed"),
                count("changed"),
                result["unchanged"].as_u64().unwrap_or(0),
            );
            for (label, key) in [("+", "added"), ("-", "removed")] {
                for c in result[key].as_array().into_iter().flatten() {
                    println!("  {label} {}", c["case_id"].as_str().unwrap_or("?"));
                }
            }
            for c in result["changed"].as_array().into_iter().flatten() {
                println!("  ~ {}", c["case_id"].as_str().unwrap_or("?"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalization_is_key_order_independent() {
        let a = canonical(&json!({ "b": 1, "a": 2 })).unwrap();
        let b = canonical(&json!({ "a": 2, "b": 1 })).unwrap();
        assert_eq!(sha256_hex(&a), sha256_hex(&b));
    }

    #[test]
    fn cases_accept_either_id_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases.jsonl");
        std::fs::write(
            &path,
            "{\"case_id\":\"a\",\"input\":\"x\"}\n\n{\"id\":\"b\",\"input\":\"y\"}\n",
        )
        .unwrap();
        let cases = read_cases(path.to_str().unwrap()).unwrap();
        assert_eq!(cases.len(), 2, "blank lines are skipped");
        assert_eq!(cases[0].0, "a");
        assert_eq!(cases[1].0, "b");
    }

    #[test]
    fn a_case_without_an_id_names_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cases.jsonl");
        std::fs::write(&path, "{\"input\":\"x\"}\n").unwrap();
        let err = read_cases(path.to_str().unwrap()).unwrap_err().to_string();
        assert!(err.contains("case 1"), "{err}");
    }
}
