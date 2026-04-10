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
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use serde_json::Value;

use crate::client::TaelClient;

struct Comment {
    author: String,
    body: String,
    created_at: String,
    span_id: Option<String>,
}

struct App {
    client: TaelClient,
    service_filter: Option<String>,
    status_filter: Option<String>,
    poll_interval: Duration,
    spans: Vec<SpanRow>,
    services: Vec<ServiceRow>,
    table_state: TableState,
    tab: Tab,
    should_quit: bool,
    last_error: Option<String>,
    paused: bool,
    // Trace waterfall state
    trace_spans: Vec<SpanRow>,
    waterfall_rows: Vec<WaterfallRow>,
    waterfall_state: TableState,
    trace_loading: bool,
    // Comments
    comments: Vec<Comment>,
    comment_input: Option<String>,
    current_trace_id: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Traces,
    Services,
    Detail,
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
        Self {
            client: TaelClient::new(server),
            service_filter: service,
            status_filter: status,
            poll_interval: Duration::from_secs(interval),
            spans: Vec::new(),
            services: Vec::new(),
            table_state: TableState::default(),
            tab: Tab::Traces,
            should_quit: false,
            last_error: None,
            paused: false,
            trace_spans: Vec::new(),
            waterfall_rows: Vec::new(),
            waterfall_state: TableState::default(),
            trace_loading: false,
            comments: Vec::new(),
            comment_input: None,
            current_trace_id: None,
        }
    }

    async fn poll_data(&mut self) {
        if self.paused {
            return;
        }

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
                self.last_error = None;
                self.spans = parse_spans(&val);
            }
            Err(e) => {
                self.last_error = Some(format!("traces: {e}"));
            }
        }

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

    fn selected_span(&self) -> Option<&SpanRow> {
        self.table_state.selected().and_then(|i| self.spans.get(i))
    }

    fn selected_waterfall_span(&self) -> Option<&SpanRow> {
        self.waterfall_state
            .selected()
            .and_then(|i| self.waterfall_rows.get(i))
            .map(|wr| &self.trace_spans[wr.span_idx])
    }

    /// Returns action: None = normal, Some("load_trace") = load trace, Some("submit_comment") = submit
    fn handle_key(&mut self, code: KeyCode) -> Option<&'static str> {
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
                    self.tab = Tab::Traces;
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
            KeyCode::Char(' ') => self.paused = !self.paused,
            KeyCode::Char('c') => {
                if self.tab == Tab::Detail {
                    self.comment_input = Some(String::new());
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.tab == Tab::Detail {
                    let len = self.waterfall_rows.len();
                    if len > 0 {
                        let i = self.waterfall_state.selected().map(|i| i + 1).unwrap_or(0);
                        self.waterfall_state.select(Some(i.min(len - 1)));
                    }
                } else if self.tab == Tab::Traces && !self.spans.is_empty() {
                    let i = self.table_state.selected().map(|i| i + 1).unwrap_or(0);
                    self.table_state.select(Some(i.min(self.spans.len() - 1)));
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
            }
            KeyCode::Backspace => {
                if self.tab == Tab::Detail {
                    self.tab = Tab::Traces;
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
                let width_pct = (span.duration_ms / trace_duration).clamp(0.005, 1.0 - offset_pct);

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
            let width_pct = (span.duration_ms / trace_duration).clamp(0.005, 1.0 - offset_pct);
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
        Tab::Traces => draw_traces(frame, chunks[1], app),
        Tab::Services => draw_services(frame, chunks[1], app),
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
    if app.paused {
        title_parts.push(Span::styled(" PAUSED", Style::default().fg(Color::Red).bold()));
    }

    let header = Paragraph::new(vec![Line::from(title_parts), Line::from(tabs)])
        .block(Block::default().borders(Borders::BOTTOM));

    frame.render_widget(header, area);
}

fn draw_traces(frame: &mut Frame, area: Rect, app: &mut App) {
    let header = Row::new(vec![
        Cell::from("Time").style(Style::default().bold()),
        Cell::from("Service").style(Style::default().bold()),
        Cell::from("Operation").style(Style::default().bold()),
        Cell::from("Duration").style(Style::default().bold()),
        Cell::from("Status").style(Style::default().bold()),
        Cell::from("Trace ID").style(Style::default().bold()),
    ]);

    let rows: Vec<Row> = app
        .spans
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

            Row::new(vec![
                Cell::from(short_time.to_string()).style(Style::default().fg(Color::DarkGray)),
                Cell::from(s.service.clone()).style(Style::default().fg(Color::Cyan)),
                Cell::from(s.operation.clone()),
                Cell::from(format!("{:.0}ms", s.duration_ms))
                    .style(Style::default().fg(duration_color(s.duration_ms))),
                Cell::from(s.status.clone())
                    .style(Style::default().fg(status_color(&s.status))),
                Cell::from(short_trace.to_string()).style(Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(13),
            Constraint::Length(20),
            Constraint::Min(20),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(17),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(format!(" Traces ({}) ", app.spans.len()))
            .borders(Borders::ALL),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(table, area, &mut app.table_state);
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
    } else {
        Line::from(vec![
            Span::styled(" q", Style::default().fg(Color::Cyan)),
            Span::raw(":quit  "),
            Span::styled("1/2", Style::default().fg(Color::Cyan)),
            Span::raw(":tab  "),
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(":nav  "),
            Span::styled("enter", Style::default().fg(Color::Cyan)),
            Span::raw(":trace  "),
            Span::styled("space", Style::default().fg(Color::Cyan)),
            Span::raw(":pause  "),
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
    app.poll_data().await;

    let result = run_loop(&mut terminal, &mut app).await;

    ratatui::restore();
    disable_raw_mode()?;
    crossterm::execute!(io::stdout(), LeaveAlternateScreen)?;

    result
}

async fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut poll_timer = tokio::time::interval(app.poll_interval);
    poll_timer.tick().await; // consume first immediate tick

    loop {
        terminal.draw(|frame| draw(frame, app))?;

        tokio::select! {
            _ = poll_timer.tick() => {
                app.poll_data().await;
            }
            ready = tokio::task::spawn_blocking(|| {
                event::poll(Duration::from_millis(50)).unwrap_or(false)
            }) => {
                if ready.unwrap_or(false) {
                    if let Event::Key(key) = event::read()? {
                        if key.kind == KeyEventKind::Press {
                            match app.handle_key(key.code) {
                                Some("load_trace") => {
                                    if let Some(span) = app.selected_span() {
                                        let trace_id = span.trace_id.clone();
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
