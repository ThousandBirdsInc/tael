use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::client::TaelClient;

struct Comment {
    author: String,
    body: String,
    created_at: String,
    span_id: Option<String>,
}

const MAX_LIVE_SPANS: usize = 200;

struct App {
    client: TaelClient,
    service_filter: Option<String>,
    status_filter: Option<String>,
    services_interval: Duration,
    spans: Vec<SpanRow>,
    services: Vec<ServiceRow>,
    table_state: TableState,
    tab: Tab,
    should_quit: bool,
    last_error: Option<String>,
    paused: bool,
    // SSE live stream
    sse_rx: mpsc::UnboundedReceiver<String>,
    // Live timeline (trace-level waterfall)
    live_trace_map: HashMap<String, LiveTraceRow>,
    live_traces_sorted: Vec<LiveTraceRow>,
    timeline_state: TableState,
    timeline_window_secs: f64,
    prev_tab: Tab,
    // Trace detail waterfall state
    trace_spans: Vec<SpanRow>,
    waterfall_rows: Vec<WaterfallRow>,
    waterfall_state: TableState,
    trace_loading: bool,
    // Comments
    comments: Vec<Comment>,
    comment_input: Option<String>,
    current_trace_id: Option<String>,
    // Interactive filter
    filter_input: Option<String>,
    filter_text: String,
    // Pinned attribute columns
    pinned_columns: Vec<String>,
    attr_picker: Option<AttrPicker>,
}

struct AttrPicker {
    keys: Vec<String>,
    state: TableState,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Traces,
    Services,
    Timeline,
    Detail,
}

const MAX_LIVE_TRACES: usize = 500;

#[derive(Clone)]
struct LiveTraceRow {
    trace_id: String,
    service: String,
    operation: String,
    start_time_ms: f64,
    end_time_ms: f64,
    duration_ms: f64,
    span_count: usize,
    has_error: bool,
}

#[derive(Clone)]
struct SpanRow {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    service: String,
    operation: String,
    duration_ms: f64,
    status: String,
    start_time: String,
    start_time_ms: f64,
    attributes: Value,
    events: Value,
}

#[derive(Clone)]
struct WaterfallRow {
    span_idx: usize,
    depth: usize,
    offset_pct: f64,
    width_pct: f64,
}

#[derive(Clone)]
struct ServiceRow {
    name: String,
    span_count: i64,
    trace_count: i64,
    avg_duration_ms: f64,
    error_rate: f64,
}

// Assign a stable color per service name
fn service_color(service: &str) -> Color {
    let hash: u32 = service.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let colors = [
        Color::Cyan,
        Color::Blue,
        Color::Magenta,
        Color::Yellow,
        Color::Green,
        Color::LightCyan,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightYellow,
        Color::LightGreen,
    ];
    colors[(hash as usize) % colors.len()]
}

impl App {
    fn new(
        server: &str,
        service: Option<String>,
        status: Option<String>,
        interval: u64,
    ) -> Self {
        let client = TaelClient::new(server);
        let sse_rx = client.subscribe_live(service.clone(), status.clone());
        Self {
            client,
            service_filter: service,
            status_filter: status,
            services_interval: Duration::from_secs(interval.max(5)),
            spans: Vec::new(),
            services: Vec::new(),
            table_state: TableState::default(),
            tab: Tab::Traces,
            should_quit: false,
            last_error: None,
            paused: false,
            sse_rx,
            live_trace_map: HashMap::new(),
            live_traces_sorted: Vec::new(),
            timeline_state: TableState::default(),
            timeline_window_secs: 60.0,
            prev_tab: Tab::Traces,
            trace_spans: Vec::new(),
            waterfall_rows: Vec::new(),
            waterfall_state: TableState::default(),
            trace_loading: false,
            comments: Vec::new(),
            comment_input: None,
            current_trace_id: None,
            filter_input: None,
            filter_text: String::new(),
            pinned_columns: Vec::new(),
            attr_picker: None,
        }
    }

    fn open_attr_picker(&mut self) {
        let span = self
            .table_state
            .selected()
            .and_then(|i| self.filtered_spans().into_iter().nth(i));
        let span = match span {
            Some(s) => s.clone(),
            None => return,
        };

        // Collect all attribute keys across all spans for a complete list,
        // but start with the selected span's keys on top
        let mut seen = std::collections::HashSet::new();
        let mut keys = Vec::new();

        // Selected span's attrs first
        if let Some(obj) = span.attributes.as_object() {
            for k in obj.keys() {
                if seen.insert(k.clone()) {
                    keys.push(k.clone());
                }
            }
        }

        // Then all other spans
        for s in &self.spans {
            if let Some(obj) = s.attributes.as_object() {
                for k in obj.keys() {
                    if seen.insert(k.clone()) {
                        keys.push(k.clone());
                    }
                }
            }
        }

        let mut state = TableState::default();
        if !keys.is_empty() {
            state.select(Some(0));
        }
        self.attr_picker = Some(AttrPicker { keys, state });
    }

    fn filtered_spans(&self) -> Vec<&SpanRow> {
        if self.filter_text.is_empty() {
            return self.spans.iter().collect();
        }
        let q = self.filter_text.to_lowercase();
        self.spans
            .iter()
            .filter(|s| {
                s.service.to_lowercase().contains(&q)
                    || s.operation.to_lowercase().contains(&q)
                    || s.trace_id.to_lowercase().contains(&q)
                    || s.status.to_lowercase().contains(&q)
            })
            .collect()
    }

    fn filtered_live_traces(&self) -> Vec<(usize, &LiveTraceRow)> {
        if self.filter_text.is_empty() {
            return self.live_traces_sorted.iter().enumerate().collect();
        }
        let q = self.filter_text.to_lowercase();
        self.live_traces_sorted
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.service.to_lowercase().contains(&q)
                    || t.operation.to_lowercase().contains(&q)
                    || t.trace_id.to_lowercase().contains(&q)
                    || (t.has_error && "error".contains(&q))
                    || (!t.has_error && "ok".contains(&q))
            })
            .collect()
    }

    fn ingest_sse(&mut self, json: &str) {
        if self.paused {
            return;
        }

        let val: Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return,
        };

        // SSE data is a JSON array of spans
        let new_spans = if val.is_array() {
            parse_spans(&serde_json::json!({ "spans": val }))
        } else {
            parse_spans(&val)
        };

        if new_spans.is_empty() {
            return;
        }

        // Update live trace map
        self.update_live_traces(&new_spans);

        // Prepend new spans (newest first)
        let mut merged = new_spans;
        merged.append(&mut self.spans);
        merged.truncate(MAX_LIVE_SPANS);
        self.spans = merged;
        self.last_error = None;
    }

    fn update_live_traces(&mut self, spans: &[SpanRow]) {
        for span in spans {
            let end_ms = span.start_time_ms + span.duration_ms;
            let entry = self
                .live_trace_map
                .entry(span.trace_id.clone())
                .or_insert_with(|| LiveTraceRow {
                    trace_id: span.trace_id.clone(),
                    service: span.service.clone(),
                    operation: span.operation.clone(),
                    start_time_ms: span.start_time_ms,
                    end_time_ms: end_ms,
                    duration_ms: span.duration_ms,
                    span_count: 0,
                    has_error: false,
                });

            entry.span_count += 1;
            if span.start_time_ms < entry.start_time_ms {
                entry.start_time_ms = span.start_time_ms;
            }
            if end_ms > entry.end_time_ms {
                entry.end_time_ms = end_ms;
            }
            entry.duration_ms = entry.end_time_ms - entry.start_time_ms;
            if span.status == "error" {
                entry.has_error = true;
            }
            // Prefer root span's service/operation
            if span.parent_span_id.is_none() {
                entry.service = span.service.clone();
                entry.operation = span.operation.clone();
            }
        }

        self.rebuild_live_traces();
    }

    fn rebuild_live_traces(&mut self) {
        let mut traces: Vec<LiveTraceRow> = self.live_trace_map.values().cloned().collect();
        traces.sort_by(|a, b| {
            a.start_time_ms
                .partial_cmp(&b.start_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Evict oldest traces if over limit
        if traces.len() > MAX_LIVE_TRACES {
            let to_remove: Vec<String> = traces[..traces.len() - MAX_LIVE_TRACES]
                .iter()
                .map(|t| t.trace_id.clone())
                .collect();
            for id in &to_remove {
                self.live_trace_map.remove(id);
            }
            traces = traces[traces.len() - MAX_LIVE_TRACES..].to_vec();
        }

        self.live_traces_sorted = traces;
    }



    async fn poll_initial(&mut self) {
        match self
            .client
            .query_traces(
                self.service_filter.as_deref(),
                None,
                None,
                None,
                self.status_filter.as_deref(),
                Some("1h"),
                200,
            )
            .await
        {
            Ok(val) => {
                self.spans = parse_spans(&val);
                self.update_live_traces(&self.spans.clone());
            }
            Err(e) => {
                self.last_error = Some(format!("initial load: {e}"));
            }
        }

        self.poll_services().await;
    }

    async fn poll_services(&mut self) {
        match self.client.list_services().await {
            Ok(val) => {
                self.services = parse_services(&val);
            }
            Err(e) => {
                self.last_error = Some(format!("services: {e}"));
            }
        }
    }

    async fn load_trace(&mut self, trace_id: &str) {
        self.trace_loading = true;
        self.current_trace_id = Some(trace_id.to_string());
        match self.client.get_trace(trace_id).await {
            Ok(val) => {
                self.trace_spans = parse_spans(&val);
                self.waterfall_rows = build_waterfall(&self.trace_spans);
                self.waterfall_state = TableState::default();
                if !self.waterfall_rows.is_empty() {
                    self.waterfall_state.select(Some(0));
                }
                self.trace_loading = false;
            }
            Err(e) => {
                self.last_error = Some(format!("get_trace: {e}"));
                self.trace_loading = false;
            }
        }
        self.load_comments(trace_id).await;
    }

    async fn load_comments(&mut self, trace_id: &str) {
        match self.client.get_comments(trace_id).await {
            Ok(val) => {
                self.comments = parse_comments(&val);
            }
            Err(e) => {
                self.last_error = Some(format!("comments: {e}"));
            }
        }
    }

    async fn submit_comment(&mut self) {
        let body = match self.comment_input.take() {
            Some(b) if !b.trim().is_empty() => b,
            _ => return,
        };
        let trace_id = match &self.current_trace_id {
            Some(id) => id.clone(),
            None => return,
        };
        let span_id = self.selected_waterfall_span().map(|s| s.span_id.clone());
        match self
            .client
            .add_comment(&trace_id, &body, Some("tui"), span_id.as_deref())
            .await
        {
            Ok(_) => {
                self.load_comments(&trace_id).await;
            }
            Err(e) => {
                self.last_error = Some(format!("add_comment: {e}"));
            }
        }
    }

    fn selected_waterfall_span(&self) -> Option<&SpanRow> {
        self.waterfall_state
            .selected()
            .and_then(|i| self.waterfall_rows.get(i))
            .map(|wr| &self.trace_spans[wr.span_idx])
    }

    /// Returns action: None = normal, Some("load_trace") = load trace, Some("submit_comment") = submit
    fn handle_key(&mut self, code: KeyCode) -> Option<&'static str> {
        // Attribute picker mode
        if let Some(ref mut picker) = self.attr_picker {
            match code {
                KeyCode::Esc | KeyCode::Char('a') => {
                    self.attr_picker = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let len = picker.keys.len();
                    if len > 0 {
                        let i = picker.state.selected().map(|i| i + 1).unwrap_or(0);
                        picker.state.select(Some(i.min(len - 1)));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = picker
                        .state
                        .selected()
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    picker.state.select(Some(i));
                }
                KeyCode::Enter => {
                    if let Some(idx) = picker.state.selected() {
                        if let Some(key) = picker.keys.get(idx).cloned() {
                            if let Some(pos) = self.pinned_columns.iter().position(|k| *k == key) {
                                self.pinned_columns.remove(pos);
                            } else {
                                self.pinned_columns.push(key);
                            }
                        }
                    }
                }
                _ => {}
            }
            return None;
        }

        // Filter input mode
        if self.filter_input.is_some() {
            match code {
                KeyCode::Enter => {
                    if let Some(input) = self.filter_input.take() {
                        self.filter_text = input;
                        self.table_state.select(None);
                        self.timeline_state.select(None);
                    }
                }
                KeyCode::Esc => {
                    self.filter_input = None;
                }
                KeyCode::Backspace => {
                    if let Some(ref mut input) = self.filter_input {
                        input.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(ref mut input) = self.filter_input {
                        input.push(c);
                    }
                }
                _ => {}
            }
            return None;
        }

        // Comment input mode
        if self.comment_input.is_some() {
            match code {
                KeyCode::Enter => return Some("submit_comment"),
                KeyCode::Esc => {
                    self.comment_input = None;
                }
                KeyCode::Backspace => {
                    if let Some(ref mut input) = self.comment_input {
                        input.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(ref mut input) = self.comment_input {
                        input.push(c);
                    }
                }
                _ => {}
            }
            return None;
        }

        // Normal mode
        match code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return None;
            }
            KeyCode::Esc => {
                if self.tab == Tab::Detail {
                    self.tab = self.prev_tab;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('1') => {
                self.tab = Tab::Traces;
                self.table_state.select(None);
            }
            KeyCode::Char('2') => {
                self.tab = Tab::Services;
                self.table_state.select(None);
            }
            KeyCode::Char('3') => {
                self.tab = Tab::Timeline;
                self.timeline_state.select(None);
            }
            KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('/') => {
                if self.tab == Tab::Traces || self.tab == Tab::Timeline {
                    self.filter_input = Some(self.filter_text.clone());
                }
            }
            KeyCode::Char('\\') => {
                // Clear filter
                self.filter_text.clear();
                self.table_state.select(None);
                self.timeline_state.select(None);
            }
            KeyCode::Char('a') => {
                if self.tab == Tab::Traces && self.table_state.selected().is_some() {
                    self.open_attr_picker();
                }
            }
            KeyCode::Char('c') => {
                if self.tab == Tab::Detail {
                    self.comment_input = Some(String::new());
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                if self.tab == Tab::Timeline {
                    self.timeline_window_secs = (self.timeline_window_secs / 2.0).max(5.0);
                }
            }
            KeyCode::Char('-') => {
                if self.tab == Tab::Timeline {
                    self.timeline_window_secs = (self.timeline_window_secs * 2.0).min(3600.0);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.tab == Tab::Detail {
                    let len = self.waterfall_rows.len();
                    if len > 0 {
                        let i = self.waterfall_state.selected().map(|i| i + 1).unwrap_or(0);
                        self.waterfall_state.select(Some(i.min(len - 1)));
                    }
                } else if self.tab == Tab::Timeline {
                    let len = self.filtered_live_traces().len();
                    if len > 0 {
                        let i = self.timeline_state.selected().map(|i| i + 1).unwrap_or(0);
                        self.timeline_state.select(Some(i.min(len - 1)));
                    }
                } else if self.tab == Tab::Traces {
                    let len = self.filtered_spans().len();
                    if len > 0 {
                        let i = self.table_state.selected().map(|i| i + 1).unwrap_or(0);
                        self.table_state.select(Some(i.min(len - 1)));
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.tab == Tab::Detail {
                    let i = self
                        .waterfall_state
                        .selected()
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    self.waterfall_state.select(Some(i));
                } else if self.tab == Tab::Timeline {
                    let i = self
                        .timeline_state
                        .selected()
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    self.timeline_state.select(Some(i));
                } else if self.tab == Tab::Traces {
                    let i = self
                        .table_state
                        .selected()
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    self.table_state.select(Some(i));
                }
            }
            KeyCode::Enter => {
                if self.tab == Tab::Traces && self.table_state.selected().is_some() {
                    return Some("load_trace");
                }
                if self.tab == Tab::Timeline && self.timeline_state.selected().is_some() {
                    return Some("load_trace");
                }
            }
            KeyCode::Backspace => {
                if self.tab == Tab::Detail {
                    self.tab = self.prev_tab;
                }
            }
            _ => {}
        }
        None
    }
}

fn parse_time_ms(s: &str) -> f64 {
    // Parse ISO timestamp to epoch ms for relative positioning
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f"))
        .map(|dt| dt.and_utc().timestamp_millis() as f64)
        .unwrap_or(0.0)
}

fn parse_spans(val: &Value) -> Vec<SpanRow> {
    val.get("spans")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    let start_time = s["start_time"].as_str().unwrap_or("-").to_string();
                    let start_time_ms = parse_time_ms(&start_time);
                    SpanRow {
                        trace_id: s["trace_id"].as_str().unwrap_or("-").to_string(),
                        span_id: s["span_id"].as_str().unwrap_or("-").to_string(),
                        parent_span_id: s["parent_span_id"].as_str().map(|s| s.to_string()),
                        service: s["service"].as_str().unwrap_or("-").to_string(),
                        operation: s["operation"].as_str().unwrap_or("-").to_string(),
                        duration_ms: s["duration_ms"].as_f64().unwrap_or(0.0),
                        status: s["status"].as_str().unwrap_or("-").to_string(),
                        start_time,
                        start_time_ms,
                        attributes: s["attributes"].clone(),
                        events: s["events"].clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_services(val: &Value) -> Vec<ServiceRow> {
    val.get("services")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| ServiceRow {
                    name: s["name"].as_str().unwrap_or("-").to_string(),
                    span_count: s["span_count"].as_i64().unwrap_or(0),
                    trace_count: s["trace_count"].as_i64().unwrap_or(0),
                    avg_duration_ms: s["avg_duration_ms"].as_f64().unwrap_or(0.0),
                    error_rate: s["error_rate"].as_f64().unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_comments(val: &Value) -> Vec<Comment> {
    val.get("comments")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| Comment {
                    author: c["author"].as_str().unwrap_or("-").to_string(),
                    body: c["body"].as_str().unwrap_or("").to_string(),
                    created_at: c["created_at"].as_str().unwrap_or("-").to_string(),
                    span_id: c["span_id"].as_str().map(|s| s.to_string()),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn build_waterfall(spans: &[SpanRow]) -> Vec<WaterfallRow> {
    if spans.is_empty() {
        return Vec::new();
    }

    // Find trace time range
    let trace_start = spans
        .iter()
        .map(|s| s.start_time_ms)
        .fold(f64::INFINITY, f64::min);
    let trace_end = spans
        .iter()
        .map(|s| s.start_time_ms + s.duration_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let trace_duration = (trace_end - trace_start).max(1.0);

    // Build parent->children index
    let mut children: std::collections::HashMap<Option<String>, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, span) in spans.iter().enumerate() {
        children
            .entry(span.parent_span_id.clone())
            .or_default()
            .push(i);
    }

    // DFS to build ordered rows with depth
    let mut rows = Vec::new();
    let mut stack: Vec<(Option<String>, usize)> = vec![(None, 0)];

    while let Some((parent_id, depth)) = stack.pop() {
        if let Some(child_indices) = children.get(&parent_id) {
            // Reverse so we process in original order (stack is LIFO)
            for &idx in child_indices.iter().rev() {
                let span = &spans[idx];
                let offset_pct = ((span.start_time_ms - trace_start) / trace_duration).clamp(0.0, 1.0);
                let width_pct = (span.duration_ms / trace_duration).clamp(0.005, (1.0 - offset_pct).max(0.005));

                rows.push(WaterfallRow {
                    span_idx: idx,
                    depth,
                    offset_pct,
                    width_pct,
                });
                stack.push((Some(span.span_id.clone()), depth + 1));
            }
        }
    }

    // If tree walk missed any spans (e.g. orphans), append them
    let visited: std::collections::HashSet<usize> = rows.iter().map(|r| r.span_idx).collect();
    for (i, span) in spans.iter().enumerate() {
        if !visited.contains(&i) {
            let offset_pct = ((span.start_time_ms - trace_start) / trace_duration).clamp(0.0, 1.0);
            let width_pct = (span.duration_ms / trace_duration).clamp(0.005, (1.0 - offset_pct).max(0.005));
            rows.push(WaterfallRow {
                span_idx: i,
                depth: 0,
                offset_pct,
                width_pct,
            });
        }
    }

    rows
}

fn status_color(status: &str) -> Color {
    match status {
        "error" => Color::Red,
        "ok" => Color::Green,
        _ => Color::DarkGray,
    }
}

fn duration_color(ms: f64) -> Color {
    if ms >= 500.0 {
        Color::Red
    } else if ms >= 100.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn render_waterfall_bar(offset_pct: f64, width_pct: f64, bar_width: u16, color: Color, status: &str) -> Line<'static> {
    let total = bar_width as usize;
    if total == 0 {
        return Line::raw("");
    }

    let start = (offset_pct * total as f64).round() as usize;
    let width = (width_pct * total as f64).round().max(1.0) as usize;
    let start = start.min(total);
    let width = width.min(total - start);
    let end = total - start - width;

    let bar_char = if status == "error" { "▓" } else { "█" };

    Line::from(vec![
        Span::raw(" ".repeat(start)),
        Span::styled(bar_char.repeat(width), Style::default().fg(color)),
        Span::raw(" ".repeat(end)),
    ])
}

fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_header(frame, chunks[0], app);

    match app.tab {
        Tab::Traces => {
            let has_selection = app.table_state.selected().is_some();
            if has_selection {
                let splits = Layout::vertical([
                    Constraint::Min(8),
                    Constraint::Length(10),
                ])
                .split(chunks[1]);
                draw_traces(frame, splits[0], app);
                draw_selected_span_properties(frame, splits[1], app);
            } else {
                draw_traces(frame, chunks[1], app);
            }
            if app.attr_picker.is_some() {
                draw_attr_picker(frame, chunks[1], app);
            }
        }
        Tab::Services => draw_services(frame, chunks[1], app),
        Tab::Timeline => {
            let has_selection = app.timeline_state.selected().is_some();
            if has_selection {
                let splits = Layout::vertical([
                    Constraint::Min(8),
                    Constraint::Length(10),
                ])
                .split(chunks[1]);
                draw_timeline(frame, splits[0], app);
                draw_selected_timeline_properties(frame, splits[1], app);
            } else {
                draw_timeline(frame, chunks[1], app);
            }
        }
        Tab::Detail => draw_trace_detail(frame, chunks[1], app),
    }

    draw_footer(frame, chunks[2], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let tabs: Vec<Span> = vec![
        if app.tab == Tab::Traces {
            Span::styled(" 1:Traces ", Style::default().fg(Color::Black).bg(Color::Cyan))
        } else {
            Span::styled(" 1:Traces ", Style::default().fg(Color::DarkGray))
        },
        Span::raw("  "),
        if app.tab == Tab::Services {
            Span::styled(" 2:Services ", Style::default().fg(Color::Black).bg(Color::Cyan))
        } else {
            Span::styled(" 2:Services ", Style::default().fg(Color::DarkGray))
        },
        Span::raw("  "),
        if app.tab == Tab::Timeline {
            Span::styled(" 3:Timeline ", Style::default().fg(Color::Black).bg(Color::Cyan))
        } else {
            Span::styled(" 3:Timeline ", Style::default().fg(Color::DarkGray))
        },
        Span::raw("  "),
        if app.tab == Tab::Detail {
            Span::styled(" Trace ", Style::default().fg(Color::Black).bg(Color::Cyan))
        } else {
            Span::raw("")
        },
    ];

    let mut title_parts = vec![Span::styled(" tael ", Style::default().fg(Color::Cyan).bold())];

    if let Some(ref svc) = app.service_filter {
        title_parts.push(Span::styled(
            format!(" service={svc}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(ref st) = app.status_filter {
        title_parts.push(Span::styled(
            format!(" status={st}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !app.filter_text.is_empty() {
        title_parts.push(Span::styled(
            format!(" filter={}", app.filter_text),
            Style::default().fg(Color::Green),
        ));
    }
    if let Some(ref input) = app.filter_input {
        title_parts.push(Span::styled(" /", Style::default().fg(Color::Green).bold()));
        title_parts.push(Span::styled(input.clone(), Style::default().fg(Color::White)));
        title_parts.push(Span::styled("█", Style::default().fg(Color::Green)));
    }
    if app.paused {
        title_parts.push(Span::styled(" PAUSED", Style::default().fg(Color::Red).bold()));
    }

    let header = Paragraph::new(vec![Line::from(title_parts), Line::from(tabs)])
        .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(header, area);
}

fn span_attr_value(span: &SpanRow, key: &str) -> String {
    span.attributes
        .as_object()
        .and_then(|obj| obj.get(key))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

fn draw_traces(frame: &mut Frame, area: Rect, app: &mut App) {
    let mut header_cells = vec![
        Cell::from("Time").style(Style::default().bold()),
        Cell::from("Service").style(Style::default().bold()),
        Cell::from("Operation").style(Style::default().bold()),
        Cell::from("Duration").style(Style::default().bold()),
        Cell::from("Status").style(Style::default().bold()),
        Cell::from("Trace ID").style(Style::default().bold()),
    ];
    for col in &app.pinned_columns {
        header_cells.push(Cell::from(col.clone()).style(Style::default().bold().fg(Color::Yellow)));
    }
    let header = Row::new(header_cells);

    let filtered = app.filtered_spans();
    let filtered_len = filtered.len();
    let pinned = app.pinned_columns.clone();

    let rows: Vec<Row> = filtered
        .iter()
        .map(|s| {
            let time = s
                .start_time
                .split('T')
                .nth(1)
                .unwrap_or(&s.start_time)
                .trim_end_matches('Z');
            let short_time = if time.len() > 12 { &time[..12] } else { time };
            let short_trace = if s.trace_id.len() > 16 {
                &s.trace_id[..16]
            } else {
                &s.trace_id
            };

            let mut cells = vec![
                Cell::from(short_time.to_string()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(s.service.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(s.operation.clone()),
                Cell::from(format!("{:.0}ms", s.duration_ms))
                    .style(Style::default().fg(duration_color(s.duration_ms))),
                Cell::from(s.status.clone())
                    .style(Style::default().fg(status_color(&s.status))),
                Cell::from(short_trace.to_string()).style(Style::default().fg(Color::DarkGray)),
            ];
            for col in &pinned {
                let val = span_attr_value(s, col);
                let style = if val.is_empty() {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                cells.push(Cell::from(if val.is_empty() { "-".to_string() } else { val }).style(style));
            }
            Row::new(cells)
        })
        .collect();

    let mut widths: Vec<Constraint> = vec![
        Constraint::Length(13),
        Constraint::Length(20),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(7),
        Constraint::Length(17),
    ];
    for _ in &app.pinned_columns {
        widths.push(Constraint::Length(18));
    }

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(if app.filter_text.is_empty() {
                    format!(" Traces ({}) ", app.spans.len())
                } else {
                    format!(" Traces ({}/{}) ", filtered_len, app.spans.len())
                })
                .borders(Borders::ALL),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_attr_picker(frame: &mut Frame, area: Rect, app: &mut App) {
    let picker = match app.attr_picker.as_mut() {
        Some(p) => p,
        None => return,
    };

    // Center a popup over the area
    let popup_width = 40u16.min(area.width.saturating_sub(4));
    let popup_height = (picker.keys.len() as u16 + 3).min(area.height.saturating_sub(2)).max(5);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(x, y, popup_width, popup_height);

    frame.render_widget(Clear, popup_area);

    if picker.keys.is_empty() {
        let msg = Paragraph::new(" No attributes found.")
            .block(
                Block::default()
                    .title(" Pin Column (a/esc:close) ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        frame.render_widget(msg, popup_area);
        return;
    }

    let pinned = &app.pinned_columns;
    let rows: Vec<Row> = picker
        .keys
        .iter()
        .map(|k| {
            let is_pinned = pinned.contains(k);
            let marker = if is_pinned { "[x]" } else { "[ ]" };
            let style = if is_pinned {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(marker).style(style),
                Cell::from(k.clone()).style(style),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [Constraint::Length(4), Constraint::Min(10)],
    )
    .block(
        Block::default()
            .title(" Pin Column (enter:toggle, a/esc:close) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(table, popup_area, &mut picker.state);
}

fn draw_selected_span_properties(frame: &mut Frame, area: Rect, app: &App) {
    let span = match app
        .table_state
        .selected()
        .and_then(|i| app.filtered_spans().into_iter().nth(i))
    {
        Some(s) => s,
        None => {
            let msg = Paragraph::new(" No span selected.")
                .block(Block::default().title(" Properties ").borders(Borders::ALL));
            frame.render_widget(msg, area);
            return;
        }
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" trace_id: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.trace_id),
            Span::raw("    "),
            Span::styled("span_id: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.span_id),
            Span::raw("    "),
            Span::styled("parent: ", Style::default().fg(Color::DarkGray)),
            Span::raw(span.parent_span_id.as_deref().unwrap_or("none")),
        ]),
        Line::from(vec![
            Span::styled(" service: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&span.service, Style::default().fg(service_color(&span.service))),
            Span::raw("    "),
            Span::styled("operation: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.operation),
            Span::raw("    "),
            Span::styled("status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&span.status, Style::default().fg(status_color(&span.status))),
            Span::raw("    "),
            Span::styled("duration: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.2}ms", span.duration_ms),
                Style::default().fg(duration_color(span.duration_ms)),
            ),
            Span::raw("    "),
            Span::styled("start: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.start_time),
        ]),
    ];

    if let Some(obj) = span.attributes.as_object() {
        if !obj.is_empty() {
            let mut attr_spans: Vec<Span> =
                vec![Span::styled(" attrs: ", Style::default().fg(Color::DarkGray))];
            for (i, (k, v)) in obj.iter().enumerate() {
                if i > 0 {
                    attr_spans.push(Span::raw("  "));
                }
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                attr_spans.push(Span::styled(
                    format!("{k}="),
                    Style::default().fg(Color::Yellow),
                ));
                attr_spans.push(Span::raw(val));
            }
            lines.push(Line::from(attr_spans));
        }
    }

    if let Some(events) = span.events.as_array() {
        for evt in events {
            let name = evt["name"].as_str().unwrap_or("-");
            let mut evt_spans: Vec<Span> = vec![Span::styled(
                format!(" event({name}): "),
                Style::default().fg(Color::Magenta),
            )];
            if let Some(attrs) = evt["attributes"].as_object() {
                for (i, (k, v)) in attrs.iter().enumerate() {
                    if i > 0 {
                        evt_spans.push(Span::raw("  "));
                    }
                    let val = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    evt_spans.push(Span::styled(
                        format!("{k}="),
                        Style::default().fg(Color::Yellow),
                    ));
                    evt_spans.push(Span::raw(val));
                }
            }
            lines.push(Line::from(evt_spans));
        }
    }

    let block = Block::default().title(" Properties ").borders(Borders::ALL);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_selected_timeline_properties(frame: &mut Frame, area: Rect, app: &App) {
    let trace = match app
        .timeline_state
        .selected()
        .and_then(|i| app.filtered_live_traces().into_iter().nth(i))
        .map(|(_, t)| t)
    {
        Some(t) => t,
        None => {
            let msg = Paragraph::new(" No trace selected.")
                .block(Block::default().title(" Properties ").borders(Borders::ALL));
            frame.render_widget(msg, area);
            return;
        }
    };

    let status_str = if trace.has_error { "error" } else { "ok" };
    let lines = vec![
        Line::from(vec![
            Span::styled(" trace_id: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&trace.trace_id),
        ]),
        Line::from(vec![
            Span::styled(" service: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                &trace.service,
                Style::default().fg(service_color(&trace.service)),
            ),
            Span::raw("    "),
            Span::styled("operation: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&trace.operation),
            Span::raw("    "),
            Span::styled("status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(status_str, Style::default().fg(status_color(status_str))),
        ]),
        Line::from(vec![
            Span::styled(" duration: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.2}ms", trace.duration_ms),
                Style::default().fg(duration_color(trace.duration_ms)),
            ),
            Span::raw("    "),
            Span::styled("spans: ", Style::default().fg(Color::DarkGray)),
            Span::raw(trace.span_count.to_string()),
        ]),
    ];

    let block = Block::default().title(" Properties ").borders(Borders::ALL);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_services(frame: &mut Frame, area: Rect, app: &mut App) {
    let header = Row::new(vec![
        Cell::from("Service").style(Style::default().bold()),
        Cell::from("Spans").style(Style::default().bold()),
        Cell::from("Traces").style(Style::default().bold()),
        Cell::from("Avg Duration").style(Style::default().bold()),
        Cell::from("Error Rate").style(Style::default().bold()),
    ]);

    let rows: Vec<Row> = app
        .services
        .iter()
        .map(|s| {
            let err_color = if s.error_rate > 0.05 {
                Color::Red
            } else if s.error_rate > 0.0 {
                Color::Yellow
            } else {
                Color::Green
            };

            Row::new(vec![
                Cell::from(s.name.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(s.span_count.to_string()),
                Cell::from(s.trace_count.to_string()),
                Cell::from(format!("{:.1}ms", s.avg_duration_ms))
                    .style(Style::default().fg(duration_color(s.avg_duration_ms))),
                Cell::from(format!("{:.1}%", s.error_rate * 100.0))
                    .style(Style::default().fg(err_color)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(20),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(format!(" Services ({}) ", app.services.len()))
            .borders(Borders::ALL),
    );

    frame.render_widget(table, area);
}

fn draw_timeline(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.live_traces_sorted.is_empty() {
        let msg = Paragraph::new(" Waiting for traces...")
            .block(Block::default().title(" Live Timeline ").borders(Borders::ALL));
        frame.render_widget(msg, area);
        return;
    }

    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);

    let label_width: u16 = 30;
    let duration_width: u16 = 10;
    let spans_width: u16 = 6;
    let bar_width = inner
        .width
        .saturating_sub(label_width + duration_width + spans_width + 4);

    // Determine time window
    let window_ms = app.timeline_window_secs * 1000.0;
    let window_end = app
        .live_traces_sorted
        .iter()
        .map(|t| t.end_time_ms)
        .fold(f64::NEG_INFINITY, f64::max);
    let window_start = window_end - window_ms;

    let title = format!(
        " Live Timeline ({}) | window: {:.0}s | +/- zoom ",
        app.live_traces_sorted.len(),
        app.timeline_window_secs
    );
    let block = block.title(title);

    // Time axis header
    let header_row = {
        let axis_width = bar_width as usize;
        let left_label = format!("-{:.0}s", app.timeline_window_secs);
        let mid_label = format!("-{:.0}s", app.timeline_window_secs / 2.0);
        let right_label = "now";

        let mut axis = " ".repeat(axis_width);
        if axis_width > left_label.len() {
            axis.replace_range(0..left_label.len(), &left_label);
        }
        let mid_pos = axis_width / 2;
        if mid_pos + mid_label.len() < axis_width {
            axis.replace_range(mid_pos..mid_pos + mid_label.len(), &mid_label);
        }
        if axis_width >= right_label.len() {
            let end_pos = axis_width - right_label.len();
            axis.replace_range(end_pos..axis_width, right_label);
        }

        Row::new(vec![
            Cell::from(""),
            Cell::from(axis).style(Style::default().fg(Color::DarkGray)),
            Cell::from(""),
            Cell::from(""),
        ])
    };

    // Apply text filter then time window
    let text_filtered = app.filtered_live_traces();
    let visible: Vec<(usize, &LiveTraceRow)> = text_filtered
        .into_iter()
        .filter(|(_, t)| t.end_time_ms >= window_start && t.start_time_ms <= window_end)
        .collect();

    let rows: Vec<Row> = visible
        .iter()
        .map(|(_, trace)| {
            let svc_short = if trace.service.len() > 14 {
                &trace.service[..14]
            } else {
                &trace.service
            };
            let max_op = 14usize;
            let op_short = if trace.operation.len() > max_op {
                &trace.operation[..max_op]
            } else {
                &trace.operation
            };
            let label = format!("{svc_short} {op_short}");
            let label = if label.len() > label_width as usize {
                format!("{}…", &label[..label_width as usize - 1])
            } else {
                format!("{:<width$}", label, width = label_width as usize)
            };

            let offset_pct =
                ((trace.start_time_ms - window_start) / window_ms).clamp(0.0, 1.0);
            let width_pct =
                (trace.duration_ms / window_ms).clamp(0.005, (1.0 - offset_pct).max(0.005));

            let color = if trace.has_error {
                Color::Red
            } else {
                service_color(&trace.service)
            };
            let status_str = if trace.has_error { "error" } else { "ok" };
            let bar =
                render_waterfall_bar(offset_pct, width_pct, bar_width, color, status_str);

            let dur = format!("{:>7.0}ms", trace.duration_ms);
            let spans_label = format!("{:>4}", trace.span_count);

            Row::new(vec![
                Cell::from(label).style(Style::default().fg(service_color(&trace.service))),
                Cell::from(bar),
                Cell::from(dur).style(Style::default().fg(duration_color(trace.duration_ms))),
                Cell::from(spans_label).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    // Map selected index: timeline_state indexes into live_traces_sorted, but table rows
    // are the visible subset. We need to translate.
    let selected_sorted_idx = app.timeline_state.selected();
    let mut display_state = TableState::default();
    if let Some(sel) = selected_sorted_idx {
        for (display_idx, (sorted_idx, _)) in visible.iter().enumerate() {
            if *sorted_idx == sel {
                display_state.select(Some(display_idx));
                break;
            }
        }
    }

    let table = Table::new(
        rows,
        [
            Constraint::Length(label_width),
            Constraint::Min(bar_width),
            Constraint::Length(duration_width),
            Constraint::Length(spans_width),
        ],
    )
    .header(header_row)
    .block(block)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(table, area, &mut display_state);

    // Scrollbar
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let mut scrollbar_state = ScrollbarState::new(app.live_traces_sorted.len())
        .position(app.timeline_state.selected().unwrap_or(0));
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn draw_trace_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.trace_loading {
        let msg = Paragraph::new(" Loading trace...")
            .block(Block::default().title(" Trace ").borders(Borders::ALL));
        frame.render_widget(msg, area);
        return;
    }

    if app.trace_spans.is_empty() {
        let msg = Paragraph::new(" No spans found for this trace. Press Esc to go back.")
            .block(Block::default().title(" Trace ").borders(Borders::ALL));
        frame.render_widget(msg, area);
        return;
    }

    let has_input = app.comment_input.is_some();
    let comment_height = if app.comments.is_empty() && !has_input {
        0
    } else {
        (app.comments.len() as u16 + 2).min(8).max(3) + if has_input { 1 } else { 0 }
    };

    let chunks = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(6),
        Constraint::Length(comment_height),
    ])
    .split(area);

    draw_waterfall(frame, chunks[0], app);
    draw_span_detail(frame, chunks[1], app);
    if comment_height > 0 {
        draw_comments(frame, chunks[2], app);
    }
}

fn draw_waterfall(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL);
    let inner = block.inner(area);

    // Calculate bar width: total width minus label columns
    let label_width: u16 = 32; // service + operation label area
    let duration_width: u16 = 10;
    let bar_width = inner.width.saturating_sub(label_width + duration_width + 3);

    let trace_id = app.trace_spans.first().map(|s| &s.trace_id).cloned().unwrap_or_default();
    let short_trace = if trace_id.len() > 16 { &trace_id[..16] } else { &trace_id };

    // Find trace total duration for the title
    let total_duration: f64 = app.trace_spans.iter()
        .map(|s| s.duration_ms)
        .fold(f64::NEG_INFINITY, f64::max);

    let title = format!(" Trace {short_trace}… │ {:.0}ms │ {} spans ",
        total_duration, app.trace_spans.len());
    let block = block.title(title);

    // Build header with time axis
    let header_row = {
        // Show 0ms on left, total on right
        let root_duration = app.trace_spans.iter()
            .filter(|s| s.parent_span_id.is_none())
            .map(|s| s.duration_ms)
            .fold(0.0f64, f64::max);
        let total = if root_duration > 0.0 { root_duration } else { total_duration };

        let mid = format!("{:.0}ms", total / 2.0);
        let end = format!("{:.0}ms", total);
        let axis_width = bar_width as usize;
        let mid_pos = axis_width / 2;

        let mut axis = " ".repeat(axis_width);
        if axis_width > 0 {
            axis.replace_range(0..1, "0");
        }
        if mid_pos + mid.len() < axis_width {
            axis.replace_range(mid_pos..mid_pos + mid.len(), &mid);
        }
        if axis_width >= end.len() {
            let end_pos = axis_width - end.len();
            axis.replace_range(end_pos..axis_width, &end);
        }

        Row::new(vec![
            Cell::from("").style(Style::default()),
            Cell::from(axis).style(Style::default().fg(Color::DarkGray)),
            Cell::from("").style(Style::default()),
        ])
        .style(Style::default())
    };

    let rows: Vec<Row> = app
        .waterfall_rows
        .iter()
        .map(|wr| {
            let span = &app.trace_spans[wr.span_idx];
            let indent = "  ".repeat(wr.depth.min(6));
            let svc_short = if span.service.len() > 12 {
                &span.service[..12]
            } else {
                &span.service
            };
            let op_short = if span.operation.len() > (16 - indent.len()) {
                &span.operation[..16 - indent.len()]
            } else {
                &span.operation
            };

            let label = format!("{indent}{svc_short} {op_short}");
            let label = if label.len() > label_width as usize {
                format!("{}…", &label[..label_width as usize - 1])
            } else {
                format!("{:<width$}", label, width = label_width as usize)
            };

            let color = if span.status == "error" {
                Color::Red
            } else {
                service_color(&span.service)
            };

            let bar = render_waterfall_bar(wr.offset_pct, wr.width_pct, bar_width, color, &span.status);

            let dur = format!("{:>7.0}ms", span.duration_ms);

            Row::new(vec![
                Cell::from(label).style(Style::default().fg(service_color(&span.service))),
                Cell::from(bar),
                Cell::from(dur).style(Style::default().fg(duration_color(span.duration_ms))),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(label_width),
            Constraint::Min(bar_width),
            Constraint::Length(duration_width),
        ],
    )
    .header(header_row)
    .block(block)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(table, area, &mut app.waterfall_state);

    // Scrollbar
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
    let mut scrollbar_state = ScrollbarState::new(app.waterfall_rows.len())
        .position(app.waterfall_state.selected().unwrap_or(0));
    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn draw_span_detail(frame: &mut Frame, area: Rect, app: &App) {
    let span = match app.selected_waterfall_span() {
        Some(s) => s,
        None => {
            let msg = Paragraph::new(" Select a span in the waterfall above.")
                .block(Block::default().title(" Span Detail ").borders(Borders::ALL));
            frame.render_widget(msg, area);
            return;
        }
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled(" span_id: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.span_id),
            Span::raw("  "),
            Span::styled("service: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&span.service, Style::default().fg(service_color(&span.service))),
            Span::raw("  "),
            Span::styled("op: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&span.operation),
            Span::raw("  "),
            Span::styled("status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&span.status, Style::default().fg(status_color(&span.status))),
            Span::raw("  "),
            Span::styled("duration: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{:.1}ms", span.duration_ms),
                Style::default().fg(duration_color(span.duration_ms)),
            ),
        ]),
    ];

    // Attributes on one line if small, multi-line if large
    if let Some(obj) = span.attributes.as_object() {
        if !obj.is_empty() {
            let mut attr_spans: Vec<Span> = vec![Span::styled(" attrs: ", Style::default().fg(Color::DarkGray))];
            for (i, (k, v)) in obj.iter().enumerate() {
                if i > 0 {
                    attr_spans.push(Span::raw("  "));
                }
                let val = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                attr_spans.push(Span::styled(format!("{k}="), Style::default().fg(Color::Yellow)));
                attr_spans.push(Span::raw(val));
            }
            lines.push(Line::from(attr_spans));
        }
    }

    // Events
    if let Some(events) = span.events.as_array() {
        for evt in events {
            let name = evt["name"].as_str().unwrap_or("-");
            let mut evt_spans: Vec<Span> = vec![
                Span::styled(format!(" event({name}): "), Style::default().fg(Color::Magenta)),
            ];
            if let Some(attrs) = evt["attributes"].as_object() {
                for (i, (k, v)) in attrs.iter().enumerate() {
                    if i > 0 {
                        evt_spans.push(Span::raw("  "));
                    }
                    let val = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    evt_spans.push(Span::styled(format!("{k}="), Style::default().fg(Color::Yellow)));
                    evt_spans.push(Span::raw(val));
                }
            }
            lines.push(Line::from(evt_spans));
        }
    }

    let detail = Paragraph::new(lines).block(
        Block::default()
            .title(" Span Detail ")
            .borders(Borders::ALL),
    );
    frame.render_widget(detail, area);
}

fn draw_comments(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = app
        .comments
        .iter()
        .map(|c| {
            let time = c
                .created_at
                .split('T')
                .nth(1)
                .unwrap_or(&c.created_at)
                .trim_end_matches('Z');
            let short_time = if time.len() > 8 { &time[..8] } else { time };

            let mut parts = vec![
                Span::styled(
                    format!(" {short_time} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(&c.author, Style::default().fg(Color::Yellow)),
            ];
            if let Some(ref sid) = c.span_id {
                let short = if sid.len() > 8 { &sid[..8] } else { sid };
                parts.push(Span::styled(
                    format!(" [{short}…]"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            parts.push(Span::raw(": "));
            parts.push(Span::raw(&c.body));
            Line::from(parts)
        })
        .collect();

    if let Some(ref input) = app.comment_input {
        lines.push(Line::from(vec![
            Span::styled(" > ", Style::default().fg(Color::Cyan).bold()),
            Span::raw(input),
            Span::styled("█", Style::default().fg(Color::Cyan)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::styled(
            " No comments. Press c to add one.",
            Style::default().fg(Color::DarkGray),
        ));
    }

    let title = format!(" Comments ({}) ", app.comments.len());
    let block = Block::default().title(title).borders(Borders::ALL);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let text = if let Some(ref err) = app.last_error {
        Line::from(vec![
            Span::styled(" ERROR: ", Style::default().fg(Color::Red).bold()),
            Span::styled(err, Style::default().fg(Color::Red)),
        ])
    } else if app.attr_picker.is_some() {
        Line::from(vec![
            Span::styled(" j/k", Style::default().fg(Color::Cyan)),
            Span::raw(":navigate  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(":toggle  "),
            Span::styled("a/esc", Style::default().fg(Color::Cyan)),
            Span::raw(":close  "),
        ])
    } else if app.filter_input.is_some() {
        Line::from(vec![
            Span::styled(" enter", Style::default().fg(Color::Cyan)),
            Span::raw(":apply  "),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::raw(":cancel  "),
        ])
    } else if app.comment_input.is_some() {
        Line::from(vec![
            Span::styled(" enter", Style::default().fg(Color::Cyan)),
            Span::raw(":submit  "),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::raw(":cancel  "),
        ])
    } else if app.tab == Tab::Detail {
        Line::from(vec![
            Span::styled(" q", Style::default().fg(Color::Cyan)),
            Span::raw(":quit  "),
            Span::styled("esc", Style::default().fg(Color::Cyan)),
            Span::raw(":back  "),
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(":navigate  "),
            Span::styled("c", Style::default().fg(Color::Cyan)),
            Span::raw(":comment  "),
        ])
    } else if app.tab == Tab::Timeline {
        Line::from(vec![
            Span::styled(" q", Style::default().fg(Color::Cyan)),
            Span::raw(":quit  "),
            Span::styled("1/2/3", Style::default().fg(Color::Cyan)),
            Span::raw(":tab  "),
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(":nav  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(":trace  "),
            Span::styled("+/-", Style::default().fg(Color::Cyan)),
            Span::raw(":zoom  "),
            Span::styled("space", Style::default().fg(Color::Cyan)),
            Span::raw(":pause  "),
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(":filter  "),
            if !app.filter_text.is_empty() {
                Span::styled("\\", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
            if !app.filter_text.is_empty() {
                Span::raw(":clear  ")
            } else {
                Span::raw("")
            },
        ])
    } else {
        Line::from(vec![
            Span::styled(" q", Style::default().fg(Color::Cyan)),
            Span::raw(":quit  "),
            Span::styled("1/2/3", Style::default().fg(Color::Cyan)),
            Span::raw(":tab  "),
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(":nav  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(":trace  "),
            Span::styled("space", Style::default().fg(Color::Cyan)),
            Span::raw(":pause  "),
            Span::styled("/", Style::default().fg(Color::Cyan)),
            Span::raw(":filter  "),
            if app.tab == Tab::Traces && app.table_state.selected().is_some() {
                Span::styled("a", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
            if app.tab == Tab::Traces && app.table_state.selected().is_some() {
                Span::raw(":columns  ")
            } else {
                Span::raw("")
            },
            if !app.filter_text.is_empty() {
                Span::styled("\\", Style::default().fg(Color::Cyan))
            } else {
                Span::raw("")
            },
            if !app.filter_text.is_empty() {
                Span::raw(":clear  ")
            } else {
                Span::raw("")
            },
        ])
    };

    frame.render_widget(Paragraph::new(text), area);
}

pub async fn run(
    server: &str,
    service: Option<String>,
    status: Option<String>,
    interval: u64,
) -> Result<()> {
    enable_raw_mode()?;
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = ratatui::init();

    let mut app = App::new(server, service, status, interval);
    app.poll_initial().await;

    let result = run_loop(&mut terminal, &mut app).await;

    ratatui::restore();
    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;

    result
}

async fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut services_timer = tokio::time::interval(app.services_interval);
    services_timer.tick().await; // consume first immediate tick

    loop {
        terminal.draw(|frame| draw(frame, app))?;

        tokio::select! {
            Some(json) = app.sse_rx.recv() => {
                app.ingest_sse(&json);
            }
            _ = services_timer.tick() => {
                app.poll_services().await;
            }
            ready = tokio::task::spawn_blocking(|| {
                event::poll(Duration::from_millis(50)).unwrap_or(false)
            }) => {
                if ready.unwrap_or(false) {
                    if let Event::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Press {
                            let current_tab = app.tab;
                            match app.handle_key(key.code) {
                                Some("load_trace") => {
                                    let trace_id = if current_tab == Tab::Timeline {
                                        app.timeline_state.selected().and_then(|i| {
                                            app.filtered_live_traces().get(i).map(|(_, t)| t.trace_id.clone())
                                        })
                                    } else {
                                        app.table_state.selected().and_then(|i| {
                                            app.filtered_spans().get(i).map(|s| s.trace_id.clone())
                                        })
                                    };
                                    if let Some(trace_id) = trace_id {
                                        app.prev_tab = current_tab;
                                        app.tab = Tab::Detail;
                                        app.load_trace(&trace_id).await;
                                    }
                                }
                                Some("submit_comment") => {
                                    app.submit_comment().await;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}
