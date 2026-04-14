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

pub fn print_summary(value: &Value) {
    let window = value["window_seconds"].as_i64().unwrap_or(0);
    let svc_filter = value["service_filter"].as_str().unwrap_or("(all)");
    println!("Summary — last {window}s, service: {svc_filter}");
    println!();

    let traces = &value["traces"];
    let mut t = Table::new();
    t.set_header(vec!["Metric", "Value"]);
    t.add_row(vec![Cell::new("Spans"), Cell::new(traces["span_count"].as_i64().unwrap_or(0).to_string())]);
    t.add_row(vec![Cell::new("Traces"), Cell::new(traces["trace_count"].as_i64().unwrap_or(0).to_string())]);
    t.add_row(vec![Cell::new("Errors"), Cell::new(traces["error_count"].as_i64().unwrap_or(0).to_string())]);
    t.add_row(vec![
        Cell::new("Error rate"),
        Cell::new(format!("{:.2}%", traces["error_rate"].as_f64().unwrap_or(0.0) * 100.0)),
    ]);
    t.add_row(vec![Cell::new("Avg duration"), Cell::new(format!("{:.1} ms", traces["avg_ms"].as_f64().unwrap_or(0.0)))]);
    t.add_row(vec![Cell::new("p50"), Cell::new(format!("{:.1} ms", traces["p50_ms"].as_f64().unwrap_or(0.0)))]);
    t.add_row(vec![Cell::new("p95"), Cell::new(format!("{:.1} ms", traces["p95_ms"].as_f64().unwrap_or(0.0)))]);
    t.add_row(vec![Cell::new("p99"), Cell::new(format!("{:.1} ms", traces["p99_ms"].as_f64().unwrap_or(0.0)))]);
    t.add_row(vec![Cell::new("Max"), Cell::new(format!("{:.1} ms", traces["max_ms"].as_f64().unwrap_or(0.0)))]);
    println!("Traces");
    println!("{t}");
    println!();

    if let Some(svcs) = value["top_services"].as_array() {
        if !svcs.is_empty() {
            let mut st = Table::new();
            st.set_header(vec!["Service", "Spans", "Error Rate", "p95 (ms)"]);
            for s in svcs {
                st.add_row(vec![
                    Cell::new(s["service"].as_str().unwrap_or("-")),
                    Cell::new(s["span_count"].as_i64().unwrap_or(0).to_string()),
                    Cell::new(format!("{:.2}%", s["error_rate"].as_f64().unwrap_or(0.0) * 100.0)),
                    Cell::new(format!("{:.1}", s["p95_ms"].as_f64().unwrap_or(0.0))),
                ]);
            }
            println!("Top services");
            println!("{st}");
            println!();
        }
    }

    if let Some(ops) = value["top_error_operations"].as_array() {
        if !ops.is_empty() {
            let mut et = Table::new();
            et.set_header(vec!["Service", "Operation", "Errors"]);
            for o in ops {
                et.add_row(vec![
                    Cell::new(o["service"].as_str().unwrap_or("-")),
                    Cell::new(o["operation"].as_str().unwrap_or("-")),
                    Cell::new(o["error_count"].as_i64().unwrap_or(0).to_string()),
                ]);
            }
            println!("Top error operations");
            println!("{et}");
            println!();
        }
    }

    let logs = &value["logs"];
    let mut lt = Table::new();
    lt.set_header(vec!["Severity", "Count"]);
    lt.add_row(vec![Cell::new("total"), Cell::new(logs["total"].as_i64().unwrap_or(0).to_string())]);
    lt.add_row(vec![Cell::new("error"), Cell::new(logs["error"].as_i64().unwrap_or(0).to_string())]);
    lt.add_row(vec![Cell::new("warn"), Cell::new(logs["warn"].as_i64().unwrap_or(0).to_string())]);
    lt.add_row(vec![Cell::new("info"), Cell::new(logs["info"].as_i64().unwrap_or(0).to_string())]);
    lt.add_row(vec![Cell::new("debug"), Cell::new(logs["debug"].as_i64().unwrap_or(0).to_string())]);
    println!("Logs");
    println!("{lt}");
    println!();

    let metrics = &value["metrics"];
    let mut mt = Table::new();
    mt.set_header(vec!["Metric", "Value"]);
    mt.add_row(vec![Cell::new("Points"), Cell::new(metrics["point_count"].as_i64().unwrap_or(0).to_string())]);
    mt.add_row(vec![Cell::new("Unique names"), Cell::new(metrics["unique_names"].as_i64().unwrap_or(0).to_string())]);
    println!("Metrics");
    println!("{mt}");
}

pub fn print_anomalies(value: &Value) {
    let cur = value["current_seconds"].as_i64().unwrap_or(0);
    let base = value["baseline_seconds"].as_i64().unwrap_or(0);
    let svc = value["service_filter"].as_str().unwrap_or("(all)");
    println!("Anomalies — current {cur}s vs baseline {base}s, service: {svc}");
    println!();

    let anomalies = match value["anomalies"].as_array() {
        Some(a) if !a.is_empty() => a,
        _ => {
            println!("No anomalies detected.");
            return;
        }
    };

    let mut t = Table::new();
    t.set_header(vec!["Severity", "Service", "Kind", "Current", "Baseline", "Description"]);
    for a in anomalies {
        t.add_row(vec![
            Cell::new(a["severity"].as_str().unwrap_or("-")),
            Cell::new(a["service"].as_str().unwrap_or("-")),
            Cell::new(a["kind"].as_str().unwrap_or("-")),
            Cell::new(format!("{:.3}", a["current"].as_f64().unwrap_or(0.0))),
            Cell::new(format!("{:.3}", a["baseline"].as_f64().unwrap_or(0.0))),
            Cell::new(a["description"].as_str().unwrap_or("-")),
        ]);
    }
    println!("{t}");
}

pub fn print_correlate(value: &Value) {
    if let Some(err) = value.get("error").and_then(|e| e.as_str()) {
        println!("{err}");
        return;
    }
    let trace_id = value["trace_id"].as_str().unwrap_or("-");
    let span_count = value["span_count"].as_i64().unwrap_or(0);
    let dur = value["duration_ms"].as_f64().unwrap_or(0.0);
    let errors = value["error_count"].as_i64().unwrap_or(0);
    let services: Vec<&str> = value["services"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    println!("Trace {trace_id}");
    println!("  spans: {span_count}  errors: {errors}  duration: {dur:.1} ms");
    println!("  services: {}", services.join(", "));
    println!(
        "  window: {} → {}",
        value["start_time"].as_str().unwrap_or("-"),
        value["end_time"].as_str().unwrap_or("-")
    );
    println!();

    let logs = value["logs"].as_array().cloned().unwrap_or_default();
    if logs.is_empty() {
        println!("Logs: (none)");
    } else {
        let mut lt = Table::new();
        lt.set_header(vec!["Timestamp", "Service", "Severity", "Body"]);
        for l in logs.iter().take(50) {
            let body = l["body"].as_str().unwrap_or("-");
            let short = if body.len() > 80 { &body[..80] } else { body };
            lt.add_row(vec![
                Cell::new(l["timestamp"].as_str().unwrap_or("-")),
                Cell::new(l["service"].as_str().unwrap_or("-")),
                Cell::new(l["severity"].as_str().unwrap_or("-")),
                Cell::new(short),
            ]);
        }
        println!("Logs ({}):", logs.len());
        println!("{lt}");
    }
    println!();

    let metrics = value["metrics"].as_array().cloned().unwrap_or_default();
    if metrics.is_empty() {
        println!("Metrics: (none in trace window)");
    } else {
        let mut mt = Table::new();
        mt.set_header(vec!["Timestamp", "Service", "Name", "Type", "Value"]);
        for m in metrics.iter().take(50) {
            mt.add_row(vec![
                Cell::new(m["timestamp"].as_str().unwrap_or("-")),
                Cell::new(m["service"].as_str().unwrap_or("-")),
                Cell::new(m["name"].as_str().unwrap_or("-")),
                Cell::new(m["metric_type"].as_str().unwrap_or("-")),
                Cell::new(format!("{:.3}", m["value"].as_f64().unwrap_or(0.0))),
            ]);
        }
        println!("Metrics ({}):", metrics.len());
        println!("{mt}");
    }
}

pub fn print_watch_tick(delta: &Value) {
    let ts = delta["timestamp"].as_str().unwrap_or("-");
    let tr = &delta["traces"];
    let lg = &delta["logs"];
    let mt = &delta["metrics"];
    let fmt_signed_i = |v: i64| {
        if v >= 0 {
            format!("+{v}")
        } else {
            v.to_string()
        }
    };
    let fmt_signed_f = |v: f64| {
        if v >= 0.0 {
            format!("+{v:.2}")
        } else {
            format!("{v:.2}")
        }
    };
    println!(
        "[{ts}] spans={} ({}) errors={} ({}) err_rate={:.2}% ({}) p95={:.1}ms ({}) logs.err={} ({}) metrics={} ({})",
        tr["span_count"].as_i64().unwrap_or(0),
        fmt_signed_i(tr["delta_span_count"].as_i64().unwrap_or(0)),
        tr["error_count"].as_i64().unwrap_or(0),
        fmt_signed_i(tr["delta_error_count"].as_i64().unwrap_or(0)),
        tr["error_rate"].as_f64().unwrap_or(0.0) * 100.0,
        fmt_signed_f(tr["delta_error_rate"].as_f64().unwrap_or(0.0) * 100.0),
        tr["p95_ms"].as_f64().unwrap_or(0.0),
        fmt_signed_f(tr["delta_p95_ms"].as_f64().unwrap_or(0.0)),
        lg["error"].as_i64().unwrap_or(0),
        fmt_signed_i(lg["delta_error"].as_i64().unwrap_or(0)),
        mt["point_count"].as_i64().unwrap_or(0),
        fmt_signed_i(mt["delta_point_count"].as_i64().unwrap_or(0)),
    );
}

pub fn render(format: &OutputFormat, value: &Value, table_fn: fn(&Value)) {
    match format {
        OutputFormat::Json => print_json(value),
        OutputFormat::Table => table_fn(value),
    }
}
