//! Minimal PromQL subset for instant queries over stored metrics.
//!
//! Supported syntax:
//!   - Bare selector:         `metric_name`
//!   - Labelled selector:     `metric_name{label="value", other!="x"}`
//!   - Rate over range:       `rate(metric_name{...}[5m])`
//!   - Aggregators:           `sum|avg|min|max|count(expr)`
//!                            `sum by (label1,label2) (expr)`
//!                            `sum(expr) by (label1,label2)`
//!
//! Not supported (yet): binary ops, offset, subqueries, `without`, regex
//! matchers (`=~`/`!~`), histogram quantile, time shifting. Anything
//! outside the grammar returns a parse error.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::storage::DuckDbStore;
use crate::storage::models::{MetricPoint, MetricQuery};

// ── AST ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Expr {
    Selector(Selector),
    Rate {
        selector: Selector,
        range_seconds: i64,
    },
    Aggregate {
        op: AggOp,
        by: Vec<String>,
        inner: Box<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct Selector {
    pub metric: String,
    pub matchers: Vec<LabelMatcher>,
}

#[derive(Debug, Clone)]
pub struct LabelMatcher {
    pub name: String,
    pub value: String,
    pub op: MatchOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    Eq,
    NotEq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggOp {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

impl AggOp {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "sum" => Some(Self::Sum),
            "avg" => Some(Self::Avg),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "count" => Some(Self::Count),
            _ => None,
        }
    }

    fn apply(self, values: &[f64]) -> f64 {
        if values.is_empty() {
            return f64::NAN;
        }
        match self {
            Self::Sum => values.iter().sum(),
            Self::Avg => values.iter().sum::<f64>() / values.len() as f64,
            Self::Min => values.iter().cloned().fold(f64::INFINITY, f64::min),
            Self::Max => values.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            Self::Count => values.len() as f64,
        }
    }
}

// ── Parser ──────────────────────────────────────────────────────────

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        self.skip_ws();
        if self.rest().starts_with(lit) {
            self.pos += lit.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, lit: &str) -> Result<()> {
        if self.eat(lit) {
            Ok(())
        } else {
            Err(anyhow!("expected '{lit}' at: {}", self.rest()))
        }
    }

    fn parse_ident(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = self.rest();
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(rest.len());
        if end == 0 {
            bail!("expected identifier at: {rest}");
        }
        let ident = rest[..end].to_string();
        self.pos += end;
        Ok(ident)
    }

    fn parse_string(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = self.rest();
        if !rest.starts_with('"') {
            bail!("expected '\"' at: {rest}");
        }
        let inner = &rest[1..];
        let end = inner
            .find('"')
            .ok_or_else(|| anyhow!("unterminated string"))?;
        let s = inner[..end].to_string();
        self.pos += 1 + end + 1;
        Ok(s)
    }

    fn parse_matchers(&mut self) -> Result<Vec<LabelMatcher>> {
        let mut out = Vec::new();
        if !self.eat("{") {
            return Ok(out);
        }
        loop {
            self.skip_ws();
            if self.eat("}") {
                break;
            }
            let name = self.parse_ident()?;
            let op = if self.eat("=") {
                MatchOp::Eq
            } else if self.eat("!=") {
                MatchOp::NotEq
            } else {
                bail!("expected '=' or '!=' after label name");
            };
            let value = self.parse_string()?;
            out.push(LabelMatcher { name, value, op });
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            self.expect("}")?;
            break;
        }
        Ok(out)
    }

    fn parse_selector(&mut self) -> Result<Selector> {
        let metric = self.parse_ident()?;
        let matchers = self.parse_matchers()?;
        Ok(Selector { metric, matchers })
    }

    fn parse_duration(&mut self) -> Result<i64> {
        self.skip_ws();
        let rest = self.rest();
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            bail!("expected duration number at: {rest}");
        }
        let num: i64 = rest[..end].parse()?;
        self.pos += end;
        let unit = self
            .rest()
            .chars()
            .next()
            .ok_or_else(|| anyhow!("expected duration unit"))?;
        self.pos += unit.len_utf8();
        let seconds = match unit {
            's' => num,
            'm' => num * 60,
            'h' => num * 3600,
            'd' => num * 86400,
            _ => bail!("unknown duration unit '{unit}'"),
        };
        Ok(seconds)
    }

    fn parse_by_clause(&mut self) -> Result<Vec<String>> {
        // caller has already matched the `by` keyword
        self.expect("(")?;
        let mut labels = Vec::new();
        loop {
            self.skip_ws();
            if self.eat(")") {
                break;
            }
            labels.push(self.parse_ident()?);
            self.skip_ws();
            if self.eat(",") {
                continue;
            }
            self.expect(")")?;
            break;
        }
        Ok(labels)
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.skip_ws();
        // Peek an identifier without consuming.
        let save = self.pos;
        let Ok(ident) = self.parse_ident() else {
            self.pos = save;
            bail!("expected expression at: {}", self.rest());
        };
        self.skip_ws();

        // rate(sel[dur])
        if ident == "rate" {
            self.expect("(")?;
            let selector = self.parse_selector()?;
            self.expect("[")?;
            let range_seconds = self.parse_duration()?;
            self.expect("]")?;
            self.expect(")")?;
            return Ok(Expr::Rate {
                selector,
                range_seconds,
            });
        }

        // Aggregators
        if let Some(op) = AggOp::parse(&ident) {
            let mut by_labels: Vec<String> = Vec::new();
            // `sum by (lbl) (expr)` form
            if self.eat("by") {
                by_labels = self.parse_by_clause()?;
                self.expect("(")?;
                let inner = self.parse_expr()?;
                self.expect(")")?;
                return Ok(Expr::Aggregate {
                    op,
                    by: by_labels,
                    inner: Box::new(inner),
                });
            }
            // `sum(expr) [by (lbl)]` form
            self.expect("(")?;
            let inner = self.parse_expr()?;
            self.expect(")")?;
            self.skip_ws();
            if self.eat("by") {
                by_labels = self.parse_by_clause()?;
            }
            return Ok(Expr::Aggregate {
                op,
                by: by_labels,
                inner: Box::new(inner),
            });
        }

        // Bare selector — we already consumed the metric name.
        let matchers = self.parse_matchers()?;
        Ok(Expr::Selector(Selector {
            metric: ident,
            matchers,
        }))
    }
}

pub fn parse(src: &str) -> Result<Expr> {
    let mut p = Parser::new(src);
    let expr = p.parse_expr()?;
    p.skip_ws();
    if !p.rest().is_empty() {
        bail!("unexpected trailing input: {}", p.rest());
    }
    Ok(expr)
}

// ── Evaluator ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub metric: String,
    pub labels: HashMap<String, String>,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

/// Evaluate an instant query. `lookback_seconds` is the time window used
/// when fetching bare selectors (defaults to 5 minutes). `rate(...[dur])`
/// always uses its own bracket duration.
pub fn evaluate(
    store: &DuckDbStore,
    expr: &Expr,
    lookback_seconds: i64,
) -> Result<Vec<Series>> {
    match expr {
        Expr::Selector(sel) => eval_selector_instant(store, sel, lookback_seconds),
        Expr::Rate {
            selector,
            range_seconds,
        } => eval_rate(store, selector, *range_seconds),
        Expr::Aggregate { op, by, inner } => {
            let input = evaluate(store, inner, lookback_seconds)?;
            Ok(aggregate(input, *op, by))
        }
    }
}

fn fetch_points(
    store: &DuckDbStore,
    sel: &Selector,
    lookback_seconds: i64,
) -> Result<Vec<MetricPoint>> {
    let query = MetricQuery {
        service: None,
        name: Some(sel.metric.clone()),
        metric_type: None,
        last_seconds: Some(lookback_seconds),
        // Pull a generous batch — we filter in-memory by label matchers.
        limit: Some(10_000),
    };
    let raw = store.query_metrics(&query)?;
    Ok(raw
        .into_iter()
        .filter(|p| matches_labels(p, &sel.matchers))
        .collect())
}

fn matches_labels(point: &MetricPoint, matchers: &[LabelMatcher]) -> bool {
    for m in matchers {
        // Allow matching on synthetic labels: `service`, `__name__`.
        let actual = if m.name == "service" {
            Some(point.service.as_str())
        } else if m.name == "__name__" {
            Some(point.name.as_str())
        } else {
            point.attributes.get(&m.name).map(|s| s.as_str())
        };
        let matched = match (m.op, actual) {
            (MatchOp::Eq, Some(v)) => v == m.value,
            (MatchOp::Eq, None) => m.value.is_empty(),
            (MatchOp::NotEq, Some(v)) => v != m.value,
            (MatchOp::NotEq, None) => !m.value.is_empty(),
        };
        if !matched {
            return false;
        }
    }
    true
}

/// Group points into series keyed by their full label set, preserving
/// the most-recent sample per series.
fn eval_selector_instant(
    store: &DuckDbStore,
    sel: &Selector,
    lookback_seconds: i64,
) -> Result<Vec<Series>> {
    let points = fetch_points(store, sel, lookback_seconds)?;
    // Group: key = (service, sorted attrs)
    let mut by_key: HashMap<String, MetricPoint> = HashMap::new();
    for p in points {
        let key = series_key(&p);
        match by_key.get(&key) {
            Some(existing) if existing.timestamp >= p.timestamp => {}
            _ => {
                by_key.insert(key, p);
            }
        }
    }
    Ok(by_key.into_values().map(point_to_series).collect())
}

fn eval_rate(
    store: &DuckDbStore,
    sel: &Selector,
    range_seconds: i64,
) -> Result<Vec<Series>> {
    let points = fetch_points(store, sel, range_seconds)?;
    // Group all samples per series.
    let mut grouped: HashMap<String, Vec<MetricPoint>> = HashMap::new();
    for p in points {
        grouped.entry(series_key(&p)).or_default().push(p);
    }
    let mut out = Vec::new();
    for (_, mut samples) in grouped {
        if samples.len() < 2 {
            continue;
        }
        samples.sort_by_key(|p| p.timestamp);
        let first = &samples[0];
        let last = &samples[samples.len() - 1];
        let elapsed = (last.timestamp - first.timestamp).num_seconds();
        if elapsed <= 0 {
            continue;
        }
        // Counter-style rate; negative deltas treated as reset (clamp to 0).
        let delta = (last.value - first.value).max(0.0);
        let rate = delta / elapsed as f64;
        out.push(Series {
            metric: format!("rate({})", last.name),
            labels: series_labels(last),
            value: rate,
            timestamp: last.timestamp,
        });
    }
    Ok(out)
}

fn aggregate(input: Vec<Series>, op: AggOp, by: &[String]) -> Vec<Series> {
    let mut groups: HashMap<String, (HashMap<String, String>, Vec<f64>, DateTime<Utc>)> =
        HashMap::new();

    for s in input {
        let mut group_labels: HashMap<String, String> = HashMap::new();
        if !by.is_empty() {
            for lbl in by {
                if let Some(v) = s.labels.get(lbl) {
                    group_labels.insert(lbl.clone(), v.clone());
                }
            }
        }
        // Key: sorted group labels
        let mut kv: Vec<(&String, &String)> = group_labels.iter().collect();
        kv.sort();
        let key = kv
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");

        let entry = groups
            .entry(key)
            .or_insert_with(|| (group_labels.clone(), Vec::new(), s.timestamp));
        entry.1.push(s.value);
        if s.timestamp > entry.2 {
            entry.2 = s.timestamp;
        }
    }

    groups
        .into_iter()
        .map(|(_, (labels, values, ts))| Series {
            metric: op_name(op).to_string(),
            labels,
            value: op.apply(&values),
            timestamp: ts,
        })
        .collect()
}

fn op_name(op: AggOp) -> &'static str {
    match op {
        AggOp::Sum => "sum",
        AggOp::Avg => "avg",
        AggOp::Min => "min",
        AggOp::Max => "max",
        AggOp::Count => "count",
    }
}

fn series_key(p: &MetricPoint) -> String {
    let mut kv: Vec<(&String, &String)> = p.attributes.iter().collect();
    kv.sort();
    let attrs = kv
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|{}|{}", p.name, p.service, attrs)
}

fn series_labels(p: &MetricPoint) -> HashMap<String, String> {
    let mut labels = p.attributes.clone();
    labels.insert("service".to_string(), p.service.clone());
    labels
}

fn point_to_series(p: MetricPoint) -> Series {
    let labels = series_labels(&p);
    Series {
        metric: p.name,
        labels,
        value: p.value,
        timestamp: p.timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_metric() {
        let e = parse("http_requests").unwrap();
        matches!(e, Expr::Selector(s) if s.metric == "http_requests");
    }

    #[test]
    fn parses_selector_with_labels() {
        let e = parse(r#"http_requests{service="api",method!="GET"}"#).unwrap();
        let Expr::Selector(s) = e else {
            panic!("expected selector");
        };
        assert_eq!(s.metric, "http_requests");
        assert_eq!(s.matchers.len(), 2);
        assert_eq!(s.matchers[0].op, MatchOp::Eq);
        assert_eq!(s.matchers[1].op, MatchOp::NotEq);
    }

    #[test]
    fn parses_rate() {
        let e = parse("rate(http_requests[5m])").unwrap();
        let Expr::Rate { range_seconds, .. } = e else {
            panic!("expected rate");
        };
        assert_eq!(range_seconds, 300);
    }

    #[test]
    fn parses_sum_by_prefix() {
        let e = parse("sum by (service) (http_requests)").unwrap();
        let Expr::Aggregate { op, by, .. } = e else {
            panic!("expected aggregate");
        };
        assert_eq!(op, AggOp::Sum);
        assert_eq!(by, vec!["service".to_string()]);
    }

    #[test]
    fn parses_sum_by_suffix() {
        let e = parse("sum(http_requests) by (service, method)").unwrap();
        let Expr::Aggregate { by, .. } = e else {
            panic!("expected aggregate");
        };
        assert_eq!(by, vec!["service".to_string(), "method".to_string()]);
    }

    #[test]
    fn parses_agg_of_rate() {
        let e = parse("sum by (service) (rate(http_requests{code=\"500\"}[1m]))").unwrap();
        let Expr::Aggregate { inner, .. } = e else {
            panic!("expected aggregate");
        };
        matches!(*inner, Expr::Rate { .. });
    }
}
