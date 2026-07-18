use std::fs;

use anyhow::{Context, Result};
use comfy_table::{Cell, Table};
use serde_json::Value;
use tokio::process::Command;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::reliability::{comment_rows, enriched_comment, field, kind, short_trace};
use crate::output;

pub async fn runs(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    let result = client.eval_runs().await?;
    output::render(format, &result, output::print_eval_runs);
    Ok(())
}

pub async fn status(client: &TaelClient, format: &OutputFormat, run_id: &str) -> Result<()> {
    let result = client.eval_status(run_id).await?;
    output::render(format, &result, output::print_eval_status);
    Ok(())
}

pub async fn cases(client: &TaelClient, format: &OutputFormat, run_id: &str) -> Result<()> {
    let result = client.eval_cases(run_id).await?;
    output::render(format, &result, output::print_eval_cases);
    Ok(())
}

pub async fn scores(client: &TaelClient, format: &OutputFormat, run_id: &str) -> Result<()> {
    let result = client.eval_scores(run_id).await?;
    output::render(format, &result, output::print_eval_scores);
    Ok(())
}

pub async fn report(client: &TaelClient, format: &OutputFormat, run_id: &str) -> Result<()> {
    let status = client.eval_status(run_id).await?;
    let cases = client.eval_cases(run_id).await?;
    let result = serde_json::json!({
        "run": status.get("run").cloned().unwrap_or(Value::Null),
        "cases": cases.get("cases").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
    });
    output::render(format, &result, output::print_eval_report);
    Ok(())
}

pub async fn compare(
    client: &TaelClient,
    format: &OutputFormat,
    run_id: &str,
    baseline: &str,
) -> Result<()> {
    let result = client.eval_compare(run_id, baseline).await?;
    output::render(format, &result, output::print_eval_compare);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn case_add(
    client: &TaelClient,
    format: &OutputFormat,
    from_trace: &str,
    suite: &str,
    case_id: &str,
    failure_mode: Option<String>,
    source_issue_id: Option<String>,
    critical_path: bool,
    expected_behavior: Option<String>,
    author: Option<String>,
) -> Result<()> {
    let mut body = serde_json::json!({
        "kind": "eval_case",
        "suite": suite,
        "case_id": case_id,
        "source_trace_id": from_trace,
        "created_from": "production",
        "critical_path": critical_path,
    });
    if let Some(v) = failure_mode.filter(|s| !s.is_empty()) {
        body["failure_mode"] = Value::String(v);
    }
    if let Some(v) = source_issue_id.filter(|s| !s.is_empty()) {
        body["source_issue_id"] = Value::String(v);
    }
    if let Some(v) = expected_behavior.filter(|s| !s.is_empty()) {
        body["expected_behavior"] = Value::String(v);
    }
    // When the source trace came from a Chidori agent, the run is not just
    // described by the trace — it IS a replayable artifact. Capture the run id
    // and checkpoint path so the case's fixture is the checkpoint itself:
    // `chidori resume <agent.ts> <run_id> --ci` replays it byte-for-byte at $0
    // (see docs/chidori.md for the eval-run recipes).
    if let Some((run_id, checkpoint)) = chidori_run_ref(client, from_trace).await {
        body["chidori_run_id"] = Value::String(run_id);
        if let Some(path) = checkpoint {
            body["chidori_checkpoint_path"] = Value::String(path);
        }
    }

    let result = client
        .add_comment(
            from_trace,
            &serde_json::to_string(&body)?,
            Some(author.as_deref().unwrap_or("tael:eval-case")),
            None,
        )
        .await?;
    output::render(format, &result, print_eval_case_add);
    Ok(())
}

pub async fn case_link(
    client: &TaelClient,
    format: &OutputFormat,
    case_id: &str,
    issue_id: &str,
    trace_id: Option<String>,
) -> Result<()> {
    let target_trace = match trace_id {
        Some(t) => t,
        None => find_eval_case_trace(client, case_id, 50_000)
            .await?
            .ok_or_else(|| anyhow::anyhow!("eval case {case_id} not found; pass --trace-id"))?,
    };
    let body = serde_json::json!({
        "kind": "eval_case_link",
        "case_id": case_id,
        "source_issue_id": issue_id,
    });
    let result = client
        .add_comment(
            &target_trace,
            &serde_json::to_string(&body)?,
            Some("tael:eval-case"),
            None,
        )
        .await?;
    output::render(format, &result, print_eval_case_link);
    Ok(())
}

pub async fn suite_inspect(
    client: &TaelClient,
    format: &OutputFormat,
    suite: &str,
    limit: u32,
) -> Result<()> {
    let cases: Vec<Value> = comment_rows(client, limit)
        .await?
        .iter()
        .filter_map(enriched_comment)
        .filter(|v| kind(v) == Some("eval_case"))
        .filter(|v| v.get("suite").and_then(|x| x.as_str()) == Some(suite))
        .collect();

    let mut failure_modes = std::collections::BTreeMap::<String, usize>::new();
    let mut provenance_free = Vec::new();
    let mut missing_expected = Vec::new();
    let mut critical_path_count = 0usize;
    for case in &cases {
        if case
            .get("source_trace_id")
            .and_then(|v| v.as_str())
            .is_none()
        {
            provenance_free.push(case.clone());
        }
        if case
            .get("expected_behavior")
            .and_then(|v| v.as_str())
            .is_none()
        {
            missing_expected.push(case.clone());
        }
        if case
            .get("critical_path")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            critical_path_count += 1;
        }
        if let Some(mode) = case.get("failure_mode").and_then(|v| v.as_str()) {
            *failure_modes.entry(mode.to_string()).or_insert(0) += 1;
        }
    }
    let duplicate_failure_modes: Vec<Value> = failure_modes
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(failure_mode, count)| serde_json::json!({ "failure_mode": failure_mode, "count": count }))
        .collect();
    let result = serde_json::json!({
        "suite": suite,
        "case_count": cases.len(),
        "critical_path_count": critical_path_count,
        "provenance_free": provenance_free,
        "missing_expected_behavior": missing_expected,
        "duplicate_failure_modes": duplicate_failure_modes,
        "cases": cases,
    });
    output::render(format, &result, print_eval_suite_inspect);
    Ok(())
}

pub async fn score(
    client: &TaelClient,
    format: &OutputFormat,
    run_id: &str,
    path: &str,
) -> Result<()> {
    let body = fs::read_to_string(path).with_context(|| format!("reading score file {path}"))?;
    let mut created = Vec::new();
    for (idx, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut value: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing JSONL score {} in {path}", idx + 1))?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("score line {} must be a JSON object", idx + 1))?;
        obj.entry("run_id".to_string())
            .or_insert_with(|| Value::String(run_id.to_string()));
        if !obj.contains_key("rationale_sha256") {
            if let Some(rationale) = obj
                .remove("rationale")
                .and_then(|v| v.as_str().map(str::to_string))
            {
                let blob = client.put_blob(&rationale).await?;
                if let Some(hash) = blob.get("sha256").and_then(|v| v.as_str()) {
                    obj.insert(
                        "rationale_sha256".to_string(),
                        Value::String(hash.to_string()),
                    );
                }
            }
        }
        let result = client.add_eval_score(&value).await?;
        created.push(result.get("score").cloned().unwrap_or(Value::Null));
    }

    let result = serde_json::json!({ "scores": created, "count": created.len() });
    output::render(format, &result, output::print_eval_scores);
    Ok(())
}

pub async fn run(
    client: &TaelClient,
    server: &str,
    cases_path: &str,
    suite: &str,
    cmd_template: &str,
    code_version: Option<String>,
    run_id: Option<String>,
) -> Result<()> {
    let body =
        fs::read_to_string(cases_path).with_context(|| format!("reading cases {cases_path}"))?;
    let cases: Vec<Value> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(idx, line)| {
            serde_json::from_str::<Value>(line)
                .with_context(|| format!("parsing JSONL case {} in {cases_path}", idx + 1))
        })
        .collect::<Result<Vec<_>>>()?;

    let run_id =
        run_id.unwrap_or_else(|| format!("run_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")));
    let case_count = cases.len().to_string();
    println!("eval run {run_id}: {} cases", cases.len());

    for (idx, case) in cases.iter().enumerate() {
        let case_id = case
            .get("case_id")
            .or_else(|| case.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("case_{:04}", idx + 1));
        let case_index = idx.to_string();
        let rendered = cmd_template
            .replace("{case_id}", &case_id)
            .replace("{case_index}", &case_index)
            .replace("{run_id}", &run_id)
            .replace("{suite_id}", suite);
        let (trace_id, span_id) = new_trace_span_ids();
        let started_at = chrono::Utc::now();
        write_runner_span(
            client,
            suite,
            &run_id,
            &case_id,
            &trace_id,
            &span_id,
            idx,
            cases.len(),
            code_version.as_deref(),
            "unset",
            started_at,
            started_at,
        )
        .await?;

        println!("[{}/{}] {case_id}: {rendered}", idx + 1, cases.len());
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&rendered)
            .env("TAEL_EVAL_SUITE_ID", suite)
            .env("TAEL_EVAL_RUN_ID", &run_id)
            .env("TAEL_EVAL_CASE_ID", &case_id)
            .env("TAEL_EVAL_CASE_INDEX", &case_index)
            .env("TAEL_EVAL_CASE_COUNT", &case_count)
            .env("TAEL_EVAL_TRACE_ID", &trace_id)
            .env("TAEL_EVAL_SPAN_ID", &span_id)
            .env("OTEL_EXPORTER_OTLP_ENDPOINT", server);
        if let Some(version) = &code_version {
            cmd.env("TAEL_EVAL_CODE_VERSION", version);
        }

        let status = cmd
            .status()
            .await
            .with_context(|| format!("running case {case_id}"))?;
        let finished_at = chrono::Utc::now();
        write_runner_span(
            client,
            suite,
            &run_id,
            &case_id,
            &trace_id,
            &span_id,
            idx,
            cases.len(),
            code_version.as_deref(),
            if status.success() { "ok" } else { "error" },
            started_at,
            finished_at,
        )
        .await?;
        if !status.success() {
            anyhow::bail!("case {case_id} exited with {status}");
        }
    }

    println!("eval run {run_id}: complete");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_runner_span(
    client: &TaelClient,
    suite: &str,
    run_id: &str,
    case_id: &str,
    trace_id: &str,
    span_id: &str,
    case_index: usize,
    case_count: usize,
    code_version: Option<&str>,
    status: &str,
    start_time: chrono::DateTime<chrono::Utc>,
    end_time: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let mut payload = serde_json::json!({
        "suite_id": suite,
        "run_id": run_id,
        "case_id": case_id,
        "trace_id": trace_id,
        "span_id": span_id,
        "case_index": case_index,
        "case_count": case_count,
        "status": status,
        "start_time": start_time.to_rfc3339(),
        "end_time": end_time.to_rfc3339(),
        "duration_ms": end_time
            .signed_duration_since(start_time)
            .num_microseconds()
            .map(|us| us as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0),
    });
    if let Some(version) = code_version {
        payload["code_version"] = Value::String(version.to_string());
    }
    client.add_eval_runner_span(&payload).await?;
    Ok(())
}

fn new_trace_span_ids() -> (String, String) {
    let trace_id = uuid::Uuid::new_v4().simple().to_string();
    let span_id = uuid::Uuid::new_v4().simple().to_string()[..16].to_string();
    (trace_id, span_id)
}

/// Scan a trace's spans for Chidori run correlation attributes. Returns
/// `(chidori.run_id, chidori.checkpoint_path?)` when the trace was emitted by
/// a Chidori agent run, None otherwise (including when the trace is missing —
/// promotion still works, it just records no replayable fixture).
async fn chidori_run_ref(client: &TaelClient, trace_id: &str) -> Option<(String, Option<String>)> {
    let trace = client.get_trace(trace_id).await.ok()?;
    let spans = trace.get("spans")?.as_array()?;
    let mut run_id = None;
    let mut checkpoint = None;
    for span in spans {
        let Some(attrs) = span.get("attributes").and_then(|v| v.as_object()) else {
            continue;
        };
        if run_id.is_none() {
            if let Some(v) = attrs.get("chidori.run_id").and_then(|v| v.as_str()) {
                run_id = Some(v.to_string());
            }
        }
        if checkpoint.is_none() {
            if let Some(v) = attrs.get("chidori.checkpoint_path").and_then(|v| v.as_str()) {
                checkpoint = Some(v.to_string());
            }
        }
        if run_id.is_some() && checkpoint.is_some() {
            break;
        }
    }
    run_id.map(|id| (id, checkpoint))
}

async fn find_eval_case_trace(
    client: &TaelClient,
    case_id: &str,
    limit: u32,
) -> Result<Option<String>> {
    Ok(comment_rows(client, limit)
        .await?
        .iter()
        .filter_map(enriched_comment)
        .find(|v| {
            kind(v) == Some("eval_case")
                && v.get("case_id").and_then(|x| x.as_str()) == Some(case_id)
        })
        .and_then(|v| {
            v.get("source_trace_id")
                .or_else(|| v.get("trace_id"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        }))
}

fn print_eval_case_add(value: &Value) {
    if let Some(comment) = value.get("comment") {
        let body: Value = comment["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        println!(
            "Eval case {}/{} added from trace {}",
            field(&body, "suite"),
            field(&body, "case_id"),
            field(&body, "source_trace_id")
        );
        if let Some(run_id) = body.get("chidori_run_id").and_then(|v| v.as_str()) {
            println!(
                "Fixture: chidori run {run_id} — replay with `chidori resume <agent.ts> {run_id} --ci` ($0)"
            );
        }
    }
}

fn print_eval_case_link(value: &Value) {
    if let Some(comment) = value.get("comment") {
        let body: Value = comment["body"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        println!(
            "Eval case {} linked to issue {}",
            field(&body, "case_id"),
            field(&body, "source_issue_id")
        );
    }
}

fn print_eval_suite_inspect(value: &Value) {
    println!("Eval suite {}", value["suite"].as_str().unwrap_or("-"));
    let mut summary = Table::new();
    summary.set_header(vec!["Metric", "Value"]);
    summary.add_row(vec![
        Cell::new("cases"),
        Cell::new(value["case_count"].as_u64().unwrap_or(0).to_string()),
    ]);
    summary.add_row(vec![
        Cell::new("critical_path"),
        Cell::new(
            value["critical_path_count"]
                .as_u64()
                .unwrap_or(0)
                .to_string(),
        ),
    ]);
    summary.add_row(vec![
        Cell::new("missing_expected_behavior"),
        Cell::new(
            value["missing_expected_behavior"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0)
                .to_string(),
        ),
    ]);
    summary.add_row(vec![
        Cell::new("provenance_free"),
        Cell::new(
            value["provenance_free"]
                .as_array()
                .map(|v| v.len())
                .unwrap_or(0)
                .to_string(),
        ),
    ]);
    println!("{summary}");

    if let Some(cases) = value["cases"].as_array().filter(|v| !v.is_empty()) {
        println!();
        let mut table = Table::new();
        table.set_header(vec!["Case", "Mode", "Critical", "Trace", "Expected"]);
        for case in cases {
            table.add_row(vec![
                Cell::new(field(case, "case_id")),
                Cell::new(field(case, "failure_mode")),
                Cell::new(
                    case.get("critical_path")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                        .to_string(),
                ),
                Cell::new(short_trace(field(case, "source_trace_id"))),
                Cell::new(field(case, "expected_behavior")),
            ]);
        }
        println!("{table}");
    }
}
