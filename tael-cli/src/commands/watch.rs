use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::commands::condition::{Condition, Trip, lookup_field};
use crate::exit::{CategorizedError, ExitCategory};
use crate::output;

pub async fn run(
    client: &TaelClient,
    format: &OutputFormat,
    last: Option<String>,
    service: Option<String>,
    interval: u64,
    exit_on: Vec<String>,
    max_ticks: Option<u64>,
) -> Result<()> {
    let window = last.as_deref().unwrap_or("1m");
    let conditions = exit_on
        .iter()
        .map(|c| Condition::parse(c))
        .collect::<Result<Vec<_>>>()
        .map_err(|e| CategorizedError::new(ExitCategory::BadQuery, e.to_string()))?;

    let mut prev: Option<Value> = None;
    // First observed value per field, for `>2x`-style relative thresholds.
    let mut baselines: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut ticks: u64 = 0;

    loop {
        let sample = client.summary(Some(window), service.as_deref()).await?;
        let delta = build_delta(prev.as_ref(), &sample);
        ticks += 1;

        for condition in &conditions {
            baselines
                .entry(condition.field.clone())
                .or_insert_with(|| lookup_field(&delta, &condition.field).unwrap_or(f64::NAN));
        }

        match format {
            OutputFormat::Json => println!("{}", serde_json::to_string(&delta)?),
            OutputFormat::Table => output::print_watch_tick(&delta),
        }

        // Conditions are checked against the tick just printed, so the output
        // always ends with the sample that tripped it.
        let tripped: Vec<Trip> = conditions
            .iter()
            .filter_map(|c| {
                let baseline = baselines.get(&c.field).copied().filter(|b| !b.is_nan());
                c.evaluate(&delta, baseline)
            })
            .collect();

        if !tripped.is_empty() {
            let verdict = json!({
                "verdict": "condition_met",
                "ticks": ticks,
                "tripped": tripped,
                "tick": delta,
            });
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string(&verdict)?),
                OutputFormat::Table => output::print_watch_verdict(&verdict),
            }
            // Exit code 6 lets a calling agent branch on "the thing I was
            // waiting for happened" without re-reading the stream.
            return Err(CategorizedError::new(ExitCategory::ConditionMet, String::new()).into());
        }

        if max_ticks.is_some_and(|max| ticks >= max) {
            let verdict = json!({
                "verdict": "max_ticks_reached",
                "ticks": ticks,
                "tripped": [],
                "tick": delta,
            });
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string(&verdict)?),
                OutputFormat::Table => output::print_watch_verdict(&verdict),
            }
            return Ok(());
        }

        prev = Some(sample);
        sleep(Duration::from_secs(interval)).await;
    }
}

fn build_delta(prev: Option<&Value>, current: &Value) -> Value {
    let cur_traces = &current["traces"];
    let cur_logs = &current["logs"];
    let cur_metrics = &current["metrics"];

    let delta_i64 = |field: &str, group: &str| -> i64 {
        let cur = current[group][field].as_i64().unwrap_or(0);
        let base = prev.and_then(|p| p[group][field].as_i64()).unwrap_or(cur);
        cur - base
    };
    let delta_f64 = |field: &str, group: &str| -> f64 {
        let cur = current[group][field].as_f64().unwrap_or(0.0);
        let base = prev.and_then(|p| p[group][field].as_f64()).unwrap_or(cur);
        cur - base
    };

    json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "window_seconds": current["window_seconds"],
        "traces": {
            "span_count": cur_traces["span_count"],
            "error_count": cur_traces["error_count"],
            "error_rate": cur_traces["error_rate"],
            "p95_ms": cur_traces["p95_ms"],
            "delta_span_count": delta_i64("span_count", "traces"),
            "delta_error_count": delta_i64("error_count", "traces"),
            "delta_error_rate": delta_f64("error_rate", "traces"),
            "delta_p95_ms": delta_f64("p95_ms", "traces"),
        },
        "logs": {
            "total": cur_logs["total"],
            "error": cur_logs["error"],
            "delta_total": delta_i64("total", "logs"),
            "delta_error": delta_i64("error", "logs"),
        },
        "metrics": {
            "point_count": cur_metrics["point_count"],
            "delta_point_count": delta_i64("point_count", "metrics"),
        }
    })
}
