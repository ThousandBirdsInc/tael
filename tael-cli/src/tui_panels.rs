//! The TUI panels for everything the trace/service/eval views don't cover.
//!
//! `tael live` grew up around the four things a human watches while a system is
//! running: spans, services, a timeline, and an eval run. The features added
//! since — alerting, scoring, suites, the review queue, clustering, topology,
//! SQL — were reachable only from the CLI, which meant a person spot-checking a
//! system in the TUI had to drop out of it to answer "is anything firing?".
//!
//! These panels close that gap. They are deliberately read-mostly: creating an
//! alert rule or filing a review request is an agent's job and stays in the
//! CLI, where it can be scripted and its exit code checked. What a human needs
//! is to *see* the state, which is what this is.
//!
//! Each panel owns its data, its selection, and its own error string, so one
//! endpoint being unavailable (SQL on a build without the feature, clustering
//! before anything is embedded) degrades to a message in that panel instead of
//! taking down the view.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use serde_json::Value;

use crate::client::TaelClient;

/// The panels added on top of the original four views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Panel {
    Health,
    Topology,
    Automation,
    Clusters,
    Review,
    Sql,
}

impl Panel {
    pub fn label(self) -> &'static str {
        match self {
            Panel::Health => "Health",
            Panel::Topology => "Topology",
            Panel::Automation => "Automation",
            Panel::Clusters => "Clusters",
            Panel::Review => "Review",
            Panel::Sql => "SQL",
        }
    }
}

/// Shared per-panel bookkeeping: whether a fetch has happened, and what went
/// wrong if one did and failed.
#[derive(Default)]
struct Fetch {
    loaded: bool,
    error: Option<String>,
}

impl Fetch {
    /// Record the outcome of a fetch. Returns the payload when it succeeded, so
    /// callers can skip parsing on failure without repeating the match.
    ///
    /// A transport error is not the only way a fetch fails. Several endpoints
    /// answer a refusal with a normal body carrying an `error` key — SQL on a
    /// build without the engine is a 400 whose message names the feature to
    /// install — and some client methods do not raise on status. Treating that
    /// as success would render an empty table and tell the user "no rows" when
    /// the truth is "this build cannot answer that".
    fn record(&mut self, result: anyhow::Result<Value>) -> Option<Value> {
        self.loaded = true;
        match result {
            Ok(value) => match value.get("error").and_then(Value::as_str) {
                Some(message) => {
                    self.error = Some(message.to_string());
                    None
                }
                None => {
                    self.error = None;
                    Some(value)
                }
            },
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }
}

#[derive(Default)]
pub struct Panels {
    health: Health,
    topology: Topology,
    automation: Automation,
    clusters: Clusters,
    review: Review,
    sql: Sql,
    /// The window every panel that takes one is scoped to, mirroring the CLI's
    /// `--last`. Changing it re-fetches on the next refresh.
    pub window: String,
}

impl Panels {
    pub fn new(window: &str) -> Self {
        Self {
            window: window.to_string(),
            ..Default::default()
        }
    }

    /// Whether a panel has never been fetched — the trigger for loading it the
    /// first time it is opened rather than fetching all six up front.
    pub fn needs_load(&self, panel: Panel) -> bool {
        !match panel {
            Panel::Health => self.health.fetch.loaded,
            Panel::Topology => self.topology.fetch.loaded,
            Panel::Automation => self.automation.fetch.loaded,
            Panel::Clusters => self.clusters.fetch.loaded,
            Panel::Review => self.review.fetch.loaded,
            Panel::Sql => self.sql.fetch.loaded,
        }
    }

    pub async fn refresh(&mut self, client: &TaelClient, panel: Panel) {
        match panel {
            Panel::Health => self.health.refresh(client, &self.window).await,
            Panel::Topology => self.topology.refresh(client, &self.window).await,
            Panel::Automation => self.automation.refresh(client).await,
            Panel::Clusters => self.clusters.refresh(client).await,
            Panel::Review => self.review.refresh(client).await,
            Panel::Sql => self.sql.refresh(client).await,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, panel: Panel) {
        if let Some(error) = self.error_of(panel) {
            draw_message(frame, area, panel.label(), &error, Color::Red);
            return;
        }
        match panel {
            Panel::Health => self.health.draw(frame, area, &self.window),
            Panel::Topology => self.topology.draw(frame, area, &self.window),
            Panel::Automation => self.automation.draw(frame, area),
            Panel::Clusters => self.clusters.draw(frame, area),
            Panel::Review => self.review.draw(frame, area),
            Panel::Sql => self.sql.draw(frame, area),
        }
    }

    fn error_of(&self, panel: Panel) -> Option<String> {
        match panel {
            Panel::Health => self.health.fetch.error.clone(),
            Panel::Topology => self.topology.fetch.error.clone(),
            Panel::Automation => self.automation.fetch.error.clone(),
            Panel::Clusters => self.clusters.fetch.error.clone(),
            Panel::Review => self.review.fetch.error.clone(),
            Panel::Sql => self.sql.fetch.error.clone(),
        }
    }

    /// Move the selection within whichever panel owns one. Panels without a
    /// list (Health) ignore this.
    pub fn move_selection(&mut self, panel: Panel, delta: isize) {
        let (state, len) = match panel {
            Panel::Topology => (&mut self.topology.state, self.topology.edges.len()),
            Panel::Automation => {
                let len = self.automation.rows();
                (&mut self.automation.state, len)
            }
            Panel::Clusters => (&mut self.clusters.state, self.clusters.clusters.len()),
            Panel::Review => (&mut self.review.state, self.review.reviews.len()),
            Panel::Sql => (&mut self.sql.state, self.sql.rows.len()),
            Panel::Health => return,
        };
        if len == 0 {
            state.select(None);
            return;
        }
        let next = match state.selected() {
            Some(current) => (current as isize + delta).clamp(0, len as isize - 1) as usize,
            None if delta >= 0 => 0,
            None => len - 1,
        };
        state.select(Some(next));
    }

    /// The trace id under the cursor, for panels whose rows point at one. Lets
    /// `enter` open the trace detail view from a cluster exemplar or a review
    /// request, which is the whole reason to look at those lists.
    pub fn selected_trace_id(&self, panel: Panel) -> Option<String> {
        match panel {
            Panel::Clusters => {
                let cluster = self
                    .clusters
                    .clusters
                    .get(self.clusters.state.selected()?)?;
                Some(cluster.exemplar.clone())
            }
            Panel::Review => {
                let review = self.review.reviews.get(self.review.state.selected()?)?;
                review.trace_id.clone()
            }
            _ => None,
        }
    }

    /// SQL is the one panel that takes input; the TUI routes typing here while
    /// the query line is being edited.
    pub fn sql_editing(&self) -> bool {
        self.sql.editing
    }

    pub fn sql_begin_edit(&mut self) {
        self.sql.editing = true;
    }

    pub fn sql_key(&mut self, key: char) {
        self.sql.query.push(key);
    }

    pub fn sql_backspace(&mut self) {
        self.sql.query.pop();
    }

    pub fn sql_cancel_edit(&mut self) {
        self.sql.editing = false;
    }

    /// Submit the query line. Returns false when it is blank, so the caller can
    /// skip a pointless round trip.
    pub fn sql_submit(&mut self) -> bool {
        self.sql.editing = false;
        !self.sql.query.trim().is_empty()
    }
}

// ── Health: summary + anomalies over a window ───────────────────────

#[derive(Default)]
struct Health {
    fetch: Fetch,
    summary: Option<Value>,
    anomalies: Vec<Value>,
}

impl Health {
    async fn refresh(&mut self, client: &TaelClient, window: &str) {
        let summary = client.summary(Some(window), None).await;
        let Some(summary) = self.fetch.record(summary) else {
            return;
        };
        self.summary = Some(summary);
        // The baseline is four windows back, so "the last hour against the
        // last four" is the default question — the same shape `tael anomalies`
        // uses when it is not given one.
        let baseline = multiply_window(window, 4);
        if let Some(report) = self
            .fetch
            .record(client.anomalies(Some(window), Some(&baseline), None).await)
        {
            self.anomalies = array(&report, "anomalies");
        }
    }

    fn draw(&self, frame: &mut Frame, area: Rect, window: &str) {
        let Some(summary) = &self.summary else {
            draw_message(frame, area, "Health", "No summary yet.", Color::DarkGray);
            return;
        };
        let splits = Layout::vertical([Constraint::Length(11), Constraint::Min(6)]).split(area);

        let traces = summary.get("traces").cloned().unwrap_or(Value::Null);
        let logs = summary.get("logs").cloned().unwrap_or(Value::Null);
        let error_rate = num(&traces, "error_rate");
        let lines = vec![
            Line::from(vec![
                Span::styled("spans     ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:>10}", num(&traces, "span_count") as i64),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled("      traces  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", num(&traces, "trace_count") as i64),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled("errors    ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:>10}", num(&traces, "error_count") as i64),
                    Style::default().fg(rate_color(error_rate)),
                ),
                Span::styled("      rate    ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{:.2}%", error_rate * 100.0),
                    Style::default().fg(rate_color(error_rate)),
                ),
            ]),
            Line::from(vec![
                Span::styled("p50/p95/p99 ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "{:.1}ms / {:.1}ms / {:.1}ms",
                        num(&traces, "p50_ms"),
                        num(&traces, "p95_ms"),
                        num(&traces, "p99_ms")
                    ),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("logs      ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "{} total, {} error, {} warn",
                        num(&logs, "total") as i64,
                        num(&logs, "error") as i64,
                        num(&logs, "warn") as i64
                    ),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "top error operations",
                Style::default().fg(Color::DarkGray),
            )),
        ]
        .into_iter()
        .chain(
            array(summary, "top_error_operations")
                .iter()
                .take(3)
                .map(|op| {
                    Line::from(vec![
                        Span::styled(
                            format!("  {:>5}  ", num(op, "error_count") as i64),
                            Style::default().fg(Color::Red),
                        ),
                        Span::styled(text(op, "service"), Style::default().fg(Color::Cyan)),
                        Span::raw(" "),
                        Span::raw(text(op, "operation")),
                    ])
                })
                .collect::<Vec<_>>(),
        )
        .collect::<Vec<_>>();

        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(format!(" Health (last {window}) "))
                    .borders(Borders::ALL),
            ),
            splits[0],
        );

        let rows: Vec<Row> = self
            .anomalies
            .iter()
            .map(|a| {
                let severity = text(a, "severity");
                Row::new(vec![
                    Cell::from(text(a, "service")).style(Style::default().fg(Color::Cyan)),
                    Cell::from(text(a, "kind")),
                    Cell::from(severity.clone()).style(Style::default().fg(
                        if severity == "high" {
                            Color::Red
                        } else {
                            Color::Yellow
                        },
                    )),
                    Cell::from(format!("{:.2}", num(a, "baseline"))),
                    Cell::from(format!("{:.2}", num(a, "current"))),
                    Cell::from(text(a, "description")),
                ])
            })
            .collect();

        if rows.is_empty() {
            draw_message(
                frame,
                splits[1],
                "Anomalies",
                "Nothing regressed against the baseline window.",
                Color::Green,
            );
            return;
        }

        frame.render_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(16),
                    Constraint::Length(12),
                    Constraint::Length(8),
                    Constraint::Length(10),
                    Constraint::Length(10),
                    Constraint::Min(20),
                ],
            )
            .header(header_row(&[
                "Service",
                "Kind",
                "Severity",
                "Baseline",
                "Current",
                "Description",
            ]))
            .block(
                Block::default()
                    .title(format!(" Anomalies ({}) ", self.anomalies.len()))
                    .borders(Borders::ALL),
            ),
            splits[1],
        );
    }
}

// ── Topology: the service graph derived from parent/child edges ─────

#[derive(Default)]
struct Topology {
    fetch: Fetch,
    edges: Vec<Value>,
    state: TableState,
    spans_examined: i64,
    dangling: i64,
}

impl Topology {
    async fn refresh(&mut self, client: &TaelClient, window: &str) {
        let Some(report) = self
            .fetch
            .record(client.topology(Some(window), 50_000).await)
        else {
            return;
        };
        self.edges = array(&report, "edges");
        self.spans_examined = num(&report, "spans_examined") as i64;
        self.dangling = num(&report, "spans_with_parent_outside_window") as i64;
        if self.state.selected().is_none() && !self.edges.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, window: &str) {
        if self.edges.is_empty() {
            draw_message(
                frame,
                area,
                "Topology",
                "No parent/child edges in this window. A single-service trace has no graph.",
                Color::DarkGray,
            );
            return;
        }

        let rows: Vec<Row> = self
            .edges
            .iter()
            .map(|e| {
                let error_rate = num(e, "error_rate");
                Row::new(vec![
                    Cell::from(text(e, "from")).style(Style::default().fg(Color::Cyan)),
                    Cell::from("→").style(Style::default().fg(Color::DarkGray)),
                    Cell::from(text(e, "to")).style(Style::default().fg(Color::Cyan)),
                    Cell::from(format!("{}", num(e, "calls") as i64)),
                    Cell::from(format!("{}", num(e, "errors") as i64))
                        .style(Style::default().fg(rate_color(error_rate))),
                    Cell::from(format!("{:.1}%", error_rate * 100.0))
                        .style(Style::default().fg(rate_color(error_rate))),
                    Cell::from(format!("{:.1}ms", num(e, "avg_duration_ms"))),
                ])
            })
            .collect();

        // A span whose parent fell outside the window looks like an entry point
        // and would overstate the roots, so the count is on the title rather
        // than buried — it is the number that says whether to widen the window.
        let title = if self.dangling > 0 {
            format!(
                " Topology (last {window}, {} edges, {} spans, {} with parent outside window) ",
                self.edges.len(),
                self.spans_examined,
                self.dangling
            )
        } else {
            format!(
                " Topology (last {window}, {} edges, {} spans) ",
                self.edges.len(),
                self.spans_examined
            )
        };

        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Min(14),
                    Constraint::Length(3),
                    Constraint::Min(14),
                    Constraint::Length(10),
                    Constraint::Length(8),
                    Constraint::Length(10),
                    Constraint::Length(12),
                ],
            )
            .header(header_row(&[
                "From", "", "To", "Calls", "Errors", "Rate", "Avg",
            ]))
            .block(Block::default().title(title).borders(Borders::ALL))
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            area,
            &mut self.state,
        );
    }
}

// ── Automation: alert rules, their firings, and scoring rules ───────

#[derive(Default)]
struct Automation {
    fetch: Fetch,
    alerts: Vec<Value>,
    events: Vec<Value>,
    score_rules: Vec<Value>,
    state: TableState,
}

impl Automation {
    fn rows(&self) -> usize {
        self.alerts.len()
    }

    async fn refresh(&mut self, client: &TaelClient) {
        if let Some(list) = self.fetch.record(client.list_alerts().await) {
            self.alerts = array(&list, "alerts");
        }
        if let Some(feed) = self.fetch.record(client.alert_events(20).await) {
            self.events = array(&feed, "events");
        }
        if let Some(rules) = self.fetch.record(client.list_score_rules().await) {
            self.score_rules = array(&rules, "rules");
        }
        if self.state.selected().is_none() && !self.alerts.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let splits = Layout::vertical([
            Constraint::Percentage(40),
            Constraint::Percentage(35),
            Constraint::Percentage(25),
        ])
        .split(area);

        let alert_rows: Vec<Row> = self
            .alerts
            .iter()
            .map(|a| {
                let state = text(a, "state");
                Row::new(vec![
                    Cell::from(text(a, "name")).style(Style::default().fg(Color::Cyan)),
                    Cell::from(state.clone()).style(Style::default().fg(alert_state_color(&state))),
                    Cell::from(format!("{}s", num(a, "for_seconds") as i64)),
                    Cell::from(format!("{}", array(a, "sinks").len())),
                    Cell::from(text(a, "query")),
                ])
            })
            .collect();

        if alert_rows.is_empty() {
            draw_message(
                frame,
                splits[0],
                "Alert rules",
                "No alert rules. Create one with `tael alert create`.",
                Color::DarkGray,
            );
        } else {
            frame.render_stateful_widget(
                Table::new(
                    alert_rows,
                    [
                        Constraint::Length(20),
                        Constraint::Length(10),
                        Constraint::Length(8),
                        Constraint::Length(7),
                        Constraint::Min(20),
                    ],
                )
                .header(header_row(&["Rule", "State", "For", "Sinks", "Query"]))
                .block(
                    Block::default()
                        .title(format!(" Alert rules ({}) ", self.alerts.len()))
                        .borders(Borders::ALL),
                )
                .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
                splits[0],
                &mut self.state,
            );
        }

        let event_rows: Vec<Row> = self
            .events
            .iter()
            .map(|e| {
                let state = text(e, "state");
                Row::new(vec![
                    Cell::from(short_time(&text(e, "at")))
                        .style(Style::default().fg(Color::DarkGray)),
                    Cell::from(text(e, "rule")).style(Style::default().fg(Color::Cyan)),
                    Cell::from(format!("{} → {}", text(e, "previous_state"), state.clone()))
                        .style(Style::default().fg(alert_state_color(&state))),
                    Cell::from(format!("{} series", array(e, "matched").len())),
                ])
            })
            .collect();

        if event_rows.is_empty() {
            draw_message(
                frame,
                splits[1],
                "Alert feed",
                "Nothing has fired.",
                Color::Green,
            );
        } else {
            frame.render_widget(
                Table::new(
                    event_rows,
                    [
                        Constraint::Length(14),
                        Constraint::Length(20),
                        Constraint::Length(22),
                        Constraint::Min(12),
                    ],
                )
                .header(header_row(&["When", "Rule", "Transition", "Matched"]))
                .block(
                    Block::default()
                        .title(format!(" Alert feed ({}) ", self.events.len()))
                        .borders(Borders::ALL),
                ),
                splits[1],
            );
        }

        let score_rows: Vec<Row> = self
            .score_rules
            .iter()
            .map(|r| {
                let status = r.get("status").cloned().unwrap_or(Value::Null);
                let last_error = text(&status, "last_error");
                // Status field names mirror `RuleStatus` on the server
                // (`traces_sampled` / `scores_written`), same as
                // `tael score rule list` reads them.
                Row::new(vec![
                    Cell::from(text(r, "name")).style(Style::default().fg(Color::Cyan)),
                    Cell::from(format!("{:.0}%", num(r, "sample") * 100.0)),
                    Cell::from(format!("{}", num(&status, "traces_sampled") as i64)),
                    Cell::from(format!("{}", num(&status, "scores_written") as i64)),
                    Cell::from(if last_error.is_empty() {
                        "-".to_string()
                    } else {
                        last_error
                    })
                    .style(Style::default().fg(Color::Red)),
                    Cell::from(text(r, "command")),
                ])
            })
            .collect();

        if score_rows.is_empty() {
            draw_message(
                frame,
                splits[2],
                "Scoring rules",
                "No scoring rules. Create one with `tael score rule create`.",
                Color::DarkGray,
            );
        } else {
            frame.render_widget(
                Table::new(
                    score_rows,
                    [
                        Constraint::Length(20),
                        Constraint::Length(8),
                        Constraint::Length(8),
                        Constraint::Length(8),
                        Constraint::Length(24),
                        Constraint::Min(16),
                    ],
                )
                .header(header_row(&[
                    "Rule",
                    "Sample",
                    "Sampled",
                    "Scored",
                    "Last error",
                    "Command",
                ]))
                .block(
                    Block::default()
                        .title(format!(" Scoring rules ({}) ", self.score_rules.len()))
                        .borders(Borders::ALL),
                ),
                splits[2],
            );
        }
    }
}

// ── Clusters: grouped failures, with cohesion shown ─────────────────

struct ClusterRow {
    id: i64,
    exemplar: String,
    size: i64,
    cohesion: f64,
}

#[derive(Default)]
struct Clusters {
    fetch: Fetch,
    clusters: Vec<ClusterRow>,
    corpus_size: i64,
    state: TableState,
}

impl Clusters {
    async fn refresh(&mut self, client: &TaelClient) {
        let Some(report) = self.fetch.record(client.cluster_traces(5).await) else {
            return;
        };
        self.corpus_size = num(&report, "corpus_size") as i64;
        self.clusters = array(&report, "clusters")
            .iter()
            .map(|c| ClusterRow {
                id: num(c, "id") as i64,
                exemplar: text(c, "exemplar"),
                size: num(c, "size") as i64,
                cohesion: num(c, "cohesion"),
            })
            .collect();
        if self.state.selected().is_none() && !self.clusters.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if self.clusters.is_empty() {
            draw_message(
                frame,
                area,
                "Clusters",
                "Nothing embedded yet. Run `tael embed --command <your embedder>` first.",
                Color::DarkGray,
            );
            return;
        }

        let rows: Vec<Row> = self
            .clusters
            .iter()
            .map(|c| {
                // Cohesion is the honest part of this view: a tight number means
                // the grouping is real, a loose one means read the exemplar
                // before believing it.
                let color = if c.cohesion >= 0.85 {
                    Color::Green
                } else if c.cohesion >= 0.7 {
                    Color::Yellow
                } else {
                    Color::Red
                };
                Row::new(vec![
                    Cell::from(format!("#{}", c.id)).style(Style::default().fg(Color::Cyan)),
                    Cell::from(c.size.to_string()),
                    Cell::from(format!("{:.3}", c.cohesion)).style(Style::default().fg(color)),
                    Cell::from(if c.cohesion >= 0.7 { "" } else { "weak" })
                        .style(Style::default().fg(Color::Red)),
                    Cell::from(c.exemplar.clone()).style(Style::default().fg(Color::DarkGray)),
                ])
            })
            .collect();

        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(6),
                    Constraint::Length(8),
                    Constraint::Length(10),
                    Constraint::Length(6),
                    Constraint::Min(20),
                ],
            )
            .header(header_row(&[
                "Cluster",
                "Size",
                "Cohesion",
                "",
                "Exemplar trace (enter to open)",
            ]))
            .block(
                Block::default()
                    .title(format!(
                        " Clusters ({} over {} embedded traces) ",
                        self.clusters.len(),
                        self.corpus_size
                    ))
                    .borders(Borders::ALL),
            )
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            area,
            &mut self.state,
        );
    }
}

// ── Review: the queue of questions an agent filed for a human ───────

struct ReviewRow {
    review_id: String,
    state: String,
    trace_id: Option<String>,
    question: String,
    answer: Option<String>,
}

#[derive(Default)]
struct Review {
    fetch: Fetch,
    reviews: Vec<ReviewRow>,
    state: TableState,
}

impl Review {
    async fn refresh(&mut self, client: &TaelClient) {
        let Some(list) = self.fetch.record(client.list_comments(500).await) else {
            return;
        };
        self.reviews = derive_reviews(&array(&list, "comments"));
        if self.state.selected().is_none() && !self.reviews.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if self.reviews.is_empty() {
            draw_message(
                frame,
                area,
                "Review queue",
                "Nothing waiting on a human.",
                Color::Green,
            );
            return;
        }

        let open = self.reviews.iter().filter(|r| r.state == "open").count();
        let rows: Vec<Row> = self
            .reviews
            .iter()
            .map(|r| {
                Row::new(vec![
                    Cell::from(r.state.clone()).style(Style::default().fg(if r.state == "open" {
                        Color::Yellow
                    } else {
                        Color::Green
                    })),
                    Cell::from(
                        r.trace_id
                            .as_deref()
                            .map(|t| t.chars().take(12).collect::<String>())
                            .unwrap_or_else(|| "-".to_string()),
                    )
                    .style(Style::default().fg(Color::DarkGray)),
                    Cell::from(r.question.clone()),
                    Cell::from(r.answer.clone().unwrap_or_default())
                        .style(Style::default().fg(Color::Green)),
                ])
            })
            .collect();

        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(10),
                    Constraint::Length(14),
                    Constraint::Min(30),
                    Constraint::Min(20),
                ],
            )
            .header(header_row(&[
                "State",
                "Trace",
                "Question (enter to open trace)",
                "Answer",
            ]))
            .block(
                Block::default()
                    .title(format!(
                        " Review queue ({} open of {}) ",
                        open,
                        self.reviews.len()
                    ))
                    .borders(Borders::ALL),
            )
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            area,
            &mut self.state,
        );
    }
}

/// Derive review rows from raw comment records.
///
/// Requests and answers are both structured trace comments, and an answer
/// references its request rather than mutating it — so the effective state is
/// derived here exactly as `tael review list` derives it, not read from a
/// field. The body names the review and carries the question, but the trace a
/// request hangs off lives on the comment record itself: `tael review request`
/// posts a body without a trace_id and the server stamps the comment. Reading
/// only the body would leave the Trace column empty and make enter-to-open a
/// no-op on every row.
fn derive_reviews(comments: &[Value]) -> Vec<ReviewRow> {
    let parsed: Vec<(&Value, Value)> = comments
        .iter()
        .filter_map(|c| {
            serde_json::from_str::<Value>(&text(c, "body"))
                .ok()
                .map(|body| (c, body))
        })
        .collect();

    let answers: std::collections::HashMap<String, &Value> = parsed
        .iter()
        .filter(|(_, body)| text(body, "kind") == "review_answer")
        .map(|(_, body)| (text(body, "review_id"), body))
        .collect();

    let mut reviews: Vec<ReviewRow> = parsed
        .iter()
        .filter(|(_, body)| text(body, "kind") == "review_request")
        .map(|(comment, body)| {
            let review_id = text(body, "review_id");
            let answer = answers.get(&review_id);
            ReviewRow {
                state: if answer.is_some() { "answered" } else { "open" }.to_string(),
                trace_id: comment
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .or_else(|| body.get("trace_id").and_then(Value::as_str))
                    .map(str::to_string),
                question: text(body, "question"),
                answer: answer.map(|a| text(a, "answer")),
                review_id,
            }
        })
        .collect();
    // Open questions first: they are the ones that need a human.
    reviews.sort_by_key(|r| (r.state != "open", r.review_id.clone()));
    reviews
}

// ── SQL: a read-only query console ──────────────────────────────────

#[derive(Default)]
struct Sql {
    fetch: Fetch,
    query: String,
    editing: bool,
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    state: TableState,
}

impl Sql {
    async fn refresh(&mut self, client: &TaelClient) {
        if self.query.trim().is_empty() {
            // Something to look at on first open that also proves the feature
            // is compiled in, without the user having to guess a table name.
            self.query =
                "SELECT service, count(*) AS spans FROM spans GROUP BY service ORDER BY spans DESC"
                    .to_string();
        }
        let Some(result) = self.fetch.record(client.query_sql(&self.query).await) else {
            self.columns.clear();
            self.rows.clear();
            return;
        };
        let rows = array(&result, "rows");
        // Column order comes from the first row's key order, which is the order
        // the engine produced them in.
        self.columns = rows
            .first()
            .and_then(|r| r.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        self.rows = rows
            .iter()
            .map(|row| {
                self.columns
                    .iter()
                    .map(|c| match row.get(c) {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    })
                    .collect()
            })
            .collect();
        self.state
            .select(if self.rows.is_empty() { None } else { Some(0) });
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let splits = Layout::vertical([Constraint::Length(4), Constraint::Min(6)]).split(area);

        let mut query_line = vec![Span::styled("> ", Style::default().fg(Color::Green))];
        query_line.push(Span::raw(self.query.clone()));
        if self.editing {
            query_line.push(Span::styled("█", Style::default().fg(Color::Green)));
        }
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(query_line),
                Line::from(Span::styled(
                    if self.editing {
                        "enter runs it, esc cancels"
                    } else {
                        "e to edit, r to re-run"
                    },
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .wrap(Wrap { trim: false })
            .block(Block::default().title(" SQL ").borders(Borders::ALL)),
            splits[0],
        );

        if self.rows.is_empty() {
            draw_message(frame, splits[1], "Result", "No rows.", Color::DarkGray);
            return;
        }

        let widths: Vec<Constraint> = self.columns.iter().map(|_| Constraint::Min(12)).collect();
        let header: Vec<&str> = self.columns.iter().map(String::as_str).collect();
        let rows: Vec<Row> = self
            .rows
            .iter()
            .map(|r| Row::new(r.iter().map(|c| Cell::from(c.clone())).collect::<Vec<_>>()))
            .collect();

        frame.render_stateful_widget(
            Table::new(rows, widths)
                .header(header_row(&header))
                .block(
                    Block::default()
                        .title(format!(" Result ({} rows) ", self.rows.len()))
                        .borders(Borders::ALL),
                )
                .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
            splits[1],
            &mut self.state,
        );
    }
}

// ── Shared rendering and JSON helpers ───────────────────────────────

fn header_row(labels: &[&str]) -> Row<'static> {
    Row::new(
        labels
            .iter()
            .map(|l| Cell::from(l.to_string()).style(Style::default().bold()))
            .collect::<Vec<_>>(),
    )
}

fn draw_message(frame: &mut Frame, area: Rect, title: &str, body: &str, color: Color) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {body}"),
            Style::default().fg(color),
        )))
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL),
        ),
        area,
    );
}

fn rate_color(rate: f64) -> Color {
    if rate > 0.05 {
        Color::Red
    } else if rate > 0.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn alert_state_color(state: &str) -> Color {
    match state {
        "firing" => Color::Red,
        "pending" => Color::Yellow,
        _ => Color::Green,
    }
}

fn short_time(value: &str) -> String {
    value
        .split('T')
        .nth(1)
        .unwrap_or(value)
        .trim_end_matches('Z')
        .chars()
        .take(12)
        .collect()
}

fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn num(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Scale a `1h`-style window by a factor, for deriving a baseline from the
/// current window. Falls back to the input when it cannot be parsed, which
/// makes the baseline equal the window — a comparison that finds nothing,
/// rather than one that silently compares against the wrong span of time.
fn multiply_window(window: &str, factor: u32) -> String {
    let digits: String = window.chars().take_while(char::is_ascii_digit).collect();
    let unit: String = window.chars().skip_while(char::is_ascii_digit).collect();
    match digits.parse::<u32>() {
        Ok(n) if !unit.is_empty() => format!("{}{}", n * factor, unit),
        _ => window.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_baseline_window_is_a_multiple_of_the_current_one() {
        assert_eq!(multiply_window("1h", 4), "4h");
        assert_eq!(multiply_window("30m", 4), "120m");
        assert_eq!(multiply_window("7d", 2), "14d");
    }

    #[test]
    fn an_unparseable_window_compares_against_itself() {
        // Guessing a baseline from a window we could not read would compare
        // against the wrong span of time and report confident nonsense.
        assert_eq!(multiply_window("", 4), "");
        assert_eq!(multiply_window("hour", 4), "hour");
        assert_eq!(multiply_window("12", 4), "12");
    }

    #[test]
    fn json_helpers_tolerate_missing_and_mistyped_fields() {
        // Every panel reads server JSON positionally by key; a shape change
        // should leave a blank cell, never panic the whole TUI.
        let value = serde_json::json!({"name": "api", "count": 3, "rate": null});
        assert_eq!(text(&value, "name"), "api");
        assert_eq!(text(&value, "missing"), "");
        assert_eq!(text(&value, "rate"), "");
        assert_eq!(text(&value, "count"), "3");
        assert_eq!(num(&value, "count"), 3.0);
        assert_eq!(num(&value, "missing"), 0.0);
        assert!(array(&value, "missing").is_empty());
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut panels = Panels::new("1h");
        panels.topology.edges = vec![serde_json::json!({}), serde_json::json!({})];
        // No selection yet: down picks the first row, not the second.
        panels.move_selection(Panel::Topology, 1);
        assert_eq!(panels.topology.state.selected(), Some(0));
        panels.move_selection(Panel::Topology, 1);
        assert_eq!(panels.topology.state.selected(), Some(1));
        panels.move_selection(Panel::Topology, 1);
        assert_eq!(
            panels.topology.state.selected(),
            Some(1),
            "clamped at the end"
        );
        panels.move_selection(Panel::Topology, -5);
        assert_eq!(
            panels.topology.state.selected(),
            Some(0),
            "clamped at the start"
        );
    }

    #[test]
    fn an_empty_panel_has_no_selection() {
        let mut panels = Panels::new("1h");
        panels.move_selection(Panel::Clusters, 1);
        assert_eq!(panels.clusters.state.selected(), None);
        assert_eq!(panels.selected_trace_id(Panel::Clusters), None);
    }

    #[test]
    fn a_review_request_is_answered_only_when_an_answer_references_it() {
        // The queue's state is derived, because comments are append-only and an
        // answer is a separate comment. Getting this wrong would either hide
        // open questions or show answered ones forever. The fixtures mirror
        // what the server actually returns: the body JSON has no trace_id, the
        // comment record does.
        let comments = serde_json::json!({"comments": [
            {"trace_id": "t1", "body": r#"{"kind":"review_request","review_id":"r1","question":"q1"}"#},
            {"trace_id": "t2", "body": r#"{"kind":"review_request","review_id":"r2","question":"q2"}"#},
            {"trace_id": "t2", "body": r#"{"kind":"review_answer","review_id":"r2","answer":"yes"}"#},
            {"trace_id": "t3", "body": "not json at all"},
        ]});
        let reviews = derive_reviews(&array(&comments, "comments"));

        assert_eq!(reviews.len(), 2, "the non-JSON comment is skipped");
        let r1 = reviews.iter().find(|r| r.review_id == "r1").unwrap();
        assert_eq!(r1.state, "open");
        assert_eq!(r1.answer, None);
        let r2 = reviews.iter().find(|r| r.review_id == "r2").unwrap();
        assert_eq!(r2.state, "answered");
        assert_eq!(r2.answer.as_deref(), Some("yes"));
    }

    #[test]
    fn a_review_row_takes_its_trace_from_the_comment_record() {
        // `tael review request` posts a body without a trace_id; the server
        // stamps the comment record. Reading only the body left every Trace
        // cell empty and made enter-to-open (the point of the panel) a no-op.
        let comments = serde_json::json!({"comments": [
            {"trace_id": "trace-on-comment",
             "body": r#"{"kind":"review_request","review_id":"r1","question":"q1"}"#},
            {"body": r#"{"kind":"review_request","review_id":"r2","trace_id":"trace-in-body","question":"q2"}"#},
        ]});
        let reviews = derive_reviews(&array(&comments, "comments"));

        let r1 = reviews.iter().find(|r| r.review_id == "r1").unwrap();
        assert_eq!(r1.trace_id.as_deref(), Some("trace-on-comment"));
        // A body-side trace_id still counts when the record has none.
        let r2 = reviews.iter().find(|r| r.review_id == "r2").unwrap();
        assert_eq!(r2.trace_id.as_deref(), Some("trace-in-body"));
    }

    #[test]
    fn open_reviews_sort_before_answered_ones() {
        let comments = serde_json::json!({"comments": [
            {"trace_id": "t1", "body": r#"{"kind":"review_request","review_id":"r1","question":"q1"}"#},
            {"trace_id": "t1", "body": r#"{"kind":"review_answer","review_id":"r1","answer":"yes"}"#},
            {"trace_id": "t2", "body": r#"{"kind":"review_request","review_id":"r2","question":"q2"}"#},
        ]});
        let reviews = derive_reviews(&array(&comments, "comments"));
        assert_eq!(reviews[0].review_id, "r2", "the open question leads");
        assert_eq!(reviews[1].state, "answered");
    }

    #[test]
    fn an_error_payload_is_reported_rather_than_rendered_as_empty() {
        // The failure this guards: `query_sql` does not raise on a 400, so a
        // build with no SQL engine returns Ok(json) with an `error` key. Read
        // as data, that is an empty result set and the panel says "no rows".
        let mut fetch = Fetch::default();
        let refusal = serde_json::json!({"error": "this build has no SQL engine"});
        assert!(fetch.record(Ok(refusal)).is_none());
        assert_eq!(fetch.error.as_deref(), Some("this build has no SQL engine"));

        // And a later success clears it, so a fixed server stops showing a
        // stale failure.
        assert!(fetch.record(Ok(serde_json::json!({"rows": []}))).is_some());
        assert_eq!(fetch.error, None);
    }

    /// Render a panel into an off-screen terminal and return what it drew.
    ///
    /// Layout is the part of a TUI that fails at runtime rather than at compile
    /// time — a constraint that overflows its area panics inside `draw`, on the
    /// user's terminal, with the alternate screen still active. Rendering every
    /// panel here means that happens in CI instead.
    fn rendered(panels: &mut Panels, panel: Panel) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal
            .draw(|frame| panels.draw(frame, frame.area(), panel))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn every_panel_renders_when_it_has_no_data() {
        // The state a panel is in the moment it is opened, before its first
        // fetch returns. Each one has to say something rather than draw an
        // empty box the user cannot interpret.
        let mut panels = Panels::new("1h");
        for panel in [
            Panel::Health,
            Panel::Topology,
            Panel::Automation,
            Panel::Clusters,
            Panel::Review,
            Panel::Sql,
        ] {
            let output = rendered(&mut panels, panel);
            assert!(!output.trim().is_empty(), "{panel:?} drew nothing");
        }
    }

    #[test]
    fn every_panel_renders_its_data() {
        let mut panels = Panels::new("1h");
        panels.health.fetch.loaded = true;
        panels.health.summary = Some(serde_json::json!({
            "traces": {"span_count": 3000, "trace_count": 600, "error_count": 120,
                       "error_rate": 0.04, "p50_ms": 150.0, "p95_ms": 284.0, "p99_ms": 296.0},
            "logs": {"total": 10, "error": 2, "warn": 1},
            "top_error_operations": [{"service": "api", "operation": "op-1", "error_count": 10}],
        }));
        panels.health.anomalies = vec![serde_json::json!({
            "service": "api", "kind": "error_rate", "severity": "high",
            "baseline": 0.01, "current": 0.30, "description": "api error rate increased",
        })];
        panels.topology.edges = vec![serde_json::json!({
            "from": "api", "to": "worker", "calls": 800, "errors": 4,
            "error_rate": 0.005, "avg_duration_ms": 148.75,
        })];
        panels.topology.dangling = 7;
        panels.automation.alerts = vec![serde_json::json!({
            "name": "high-errors", "state": "firing", "for_seconds": 60,
            "sinks": [{"webhook": "http://x"}], "query": "rate > 0.05",
        })];
        panels.automation.events = vec![serde_json::json!({
            "rule": "high-errors", "state": "firing", "previous_state": "ok",
            "at": "2026-07-27T03:14:16Z", "matched": [{}],
        })];
        // Status fields carry the server's `RuleStatus` names; reading any
        // other key renders a rule that scores all day as stuck at zero.
        panels.automation.score_rules = vec![serde_json::json!({
            "name": "judge", "sample": 0.1, "command": "./score.sh",
            "status": {"traces_seen": 900, "traces_sampled": 87, "scores_written": 42,
                       "failures": 0, "last_error": null},
        })];
        panels.clusters.clusters = vec![ClusterRow {
            id: 0,
            exemplar: "trace-abc".into(),
            size: 12,
            cohesion: 0.42,
        }];
        panels.review.reviews = vec![ReviewRow {
            review_id: "r1".into(),
            state: "open".into(),
            trace_id: Some("trace-xyz".into()),
            question: "was this refusal correct?".into(),
            answer: None,
        }];
        panels.sql.query = "SELECT 1".into();
        panels.sql.columns = vec!["service".into(), "spans".into()];
        panels.sql.rows = vec![vec!["api".into(), "1000".into()]];

        for (panel, needle) in [
            (Panel::Health, "3000"),
            (Panel::Topology, "worker"),
            (Panel::Automation, "high-errors"),
            (Panel::Clusters, "trace-abc"),
            (Panel::Review, "was this refusal correct?"),
            (Panel::Sql, "SELECT 1"),
        ] {
            let output = rendered(&mut panels, panel);
            assert!(
                output.contains(needle),
                "{panel:?} did not render {needle:?}"
            );
        }

        // The judgement calls each panel exists to surface, spot-checked: a
        // loose cluster says so, and a window that lost parents says how many.
        assert!(rendered(&mut panels, Panel::Clusters).contains("weak"));
        assert!(rendered(&mut panels, Panel::Topology).contains("outside window"));
        // The scoring rule's progress counters, straight off `RuleStatus`.
        let automation = rendered(&mut panels, Panel::Automation);
        assert!(automation.contains("87"), "traces_sampled must render");
        assert!(automation.contains("42"), "scores_written must render");
    }

    #[test]
    fn a_panel_error_replaces_its_body() {
        let mut panels = Panels::new("1h");
        panels.sql.fetch.error = Some("this build has no SQL engine".into());
        panels.sql.rows = vec![vec!["stale".into()]];
        let output = rendered(&mut panels, Panel::Sql);
        assert!(output.contains("no SQL engine"));
        assert!(
            !output.contains("stale"),
            "a failed fetch must not leave the previous result on screen"
        );
    }

    #[test]
    fn sql_will_not_submit_a_blank_query() {
        let mut panels = Panels::new("1h");
        panels.sql_begin_edit();
        assert!(panels.sql_editing());
        assert!(!panels.sql_submit(), "a blank line is not a query");
        for c in "SELECT 1".chars() {
            panels.sql_key(c);
        }
        panels.sql_begin_edit();
        assert!(panels.sql_submit());
        assert!(!panels.sql_editing(), "submitting leaves edit mode");
    }
}
