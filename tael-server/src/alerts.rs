//! Alert rules and their evaluation.
//!
//! There are no dashboards here, and that is deliberate: the consumer of an
//! alert is an agent, not a wall display. A rule evaluates a PromQL-subset
//! expression on a schedule and, when it holds, emits a JSON event that can be
//! POSTed to a webhook, piped to a command, or read from a `--follow` stream —
//! whichever fits the thing that needs to react.
//!
//! Two design points worth stating:
//!
//! * **`for` before firing.** A rule that fires on a single sample is a rule
//!   that cries wolf on every scrape blip. A rule must hold continuously for
//!   its `for` duration before it fires, which is what makes the event worth
//!   waking something up for.
//! * **Span-derived series.** The signals people actually alert on — error
//!   rate, p95 latency — are computed from spans, not emitted as metrics. The
//!   evaluator synthesizes them (`tael:span_error_rate` and friends) so a
//!   useful alert does not first require instrumenting a metric that
//!   duplicates data tael already has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::promql;
use crate::storage::Store;
use crate::storage::models::{MetricPoint, MetricType};

const RULES_FILE: &str = "alerts.json";

/// Synthetic metric names derived from spans, queryable like any other series.
/// Prefixed `tael:` because `:` is legal in a metric name but not produced by
/// OTLP, so these can never collide with ingested data.
pub const SPAN_ERROR_RATE: &str = "tael:span_error_rate";
pub const SPAN_P95_MS: &str = "tael:span_p95_ms";
pub const SPAN_P99_MS: &str = "tael:span_p99_ms";
pub const SPAN_COUNT: &str = "tael:span_count";
pub const SPAN_ERROR_COUNT: &str = "tael:span_error_count";

/// Where a fired alert is delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Sink {
    /// POST the event as JSON.
    Webhook { url: String },
    /// Run a command with the event on stdin and `TAEL_ALERT_*` in the
    /// environment.
    Exec { command: String },
}

impl Sink {
    /// Parse a `kind=target` sink specification.
    pub fn parse(spec: &str) -> Result<Self> {
        match spec.split_once('=') {
            Some(("webhook", url)) if !url.trim().is_empty() => Ok(Sink::Webhook {
                url: url.trim().to_string(),
            }),
            Some(("exec", command)) if !command.trim().is_empty() => Ok(Sink::Exec {
                command: command.trim().to_string(),
            }),
            _ => bail!("sink must be `webhook=<url>` or `exec=<command>`, got `{spec}`"),
        }
    }
}

/// A stored alert rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub name: String,
    /// PromQL-subset expression, normally ending in a comparison.
    pub query: String,
    /// How long the condition must hold continuously before firing.
    #[serde(default)]
    pub for_seconds: i64,
    /// Lookback used when evaluating the expression.
    #[serde(default = "default_window_seconds")]
    pub window_seconds: i64,
    #[serde(default)]
    pub sinks: Vec<Sink>,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn default_window_seconds() -> i64 {
    300
}

/// Whether a rule is currently satisfied, and since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AlertState {
    /// Not satisfied.
    #[default]
    Ok,
    /// Satisfied, but not yet for long enough to fire.
    Pending,
    /// Satisfied for at least `for_seconds`.
    Firing,
}

/// One state transition, which is what gets delivered and recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvent {
    pub rule: String,
    pub state: AlertState,
    pub previous_state: AlertState,
    pub at: DateTime<Utc>,
    pub query: String,
    /// The series that satisfied the comparison when this fired. Empty on a
    /// resolve, since nothing matches any more.
    pub matched: Vec<MatchedSeries>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedSeries {
    pub metric: String,
    pub labels: HashMap<String, String>,
    pub value: f64,
}

/// How many recent events are kept for `GET /api/v1/alerts/events`. An agent
/// that connects after something fired still needs to see it, but this is a
/// live feed, not an audit log — the durable record is the trace comment
/// written alongside each transition.
const RECENT_EVENT_CAPACITY: usize = 256;

/// Persisted rules plus their in-memory evaluation state.
pub struct AlertStore {
    data_dir: String,
    rules: RwLock<Vec<AlertRule>>,
    /// Per-rule runtime state, keyed by rule name.
    state: RwLock<HashMap<String, RuleState>>,
    /// Recently delivered events, newest last.
    recent: RwLock<std::collections::VecDeque<AlertEvent>>,
    /// Live feed for `tael alerts --follow`.
    tx: tokio::sync::broadcast::Sender<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct RuleState {
    state: AlertState,
    /// When the condition first became true in the current streak. Reset the
    /// moment it stops holding, which is what makes `for` mean "continuously".
    satisfied_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RulesFile {
    #[serde(default)]
    rules: Vec<AlertRule>,
}

impl AlertStore {
    pub fn open(data_dir: &str) -> Result<Self> {
        let rules = Self::load(data_dir)?;
        let (tx, _) = tokio::sync::broadcast::channel(RECENT_EVENT_CAPACITY);
        Ok(Self {
            data_dir: data_dir.to_string(),
            rules: RwLock::new(rules),
            state: RwLock::new(HashMap::new()),
            recent: RwLock::new(std::collections::VecDeque::new()),
            tx,
        })
    }

    pub fn path(data_dir: &str) -> PathBuf {
        Path::new(data_dir).join(RULES_FILE)
    }

    fn load(data_dir: &str) -> Result<Vec<AlertRule>> {
        let path = Self::path(data_dir);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let file: RulesFile = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?;
                Ok(file.rules)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn save(&self) -> Result<()> {
        let rules = self.rules.read().expect("alert rules poisoned").clone();
        let path = Self::path(&self.data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&RulesFile { rules })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<AlertRule> {
        self.rules.read().expect("alert rules poisoned").clone()
    }

    /// Current state of every rule, for `tael alert list`.
    pub fn states(&self) -> HashMap<String, AlertState> {
        self.state
            .read()
            .expect("alert state poisoned")
            .iter()
            .map(|(k, v)| (k.clone(), v.state))
            .collect()
    }

    /// Add a rule. The query is parsed here so a typo is rejected at creation
    /// rather than silently never firing.
    pub fn create(&self, rule: AlertRule) -> Result<()> {
        promql::parse(&rule.query)
            .with_context(|| format!("alert `{}` has an invalid query", rule.name))?;

        let mut rules = self.rules.write().expect("alert rules poisoned");
        if rules.iter().any(|r| r.name == rule.name) {
            bail!("an alert named `{}` already exists", rule.name);
        }
        rules.push(rule);
        drop(rules);
        self.save()
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let mut rules = self.rules.write().expect("alert rules poisoned");
        let before = rules.len();
        rules.retain(|r| r.name != name);
        let removed = rules.len() != before;
        drop(rules);
        if removed {
            self.state
                .write()
                .expect("alert state poisoned")
                .remove(name);
            self.save()?;
        }
        Ok(removed)
    }

    /// Subscribe to the live event feed.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.tx.subscribe()
    }

    /// Recent events, newest first.
    pub fn recent_events(&self, limit: usize) -> Vec<AlertEvent> {
        let recent = self.recent.read().expect("alert events poisoned");
        recent.iter().rev().take(limit).cloned().collect()
    }

    /// Record an event on the ring buffer and publish it to subscribers.
    fn publish(&self, event: &AlertEvent) {
        {
            let mut recent = self.recent.write().expect("alert events poisoned");
            if recent.len() >= RECENT_EVENT_CAPACITY {
                recent.pop_front();
            }
            recent.push_back(event.clone());
        }
        if let Ok(json) = serde_json::to_string(event) {
            // No subscribers is the normal case, not an error.
            let _ = self.tx.send(json);
        }
    }

    /// Evaluate every rule once, returning the state transitions.
    ///
    /// Only transitions are returned: a rule that has been firing for an hour
    /// should not re-deliver on every tick.
    pub fn evaluate_all(&self, store: &dyn Store, now: DateTime<Utc>) -> Vec<AlertEvent> {
        let rules = self.list();
        let mut events = Vec::new();

        for rule in rules {
            let matched = match self.evaluate_rule(store, &rule) {
                Ok(m) => m,
                Err(e) => {
                    // A broken rule must not stop the others from evaluating.
                    tracing::warn!(rule = %rule.name, error = %e, "alert evaluation failed");
                    continue;
                }
            };

            let mut state_map = self.state.write().expect("alert state poisoned");
            let entry = state_map.entry(rule.name.clone()).or_default();
            let previous = entry.state;

            let next = if matched.is_empty() {
                entry.satisfied_since = None;
                AlertState::Ok
            } else {
                let since = *entry.satisfied_since.get_or_insert(now);
                if (now - since).num_seconds() >= rule.for_seconds {
                    AlertState::Firing
                } else {
                    AlertState::Pending
                }
            };
            entry.state = next;
            drop(state_map);

            // Pending is an internal step on the way to firing, not something
            // worth waking a consumer for.
            let worth_reporting = matches!(
                (previous, next),
                (AlertState::Ok | AlertState::Pending, AlertState::Firing)
                    | (AlertState::Firing, AlertState::Ok)
            );
            if worth_reporting {
                let event = AlertEvent {
                    rule: rule.name.clone(),
                    state: next,
                    previous_state: previous,
                    at: now,
                    query: rule.query.clone(),
                    matched: if next == AlertState::Firing {
                        matched
                    } else {
                        Vec::new()
                    },
                    description: rule.description.clone(),
                };
                self.publish(&event);
                events.push(event);
            }
        }

        events
    }

    /// Series currently satisfying a rule's query.
    fn evaluate_rule(&self, store: &dyn Store, rule: &AlertRule) -> Result<Vec<MatchedSeries>> {
        let expr = promql::parse(&rule.query)?;
        let series = promql::evaluate(store, &expr, rule.window_seconds)?;
        Ok(series
            .into_iter()
            // A NaN is "no data", not a threshold crossing; alerting on it
            // would fire every time a series goes quiet.
            .filter(|s| !s.value.is_nan())
            .map(|s| MatchedSeries {
                metric: s.metric,
                labels: s.labels,
                value: s.value,
            })
            .collect())
    }
}

/// Compute the span-derived synthetic series for a window.
///
/// These are written into the metric store before each evaluation pass so an
/// alert can reference error rate and latency without a service first emitting
/// them as metrics — the spans already carry the information.
pub fn span_derived_points(store: &dyn Store, window_seconds: i64) -> Result<Vec<MetricPoint>> {
    let summary = store.query_summary(window_seconds, None)?;
    let now = Utc::now();
    let mut points = Vec::new();

    let mut push = |name: &str, value: f64, service: &str| {
        let mut attributes = HashMap::new();
        attributes.insert("window_seconds".to_string(), window_seconds.to_string());
        points.push(MetricPoint {
            timestamp: now,
            service: service.to_string(),
            name: name.to_string(),
            metric_type: MetricType::Gauge,
            value,
            unit: String::new(),
            attributes,
            histogram: None,
        });
    };

    // Fleet-wide, under a reserved service name so `by (service)` groupings
    // don't accidentally mix the aggregate in with per-service values.
    push(SPAN_ERROR_RATE, summary.traces.error_rate, "tael");
    push(SPAN_P95_MS, summary.traces.p95_ms, "tael");
    push(SPAN_P99_MS, summary.traces.p99_ms, "tael");
    push(SPAN_COUNT, summary.traces.span_count as f64, "tael");
    push(SPAN_ERROR_COUNT, summary.traces.error_count as f64, "tael");

    for svc in &summary.top_services {
        push(SPAN_ERROR_RATE, svc.error_rate, &svc.service);
        push(SPAN_P95_MS, svc.p95_ms, &svc.service);
        push(SPAN_COUNT, svc.span_count as f64, &svc.service);
    }

    Ok(points)
}

/// Deliver an event to a sink. Failures are logged, not propagated: one broken
/// webhook must not stop the other sinks or the evaluation loop.
pub async fn deliver(event: &AlertEvent, sink: &Sink) {
    match sink {
        Sink::Webhook { url } => {
            let client = reqwest::Client::new();
            match client
                .post(url)
                .json(event)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => tracing::warn!(
                    rule = %event.rule, url = %url, status = %resp.status(),
                    "alert webhook returned a non-success status"
                ),
                Err(e) => {
                    tracing::warn!(rule = %event.rule, url = %url, error = %e, "alert webhook failed")
                }
            }
        }
        Sink::Exec { command } => {
            let payload = serde_json::to_string(event).unwrap_or_default();
            let result = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .env("TAEL_ALERT_RULE", &event.rule)
                .env(
                    "TAEL_ALERT_STATE",
                    format!("{:?}", event.state).to_lowercase(),
                )
                .env("TAEL_ALERT_EVENT", &payload)
                .stdin(std::process::Stdio::null())
                .output()
                .await;
            match result {
                Ok(out) if out.status.success() => {}
                Ok(out) => tracing::warn!(
                    rule = %event.rule,
                    status = ?out.status.code(),
                    stderr = %String::from_utf8_lossy(&out.stderr),
                    "alert exec sink exited non-zero"
                ),
                Err(e) => {
                    tracing::warn!(rule = %event.rule, error = %e, "alert exec sink failed to run")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::testing::TestBackend;

    fn rule(name: &str, query: &str, for_seconds: i64) -> AlertRule {
        AlertRule {
            name: name.into(),
            query: query.into(),
            for_seconds,
            window_seconds: 300,
            sinks: Vec::new(),
            description: None,
            created_at: Utc::now(),
        }
    }

    fn gauge(name: &str, service: &str, value: f64) -> MetricPoint {
        MetricPoint {
            timestamp: Utc::now(),
            service: service.into(),
            name: name.into(),
            metric_type: MetricType::Gauge,
            value,
            unit: String::new(),
            attributes: HashMap::new(),
            histogram: None,
        }
    }

    #[test]
    fn sinks_parse_their_specification() {
        assert_eq!(
            Sink::parse("webhook=https://example.test/hook").unwrap(),
            Sink::Webhook {
                url: "https://example.test/hook".into()
            }
        );
        assert_eq!(
            Sink::parse("exec=notify-send hi").unwrap(),
            Sink::Exec {
                command: "notify-send hi".into()
            }
        );
        assert!(Sink::parse("webhook=").is_err());
        assert!(Sink::parse("carrier-pigeon=nearby").is_err());
    }

    #[test]
    fn creating_a_rule_rejects_an_unparseable_query() {
        let dir = tempfile::tempdir().unwrap();
        let store = AlertStore::open(dir.path().to_str().unwrap()).unwrap();
        // Better to fail at creation than to have a rule that silently never
        // fires because nobody re-reads its query.
        assert!(store.create(rule("bad", "sum(((", 0)).is_err());
        assert!(store.list().is_empty());
    }

    #[test]
    fn rules_round_trip_through_disk_and_reject_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let store = AlertStore::open(path).unwrap();
        store.create(rule("errors", "queue_depth > 10", 0)).unwrap();
        assert!(store.create(rule("errors", "queue_depth > 99", 0)).is_err());

        let reopened = AlertStore::open(path).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].query, "queue_depth > 10");

        assert!(reopened.delete("errors").unwrap());
        assert!(!reopened.delete("errors").unwrap());
        assert!(AlertStore::open(path).unwrap().list().is_empty());
    }

    #[test]
    fn a_rule_fires_once_on_crossing_and_resolves_once_on_recovery() {
        let engine = TestBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let alerts = AlertStore::open(dir.path().to_str().unwrap()).unwrap();
        alerts
            .create(rule("deep_queue", "queue_depth > 10", 0))
            .unwrap();

        let now = Utc::now();

        // Below threshold: nothing to report.
        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 3.0)])
            .unwrap();
        assert!(alerts.evaluate_all(engine.backend.as_ref(), now).is_empty());

        // Crossing fires exactly one event.
        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 42.0)])
            .unwrap();
        let events = alerts.evaluate_all(engine.backend.as_ref(), now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].state, AlertState::Firing);
        assert_eq!(events[0].matched[0].value, 42.0);

        // Still firing: no repeat delivery.
        assert!(alerts.evaluate_all(engine.backend.as_ref(), now).is_empty());

        // Recovery reports once, with no matched series.
        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 1.0)])
            .unwrap();
        let events = alerts.evaluate_all(engine.backend.as_ref(), now);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].state, AlertState::Ok);
        assert!(events[0].matched.is_empty());
    }

    #[test]
    fn a_for_duration_must_be_held_continuously() {
        let engine = TestBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let alerts = AlertStore::open(dir.path().to_str().unwrap()).unwrap();
        alerts
            .create(rule("sustained", "queue_depth > 10", 300))
            .unwrap();

        let t0 = Utc::now();
        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 50.0)])
            .unwrap();

        // Satisfied, but not for long enough yet.
        assert!(alerts.evaluate_all(engine.backend.as_ref(), t0).is_empty());
        assert_eq!(alerts.states()["sustained"], AlertState::Pending);

        // A dip resets the streak, so the clock starts over.
        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 1.0)])
            .unwrap();
        assert!(
            alerts
                .evaluate_all(engine.backend.as_ref(), t0 + chrono::Duration::seconds(120))
                .is_empty()
        );
        assert_eq!(alerts.states()["sustained"], AlertState::Ok);

        engine
            .backend
            .insert_metrics(&[gauge("queue_depth", "worker", 50.0)])
            .unwrap();
        assert!(
            alerts
                .evaluate_all(engine.backend.as_ref(), t0 + chrono::Duration::seconds(180))
                .is_empty(),
            "the streak restarted, so 180s in is only 0s of continuous breach"
        );

        // Held continuously past `for`: now it fires.
        let events =
            alerts.evaluate_all(engine.backend.as_ref(), t0 + chrono::Duration::seconds(500));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].state, AlertState::Firing);
    }

    #[test]
    fn span_derived_series_are_available_without_instrumenting_metrics() {
        use crate::storage::models::{Span, SpanKind, SpanStatus};

        let engine = TestBackend::new();
        let now = Utc::now();
        let span = |id: &str, status: SpanStatus| Span {
            trace_id: format!("t{id}"),
            span_id: format!("s{id}"),
            parent_span_id: None,
            service: "api".into(),
            operation: "GET /".into(),
            start_time: now,
            end_time: now,
            duration_ms: 100.0,
            status,
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        };
        engine
            .backend
            .insert_spans(&[
                span("1", SpanStatus::Ok),
                span("2", SpanStatus::Error),
                span("3", SpanStatus::Ok),
                span("4", SpanStatus::Ok),
            ])
            .unwrap();

        let points = span_derived_points(engine.backend.as_ref(), 3600).unwrap();
        let fleet_error_rate = points
            .iter()
            .find(|p| p.name == SPAN_ERROR_RATE && p.service == "tael")
            .expect("fleet-wide error rate should be derived");
        assert!(
            (fleet_error_rate.value - 0.25).abs() < 1e-9,
            "1 error in 4 spans, got {}",
            fleet_error_rate.value
        );

        // The derived points are ordinary metrics, so a rule can alert on them.
        engine.backend.insert_metrics(&points).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let alerts = AlertStore::open(dir.path().to_str().unwrap()).unwrap();
        alerts
            .create(rule(
                "errors",
                "tael:span_error_rate{service=\"tael\"} > 0.1",
                0,
            ))
            .unwrap();
        let events = alerts.evaluate_all(engine.backend.as_ref(), now);
        assert_eq!(events.len(), 1, "derived error rate should trip the rule");
    }
}
