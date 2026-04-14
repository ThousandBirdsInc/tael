use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

pub async fn run(
    client: &TaelClient,
    format: &OutputFormat,
    last: Option<String>,
    service: Option<String>,
    interval: u64,
) -> Result<()> {
    let window = last.as_deref().unwrap_or("1m");
    let mut prev: Option<Value> = None;

    loop {
        let sample = client.summary(Some(window), service.as_deref()).await?;
        let delta = build_delta(prev.as_ref(), &sample);

        match format {
            OutputFormat::Json => println!("{}", serde_json::to_string(&delta)?),
            OutputFormat::Table => output::print_watch_tick(&delta),
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
        let base = prev
            .and_then(|p| p[group][field].as_i64())
            .unwrap_or(cur);
        cur - base
    };
    let delta_f64 = |field: &str, group: &str| -> f64 {
        let cur = current[group][field].as_f64().unwrap_or(0.0);
        let base = prev
            .and_then(|p| p[group][field].as_f64())
            .unwrap_or(cur);
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
