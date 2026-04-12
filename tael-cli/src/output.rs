use comfy_table::{Cell, Table};
use serde_json::Value;

use crate::OutputFormat;

pub fn print_json(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

pub fn print_spans_table(value: &Value) {
    let spans = match value.get("spans").and_then(|s| s.as_array()) {
        Some(s) => s,
        None => {
            println!("No spans found.");
            return;
        }
    };

    if spans.is_empty() {
        println!("No spans found.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Trace ID", "Span ID", "Service", "Operation", "Duration (ms)", "Status"]);

    for span in spans {
        let trace_id = span["trace_id"].as_str().unwrap_or("-");
        let short_trace = if trace_id.len() > 12 {
            &trace_id[..12]
        } else {
            trace_id
        };
        table.add_row(vec![
            Cell::new(format!("{short_trace}…")),
            Cell::new(span["span_id"].as_str().unwrap_or("-")),
            Cell::new(span["service"].as_str().unwrap_or("-")),
            Cell::new(span["operation"].as_str().unwrap_or("-")),
            Cell::new(format!("{:.1}", span["duration_ms"].as_f64().unwrap_or(0.0))),
            Cell::new(span["status"].as_str().unwrap_or("-")),
        ]);
    }

    println!("{table}");
}

pub fn print_services_table(value: &Value) {
    let services = match value.get("services").and_then(|s| s.as_array()) {
        Some(s) => s,
        None => {
            println!("No services found.");
            return;
        }
    };

    if services.is_empty() {
        println!("No services found.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Service", "Spans", "Traces", "Avg Duration (ms)", "Error Rate"]);

    for svc in services {
        table.add_row(vec![
            Cell::new(svc["name"].as_str().unwrap_or("-")),
            Cell::new(svc["span_count"].as_i64().unwrap_or(0).to_string()),
            Cell::new(svc["trace_count"].as_i64().unwrap_or(0).to_string()),
            Cell::new(format!("{:.1}", svc["avg_duration_ms"].as_f64().unwrap_or(0.0))),
            Cell::new(format!("{:.2}%", svc["error_rate"].as_f64().unwrap_or(0.0) * 100.0)),
        ]);
    }

    println!("{table}");
}

pub fn print_logs_table(value: &Value) {
    let logs = match value.get("logs").and_then(|l| l.as_array()) {
        Some(l) => l,
        None => {
            println!("No logs found.");
            return;
        }
    };

    if logs.is_empty() {
        println!("No logs found.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Timestamp", "Service", "Severity", "Body", "Trace ID"]);

    for log in logs {
        let timestamp = log["timestamp"].as_str().unwrap_or("-");
        let short_ts = if timestamp.len() > 19 {
            &timestamp[..19]
        } else {
            timestamp
        };
        let body = log["body"].as_str().unwrap_or("-");
        let short_body = if body.len() > 80 {
            format!("{}…", &body[..80])
        } else {
            body.to_string()
        };
        let trace_id = log["trace_id"].as_str().unwrap_or("-");
        let short_trace = if trace_id.len() > 12 {
            format!("{}…", &trace_id[..12])
        } else {
            trace_id.to_string()
        };
        table.add_row(vec![
            Cell::new(short_ts),
            Cell::new(log["service"].as_str().unwrap_or("-")),
            Cell::new(log["severity"].as_str().unwrap_or("-")),
            Cell::new(short_body),
            Cell::new(short_trace),
        ]);
    }

    println!("{table}");
}

pub fn print_metrics_table(value: &Value) {
    let metrics = match value.get("metrics").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => {
            println!("No metrics found.");
            return;
        }
    };

    if metrics.is_empty() {
        println!("No metrics found.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Timestamp", "Service", "Name", "Type", "Value", "Unit"]);

    for m in metrics {
        let timestamp = m["timestamp"].as_str().unwrap_or("-");
        let short_ts = if timestamp.len() > 19 {
            &timestamp[..19]
        } else {
            timestamp
        };
        table.add_row(vec![
            Cell::new(short_ts),
            Cell::new(m["service"].as_str().unwrap_or("-")),
            Cell::new(m["name"].as_str().unwrap_or("-")),
            Cell::new(m["metric_type"].as_str().unwrap_or("-")),
            Cell::new(format!("{:.4}", m["value"].as_f64().unwrap_or(0.0))),
            Cell::new(m["unit"].as_str().unwrap_or("")),
        ]);
    }

    println!("{table}");
}

pub fn print_series_table(value: &Value) {
    let series = match value.get("series").and_then(|s| s.as_array()) {
        Some(s) => s,
        None => {
            println!("No series returned.");
            return;
        }
    };

    if series.is_empty() {
        println!("No series returned.");
        return;
    }

    let mut table = Table::new();
    table.set_header(vec!["Metric", "Labels", "Value", "Timestamp"]);

    for s in series {
        let labels = s
            .get("labels")
            .and_then(|l| l.as_object())
            .map(|m| {
                let mut entries: Vec<String> =
                    m.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect();
                entries.sort();
                entries.join(", ")
            })
            .unwrap_or_default();
        let timestamp = s["timestamp"].as_str().unwrap_or("-");
        let short_ts = if timestamp.len() > 19 {
            &timestamp[..19]
        } else {
            timestamp
        };
        table.add_row(vec![
            Cell::new(s["metric"].as_str().unwrap_or("-")),
            Cell::new(labels),
            Cell::new(format!("{:.4}", s["value"].as_f64().unwrap_or(0.0))),
            Cell::new(short_ts),
        ]);
    }

    println!("{table}");
}

pub fn render(format: &OutputFormat, value: &Value, table_fn: fn(&Value)) {
    match format {
        OutputFormat::Json => print_json(value),
        OutputFormat::Table => table_fn(value),
    }
}
