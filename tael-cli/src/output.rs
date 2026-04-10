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

pub fn render(format: &OutputFormat, value: &Value, table_fn: fn(&Value)) {
    match format {
        OutputFormat::Json => print_json(value),
        OutputFormat::Table => table_fn(value),
    }
}
