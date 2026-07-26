//! Online scoring — evaluating production traffic, not just eval runs.
//!
//! `tael eval run` scores a fixed set of golden cases. That answers "did this
//! change break what I already knew about", but says nothing about the traffic
//! actually arriving. Online scoring closes that gap: a rule samples matching
//! production traces and runs a scorer command against each one, writing the
//! result as an ordinary `tael_eval_score` metric point so it trends, alerts,
//! and compares exactly like an offline score does.
//!
//! The scorer is an arbitrary command — a code check, an LLM judge, whatever
//! the user wants. tael schedules it and records the result; it never calls a
//! model provider itself. That boundary is deliberate: holding provider
//! credentials is the exact trust surface a local-first tool advertises not
//! having, and it is what makes "we never see your keys" true rather than
//! aspirational.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::storage::Store;
use crate::storage::models::{Span, TraceQuery};

const RULES_FILE: &str = "score_rules.json";

/// A rule that scores sampled production traces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreRule {
    pub name: String,
    /// Fraction of matching traces to score, 0.0–1.0. Scoring every trace is
    /// usually both unnecessary and expensive; a few percent is enough to
    /// trend, and the sample rate is recorded on every point so the numbers
    /// stay interpretable.
    pub sample: f64,
    /// Filters selecting which traces this rule applies to.
    #[serde(default)]
    pub matcher: TraceMatcher,
    /// Command run once per sampled trace, with `TAEL_EVAL_*` in the
    /// environment. Must print one JSON object per line on stdout, each with
    /// at least `metric` and `value`.
    pub command: String,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Which traces a rule applies to. Deliberately the same vocabulary as
/// `tael query traces`, so a rule can be developed by narrowing a query until
/// it selects the right traffic and then pasted in.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraceMatcher {
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub min_duration_ms: Option<f64>,
    /// Span attribute equality filters, ANDed.
    #[serde(default)]
    pub attributes: Vec<(String, String)>,
}

impl TraceMatcher {
    /// Parse `key=value` selectors: bare keys map onto the built-in fields,
    /// and `attribute:foo=bar` onto span attributes.
    pub fn parse(specs: &[String]) -> Result<Self> {
        let mut m = Self::default();
        for spec in specs {
            let (key, value) = spec
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("match `{spec}` must be key=value"))?;
            match key.trim() {
                "service" => m.service = Some(value.to_string()),
                "operation" => m.operation = Some(value.to_string()),
                "status" => m.status = Some(value.to_string()),
                "min_duration_ms" => m.min_duration_ms = Some(value.parse()?),
                other => match other.strip_prefix("attribute:") {
                    Some(attr) if !attr.is_empty() => {
                        m.attributes.push((attr.to_string(), value.to_string()))
                    }
                    _ => bail!(
                        "unknown match key `{other}` \
                         (use service, operation, status, min_duration_ms, or attribute:<key>)"
                    ),
                },
            }
        }
        Ok(m)
    }

    /// The trace query that selects candidate spans for this rule.
    pub fn to_query(&self, last_seconds: i64, limit: u32) -> TraceQuery {
        TraceQuery {
            service: self.service.clone(),
            operation: self.operation.clone(),
            status: self.status.clone(),
            min_duration_ms: self.min_duration_ms,
            max_duration_ms: None,
            last_seconds: Some(last_seconds),
            limit: Some(limit),
            attributes: self.attributes.clone(),
            text: None,
        }
    }
}

/// Per-rule progress, so `score rule status` can report lag and failures
/// rather than leaving an agent guessing whether scoring is running at all.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleStatus {
    pub traces_seen: u64,
    pub traces_sampled: u64,
    pub scores_written: u64,
    pub failures: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RulesFile {
    #[serde(default)]
    rules: Vec<ScoreRule>,
}

pub struct ScoreRuleStore {
    data_dir: String,
    rules: RwLock<Vec<ScoreRule>>,
    status: RwLock<HashMap<String, RuleStatus>>,
    /// Traces already scored, keyed by `(rule, trace_id)` so two rules score
    /// the same trace independently — a faithfulness judge and a tone judge
    /// are different questions about the same request. Bounded, because this
    /// is a sampling mechanism and re-scoring an old trace after eviction is
    /// harmless.
    scored: RwLock<std::collections::VecDeque<(String, String)>>,
}

/// How many recently-scored trace IDs to remember. Sized well above one pass's
/// candidate window so nothing is rescored within a normal interval.
const SCORED_MEMORY: usize = 10_000;

impl ScoreRuleStore {
    pub fn open(data_dir: &str) -> Result<Self> {
        Ok(Self {
            data_dir: data_dir.to_string(),
            rules: RwLock::new(Self::load(data_dir)?),
            status: RwLock::new(HashMap::new()),
            scored: RwLock::new(std::collections::VecDeque::new()),
        })
    }

    pub fn path(data_dir: &str) -> PathBuf {
        Path::new(data_dir).join(RULES_FILE)
    }

    fn load(data_dir: &str) -> Result<Vec<ScoreRule>> {
        match std::fs::read(Self::path(data_dir)) {
            Ok(bytes) => Ok(serde_json::from_slice::<RulesFile>(&bytes)
                .context("parsing score rules")?
                .rules),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e).context("reading score rules"),
        }
    }

    fn save(&self) -> Result<()> {
        let rules = self.rules.read().expect("score rules poisoned").clone();
        let path = Self::path(&self.data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&RulesFile { rules })?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn list(&self) -> Vec<ScoreRule> {
        self.rules.read().expect("score rules poisoned").clone()
    }

    pub fn status(&self) -> HashMap<String, RuleStatus> {
        self.status.read().expect("score status poisoned").clone()
    }

    pub fn create(&self, rule: ScoreRule) -> Result<()> {
        if !(0.0..=1.0).contains(&rule.sample) {
            bail!(
                "sample must be between 0 and 1 (got {}); 0.05 means 5% of matching traces",
                rule.sample
            );
        }
        if rule.command.trim().is_empty() {
            bail!("a score rule needs a --cmd to run");
        }
        let mut rules = self.rules.write().expect("score rules poisoned");
        if rules.iter().any(|r| r.name == rule.name) {
            bail!("a score rule named `{}` already exists", rule.name);
        }
        rules.push(rule);
        drop(rules);
        self.save()
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        let mut rules = self.rules.write().expect("score rules poisoned");
        let before = rules.len();
        rules.retain(|r| r.name != name);
        let removed = rules.len() != before;
        drop(rules);
        if removed {
            self.status
                .write()
                .expect("score status poisoned")
                .remove(name);
            self.save()?;
        }
        Ok(removed)
    }

    /// Whether this rule has already scored this trace in a recent pass.
    fn already_scored(&self, rule: &str, trace_id: &str) -> bool {
        self.scored
            .read()
            .expect("scored set poisoned")
            .iter()
            .any(|(r, t)| r == rule && t == trace_id)
    }

    fn remember_scored(&self, rule: &str, trace_id: &str) {
        let mut scored = self.scored.write().expect("scored set poisoned");
        if scored.len() >= SCORED_MEMORY {
            scored.pop_front();
        }
        scored.push_back((rule.to_string(), trace_id.to_string()));
    }

    fn record<F: FnOnce(&mut RuleStatus)>(&self, rule: &str, f: F) {
        let mut status = self.status.write().expect("score status poisoned");
        f(status.entry(rule.to_string()).or_default());
    }

    /// Select the traces a rule should score in this pass.
    ///
    /// Sampling is deterministic in the trace ID rather than random, so the
    /// same trace is either always in the sample or always out. A random draw
    /// would let a retried pass score the same trace twice and skip another
    /// entirely, biasing the very numbers the rule exists to produce.
    pub fn select_candidates(
        &self,
        store: &dyn Store,
        rule: &ScoreRule,
        window_seconds: i64,
    ) -> Result<Vec<Span>> {
        let spans = store.query_traces(&rule.matcher.to_query(window_seconds, 1000))?;

        // One entry per trace: scorers operate on a trace, not a span.
        let mut roots: HashMap<String, Span> = HashMap::new();
        for span in spans {
            roots.entry(span.trace_id.clone()).or_insert(span);
        }

        let mut seen = 0u64;
        let mut selected = Vec::new();
        for (trace_id, span) in roots {
            seen += 1;
            if self.already_scored(&rule.name, &trace_id) {
                continue;
            }
            if sample_fraction(&trace_id) < rule.sample {
                selected.push(span);
            }
        }
        self.record(&rule.name, |s| s.traces_seen += seen);
        Ok(selected)
    }

    /// Note that a trace was sampled and its scores recorded.
    pub fn mark_scored(&self, rule: &str, trace_id: &str, scores_written: u64) {
        self.remember_scored(rule, trace_id);
        self.record(rule, |s| {
            s.traces_sampled += 1;
            s.scores_written += scores_written;
            s.last_run_at = Some(Utc::now());
        });
    }

    /// Note that scoring a trace failed.
    pub fn mark_failed(&self, rule: &str, trace_id: &str, error: String) {
        // Remembered anyway: a scorer that fails on one trace will keep failing
        // on it, and retrying forever would starve everything else.
        self.remember_scored(rule, trace_id);
        self.record(rule, |s| {
            s.failures += 1;
            s.last_run_at = Some(Utc::now());
            s.last_error = Some(error);
        });
    }
}

/// Map a trace ID onto a stable value in [0, 1) for sampling decisions.
fn sample_fraction(trace_id: &str) -> f64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(trace_id.as_bytes());
    // The first 8 bytes give plenty of resolution for a percentage.
    let n = u64::from_be_bytes(digest[..8].try_into().unwrap_or([0; 8]));
    n as f64 / u64::MAX as f64
}

/// One score emitted by a scorer command.
#[derive(Debug, Clone, Deserialize)]
pub struct ScoreLine {
    pub metric: String,
    pub value: f64,
    #[serde(default)]
    pub rationale: Option<String>,
}

/// Run a scorer command for one trace and parse its output.
///
/// The environment matches `tael eval run` so the same script works offline
/// against golden cases and online against production traffic — which is the
/// whole point of scoring both with one mechanism.
pub async fn run_scorer(rule: &ScoreRule, span: &Span) -> Result<Vec<ScoreLine>> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&rule.command)
        .env("TAEL_EVAL_RULE", &rule.name)
        .env("TAEL_EVAL_TRACE_ID", &span.trace_id)
        .env("TAEL_EVAL_SPAN_ID", &span.span_id)
        .env("TAEL_EVAL_SERVICE", &span.service)
        .env("TAEL_EVAL_OPERATION", &span.operation)
        .env("TAEL_EVAL_CASE_ID", &span.trace_id)
        .env("TAEL_EVAL_ONLINE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .with_context(|| format!("running scorer for rule `{}`", rule.name))?;

    if !output.status.success() {
        bail!(
            "scorer exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut scores = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed: ScoreLine = serde_json::from_str(line)
            .with_context(|| format!("parsing scorer output line {}", i + 1))?;
        scores.push(parsed);
    }
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::models::{SpanKind, SpanStatus};
    use crate::storage::testing::TestBackend;

    fn rule(name: &str, sample: f64, matcher: TraceMatcher) -> ScoreRule {
        ScoreRule {
            name: name.into(),
            sample,
            matcher,
            command: "echo '{\"metric\":\"faithfulness\",\"value\":1.0}'".into(),
            description: None,
            created_at: Utc::now(),
        }
    }

    fn span(trace: &str, service: &str) -> Span {
        let now = Utc::now();
        Span {
            trace_id: trace.into(),
            span_id: format!("s{trace}"),
            parent_span_id: None,
            service: service.into(),
            operation: "chat".into(),
            start_time: now,
            end_time: now,
            duration_ms: 10.0,
            status: SpanStatus::Ok,
            attributes: HashMap::new(),
            events: vec![],
            kind: SpanKind::Server,
            llm: None,
        }
    }

    #[test]
    fn matchers_parse_the_query_vocabulary() {
        let m = TraceMatcher::parse(&[
            "service=agent-api".into(),
            "status=error".into(),
            "attribute:gen_ai.system=anthropic".into(),
        ])
        .unwrap();
        assert_eq!(m.service.as_deref(), Some("agent-api"));
        assert_eq!(m.status.as_deref(), Some("error"));
        assert_eq!(
            m.attributes,
            vec![("gen_ai.system".to_string(), "anthropic".to_string())]
        );
        assert!(TraceMatcher::parse(&["nonsense".into()]).is_err());
        assert!(TraceMatcher::parse(&["colour=blue".into()]).is_err());
    }

    #[test]
    fn sampling_is_stable_for_a_given_trace() {
        // The same trace must always land on the same side of the threshold,
        // or a retried pass would score it twice and skip another.
        let a = sample_fraction("abc123");
        let b = sample_fraction("abc123");
        assert_eq!(a, b);
        assert!((0.0..1.0).contains(&a));
        assert_ne!(sample_fraction("abc123"), sample_fraction("def456"));
    }

    #[test]
    fn sampling_approximates_the_requested_rate() {
        let sampled = (0..5000)
            .filter(|i| sample_fraction(&format!("trace-{i}")) < 0.10)
            .count();
        // Wide tolerance — this asserts the hash is uniform enough to be a
        // sampler, not that it hits exactly 10%.
        assert!(
            (350..650).contains(&sampled),
            "expected roughly 500 of 5000 at 10%, got {sampled}"
        );
    }

    #[test]
    fn rules_validate_their_sample_rate_and_command() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScoreRuleStore::open(dir.path().to_str().unwrap()).unwrap();

        let mut bad = rule("bad", 5.0, TraceMatcher::default());
        assert!(store.create(bad.clone()).is_err(), "5.0 is not a fraction");
        bad.sample = 0.5;
        bad.command = "  ".into();
        assert!(store.create(bad).is_err(), "a rule needs a command");
        assert!(store.list().is_empty());
    }

    #[test]
    fn rules_round_trip_and_reject_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let store = ScoreRuleStore::open(path).unwrap();
        store
            .create(rule("faith", 0.05, TraceMatcher::default()))
            .unwrap();
        assert!(
            store
                .create(rule("faith", 0.1, TraceMatcher::default()))
                .is_err()
        );

        let reopened = ScoreRuleStore::open(path).unwrap();
        assert_eq!(reopened.list().len(), 1);
        assert_eq!(reopened.list()[0].sample, 0.05);
        assert!(reopened.delete("faith").unwrap());
        assert!(ScoreRuleStore::open(path).unwrap().list().is_empty());
    }

    #[test]
    fn candidates_respect_the_matcher_and_are_not_rescored() {
        let engine = TestBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = ScoreRuleStore::open(dir.path().to_str().unwrap()).unwrap();

        let mut spans: Vec<Span> = (0..200)
            .map(|i| span(&format!("t{i}"), "agent-api"))
            .collect();
        spans.extend((0..50).map(|i| span(&format!("other{i}"), "unrelated")));
        engine.backend.insert_spans(&spans).unwrap();

        // Sample everything so the matcher, not the sampler, decides.
        let r = rule(
            "all",
            1.0,
            TraceMatcher::parse(&["service=agent-api".into()]).unwrap(),
        );
        let first = store
            .select_candidates(engine.backend.as_ref(), &r, 3600)
            .unwrap();
        assert_eq!(first.len(), 200, "only the matching service is scored");
        assert!(first.iter().all(|s| s.service == "agent-api"));

        for s in &first {
            store.mark_scored(&r.name, &s.trace_id, 1);
        }
        let second = store
            .select_candidates(engine.backend.as_ref(), &r, 3600)
            .unwrap();
        assert!(
            second.is_empty(),
            "already-scored traces must not be scored again"
        );
    }

    #[test]
    fn two_rules_score_the_same_trace_independently() {
        // A faithfulness judge and a tone judge ask different questions about
        // the same request; one must not consume the other's candidates.
        let engine = TestBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = ScoreRuleStore::open(dir.path().to_str().unwrap()).unwrap();
        let spans: Vec<Span> = (0..20).map(|i| span(&format!("t{i}"), "api")).collect();
        engine.backend.insert_spans(&spans).unwrap();

        let faith = rule("faithfulness", 1.0, TraceMatcher::default());
        let tone = rule("tone", 1.0, TraceMatcher::default());

        let first = store
            .select_candidates(engine.backend.as_ref(), &faith, 3600)
            .unwrap();
        assert_eq!(first.len(), 20);
        for s in &first {
            store.mark_scored(&faith.name, &s.trace_id, 1);
        }

        let second = store
            .select_candidates(engine.backend.as_ref(), &tone, 3600)
            .unwrap();
        assert_eq!(
            second.len(),
            20,
            "the second rule should still see every trace"
        );
    }

    #[test]
    fn a_low_sample_rate_selects_a_small_fraction() {
        let engine = TestBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = ScoreRuleStore::open(dir.path().to_str().unwrap()).unwrap();
        let spans: Vec<Span> = (0..1000).map(|i| span(&format!("t{i}"), "api")).collect();
        engine.backend.insert_spans(&spans).unwrap();

        let r = rule("sampled", 0.05, TraceMatcher::default());
        let selected = store
            .select_candidates(engine.backend.as_ref(), &r, 3600)
            .unwrap();
        assert!(
            (10..120).contains(&selected.len()),
            "expected roughly 5% of 1000, got {}",
            selected.len()
        );
    }

    #[tokio::test]
    async fn a_scorer_command_produces_score_lines() {
        let r = ScoreRule {
            command: "printf '{\"metric\":\"faithfulness\",\"value\":0.9}\\n\
                      {\"metric\":\"tone\",\"value\":1.0,\"rationale\":\"fine\"}\\n'"
                .into(),
            ..rule("judge", 1.0, TraceMatcher::default())
        };
        let scores = run_scorer(&r, &span("t1", "api")).await.unwrap();
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0].metric, "faithfulness");
        assert_eq!(scores[0].value, 0.9);
        assert_eq!(scores[1].rationale.as_deref(), Some("fine"));
    }

    #[tokio::test]
    async fn the_scorer_sees_the_same_env_as_an_offline_eval() {
        let r = ScoreRule {
            command: "printf '{\"metric\":\"%s\",\"value\":1}\\n' \"$TAEL_EVAL_TRACE_ID\"".into(),
            ..rule("env", 1.0, TraceMatcher::default())
        };
        let scores = run_scorer(&r, &span("trace-abc", "api")).await.unwrap();
        assert_eq!(scores[0].metric, "trace-abc");
    }

    #[tokio::test]
    async fn a_failing_scorer_reports_its_stderr() {
        let r = ScoreRule {
            command: "echo 'model unreachable' >&2; exit 3".into(),
            ..rule("broken", 1.0, TraceMatcher::default())
        };
        let err = run_scorer(&r, &span("t1", "api")).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exited 3"), "{msg}");
        assert!(msg.contains("model unreachable"), "{msg}");
    }
}
