//! Minimal PromQL subset for instant queries over stored metrics.
//!
//! Supported syntax:
//!   - Bare selector:         `metric_name`
//!   - Labelled selector:     `metric_name{label="value", other!="x"}`
//!   - Rate over range:       `rate(metric_name{...}[5m])`
//!   - Aggregators:           `sum|avg|min|max|count(expr)`
//!                            `sum by (label1,label2) (expr)`
//!                            `sum(expr) by (label1,label2)`
//!   - Histogram quantile:    `histogram_quantile(0.95, metric{...})`
//!                            `histogram_quantile(0.99, metric) by (service)`
//!
//! Note that `histogram_quantile` takes a metric selector, not a fan of
//! `le`-labelled bucket series as in Prometheus — tael stores each data point's
//! whole bucket layout on the point. See [`eval_histogram_quantile`].
//!
//! Not supported (yet): binary ops, offset, subqueries, `without`, regex
//! matchers (`=~`/`!~`), time shifting. Anything outside the grammar returns a
//! parse error.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::storage::Store;
use crate::storage::models::{MetricPoint, MetricQuery, Temporality};

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
    /// `histogram_quantile(phi, selector)` — estimate a quantile from stored
    /// bucket layouts. See [`eval_histogram_quantile`] for how this differs
    /// from Prometheus's `le`-label formulation.
    HistogramQuantile {
        phi: f64,
        selector: Selector,
        /// Label set to merge histograms across before computing. Empty means
        /// one result per distinct label set.
        by: Vec<String>,
    },
    /// `<expr> <op> <scalar>` — keep only the series satisfying the comparison.
    /// Top level only; this exists so an alert rule can be written as one
    /// expression, not so series can be compared to each other.
    Compare {
        inner: Box<Expr>,
        op: CompareOp,
        threshold: f64,
    },
}

/// Scalar comparison used by [`Expr::Compare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Gt,
    Gte,
    Lt,
    Lte,
    Eq,
    NotEq,
}

impl CompareOp {
    pub fn test(self, value: f64, threshold: f64) -> bool {
        match self {
            CompareOp::Gt => value > threshold,
            CompareOp::Gte => value >= threshold,
            CompareOp::Lt => value < threshold,
            CompareOp::Lte => value <= threshold,
            CompareOp::Eq => (value - threshold).abs() < f64::EPSILON,
            CompareOp::NotEq => (value - threshold).abs() >= f64::EPSILON,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CompareOp::Gt => ">",
            CompareOp::Gte => ">=",
            CompareOp::Lt => "<",
            CompareOp::Lte => "<=",
            CompareOp::Eq => "==",
            CompareOp::NotEq => "!=",
        }
    }
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

    /// Parse a metric or label name.
    ///
    /// `.` is accepted in addition to Prometheus's character set because tael
    /// ingests OpenTelemetry natively and semantic-convention metric names are
    /// dotted (`http.server.request.duration`). Rejecting them would make the
    /// PromQL surface unusable for exactly the metrics tael is built to hold.
    fn parse_ident(&mut self) -> Result<String> {
        self.skip_ws();
        let rest = self.rest();
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':' || c == '.'))
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

    /// Consume a comparison operator if one is next. Two-character forms are
    /// tried first so `>=` is not read as `>` followed by `=`.
    fn parse_compare_op(&mut self) -> Option<CompareOp> {
        for (lit, op) in [
            (">=", CompareOp::Gte),
            ("<=", CompareOp::Lte),
            ("==", CompareOp::Eq),
            ("!=", CompareOp::NotEq),
            (">", CompareOp::Gt),
            ("<", CompareOp::Lt),
        ] {
            if self.eat(lit) {
                return Some(op);
            }
        }
        None
    }

    /// Parse a bare float literal (currently only a quantile argument).
    fn parse_number(&mut self) -> Result<f64> {
        self.skip_ws();
        let rest = self.rest();
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if end == 0 {
            bail!("expected a number at: {rest}");
        }
        let value: f64 = rest[..end]
            .parse()
            .map_err(|_| anyhow!("invalid number: {}", &rest[..end]))?;
        self.pos += end;
        Ok(value)
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

        // histogram_quantile(phi, sel) [by (lbl, ...)]
        if ident == "histogram_quantile" {
            self.expect("(")?;
            let phi = self.parse_number()?;
            if !(0.0..=1.0).contains(&phi) {
                bail!("histogram_quantile expects a quantile between 0 and 1, got {phi}");
            }
            self.expect(",")?;
            let selector = self.parse_selector()?;
            self.expect(")")?;
            self.skip_ws();
            let by = if self.eat("by") {
                self.parse_by_clause()?
            } else {
                Vec::new()
            };
            return Ok(Expr::HistogramQuantile { phi, selector, by });
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

    // A trailing comparison against a scalar is supported at the top level
    // only. That is what an alert rule is — "this series crossed this line" —
    // and keeping it out of the grammar's interior avoids implying that
    // series-to-series comparison works, which it does not.
    if let Some(op) = p.parse_compare_op() {
        let threshold = p.parse_number()?;
        p.skip_ws();
        if !p.rest().is_empty() {
            bail!("unexpected trailing input: {}", p.rest());
        }
        return Ok(Expr::Compare {
            inner: Box::new(expr),
            op,
            threshold,
        });
    }

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
pub fn evaluate(store: &dyn Store, expr: &Expr, lookback_seconds: i64) -> Result<Vec<Series>> {
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
        Expr::HistogramQuantile { phi, selector, by } => {
            eval_histogram_quantile(store, selector, *phi, by, lookback_seconds)
        }
        Expr::Compare {
            inner,
            op,
            threshold,
        } => Ok(evaluate(store, inner, lookback_seconds)?
            .into_iter()
            .filter(|s| op.test(s.value, *threshold))
            .collect()),
    }
}

/// Estimate a quantile from stored histogram buckets.
///
/// This deliberately differs from Prometheus. There, histograms arrive as a fan
/// of `_bucket` series distinguished by an `le` label, so the idiom is
/// `histogram_quantile(0.95, rate(http_duration_bucket[5m]))`. tael keeps each
/// data point's whole bucket layout on the point itself, so the second argument
/// is the metric selector directly:
///
/// ```text
/// histogram_quantile(0.95, http_server_duration{service="api"})
/// histogram_quantile(0.99, http_server_duration) by (service)
/// ```
///
/// Points combine according to their temporality: delta histograms are summed
/// across the lookback window, while cumulative ones already carry every prior
/// observation so only the newest point per series is used. Series whose points
/// carry no bucket layout are skipped rather than reported as zero — a metric
/// that predates bucket retention has no quantile, and saying so beats
/// inventing one.
fn eval_histogram_quantile(
    store: &dyn Store,
    sel: &Selector,
    phi: f64,
    by: &[String],
    lookback_seconds: i64,
) -> Result<Vec<Series>> {
    let points = fetch_points(store, sel, lookback_seconds)?;

    // Collapse each series to a single distribution first, honoring
    // temporality, then merge those across the `by` grouping.
    let mut per_series: HashMap<String, MetricPoint> = HashMap::new();
    for point in points {
        if point.histogram.is_none() {
            continue;
        }
        let key = series_key(&point);
        match per_series.get_mut(&key) {
            None => {
                per_series.insert(key, point);
            }
            Some(existing) => {
                let delta = point
                    .histogram
                    .as_ref()
                    .is_some_and(|h| h.temporality == Temporality::Delta);
                if delta {
                    if let (Some(acc), Some(add)) =
                        (existing.histogram.as_mut(), point.histogram.as_ref())
                    {
                        acc.merge(add);
                    }
                    if point.timestamp > existing.timestamp {
                        existing.timestamp = point.timestamp;
                    }
                } else if point.timestamp > existing.timestamp {
                    *existing = point;
                }
            }
        }
    }

    // Group by the requested labels (or keep series distinct when empty).
    let mut grouped: HashMap<String, (HashMap<String, String>, MetricPoint)> = HashMap::new();
    for (series_id, point) in per_series {
        let labels = group_labels(&point, by);
        let key = if by.is_empty() {
            series_id
        } else {
            let mut parts: Vec<_> = labels.iter().collect();
            parts.sort();
            parts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        match grouped.get_mut(&key) {
            None => {
                grouped.insert(key, (labels, point));
            }
            Some((_, acc)) => {
                if let (Some(target), Some(add)) =
                    (acc.histogram.as_mut(), point.histogram.as_ref())
                    && !target.merge(add)
                {
                    // Bucket layouts differ across the group; merging them
                    // would misattribute observations.
                    tracing::debug!(
                        metric = %sel.metric,
                        "skipping histogram with a mismatched bucket layout while grouping"
                    );
                }
            }
        }
    }

    let mut out = Vec::new();
    for (_, (labels, point)) in grouped {
        let Some(value) = point.histogram.as_ref().and_then(|h| h.quantile(phi)) else {
            continue;
        };
        out.push(Series {
            metric: point.name.clone(),
            labels,
            value,
            timestamp: point.timestamp,
        });
    }
    Ok(out)
}

/// The label subset a point contributes to a `by (...)` grouping. With no
/// grouping labels, the point's full label set is preserved.
fn group_labels(point: &MetricPoint, by: &[String]) -> HashMap<String, String> {
    if by.is_empty() {
        let mut labels = point.attributes.clone();
        labels.insert("service".into(), point.service.clone());
        return labels;
    }
    let mut labels = HashMap::new();
    for name in by {
        let value = if name == "service" {
            Some(point.service.clone())
        } else if name == "__name__" {
            Some(point.name.clone())
        } else {
            point.attributes.get(name).cloned()
        };
        if let Some(value) = value {
            labels.insert(name.clone(), value);
        }
    }
    labels
}

fn fetch_points(
    store: &dyn Store,
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
    store: &dyn Store,
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

fn eval_rate(store: &dyn Store, sel: &Selector, range_seconds: i64) -> Result<Vec<Series>> {
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

    #[test]
    fn parses_dotted_otel_metric_names() {
        // OpenTelemetry semconv names are dotted; these are the common case for
        // a server that ingests OTLP natively.
        let e = parse(r#"http.server.request.duration{service="api"}"#).unwrap();
        let Expr::Selector(s) = e else {
            panic!("expected selector");
        };
        assert_eq!(s.metric, "http.server.request.duration");
        assert_eq!(s.matchers.len(), 1);
    }

    #[test]
    fn parses_histogram_quantile() {
        let e = parse(r#"histogram_quantile(0.95, http_duration{service="api"})"#).unwrap();
        let Expr::HistogramQuantile { phi, selector, by } = e else {
            panic!("expected histogram_quantile");
        };
        assert_eq!(phi, 0.95);
        assert_eq!(selector.metric, "http_duration");
        assert_eq!(selector.matchers.len(), 1);
        assert!(by.is_empty());
    }

    #[test]
    fn parses_histogram_quantile_with_grouping() {
        let e = parse("histogram_quantile(0.99, http_duration) by (service, route)").unwrap();
        let Expr::HistogramQuantile { by, .. } = e else {
            panic!("expected histogram_quantile");
        };
        assert_eq!(by, vec!["service".to_string(), "route".to_string()]);
    }

    #[test]
    fn histogram_quantile_rejects_a_phi_outside_zero_to_one() {
        let err = parse("histogram_quantile(95, http_duration)").unwrap_err();
        assert!(
            err.to_string().contains("between 0 and 1"),
            "unexpected error: {err}"
        );
    }

    // ── Evaluation against a real store ─────────────────────────────

    use crate::storage::models::{HistogramBuckets, MetricType};
    use crate::storage::testing::TestBackend;

    fn histogram_point(
        service: &str,
        counts: Vec<u64>,
        temporality: Temporality,
        age_secs: i64,
    ) -> MetricPoint {
        let count = counts.iter().sum();
        MetricPoint {
            timestamp: Utc::now() - chrono::Duration::seconds(age_secs),
            service: service.into(),
            name: "http_duration".into(),
            metric_type: MetricType::Histogram,
            value: 0.0,
            unit: "ms".into(),
            attributes: HashMap::new(),
            histogram: Some(HistogramBuckets {
                bounds: vec![10.0, 50.0, 100.0],
                counts,
                count,
                sum: 100.0,
                min: Some(1.0),
                max: Some(200.0),
                temporality,
            }),
        }
    }

    #[test]
    fn evaluates_a_quantile_from_stored_buckets() {
        let engine = TestBackend::new();
        engine
            .backend
            .insert_metrics(&[histogram_point(
                "api",
                vec![2, 5, 2, 1],
                Temporality::Cumulative,
                10,
            )])
            .unwrap();

        let expr = parse("histogram_quantile(0.5, http_duration)").unwrap();
        let series = evaluate(engine.backend.as_ref(), &expr, 300).unwrap();
        assert_eq!(series.len(), 1);
        // Same interpolation as the unit test on HistogramBuckets: 10 + 40*0.6.
        assert!((series[0].value - 34.0).abs() < 1e-9, "{:?}", series[0]);
    }

    #[test]
    fn cumulative_histograms_use_only_the_newest_point() {
        let engine = TestBackend::new();
        // A cumulative point already contains every earlier observation, so
        // summing the two would double-count.
        engine
            .backend
            .insert_metrics(&[
                histogram_point("api", vec![100, 0, 0, 0], Temporality::Cumulative, 120),
                histogram_point("api", vec![0, 0, 0, 4], Temporality::Cumulative, 10),
            ])
            .unwrap();

        let expr = parse("histogram_quantile(0.5, http_duration)").unwrap();
        let series = evaluate(engine.backend.as_ref(), &expr, 300).unwrap();
        assert_eq!(series.len(), 1);
        // Everything is in the open-ended bucket of the newest point, so the
        // reported max is the answer — not a value from the older point.
        assert_eq!(series[0].value, 200.0);
    }

    #[test]
    fn delta_histograms_accumulate_across_the_window() {
        let engine = TestBackend::new();
        engine
            .backend
            .insert_metrics(&[
                histogram_point("api", vec![4, 0, 0, 0], Temporality::Delta, 120),
                histogram_point("api", vec![0, 4, 0, 0], Temporality::Delta, 10),
            ])
            .unwrap();

        let expr = parse("histogram_quantile(0.5, http_duration)").unwrap();
        let series = evaluate(engine.backend.as_ref(), &expr, 300).unwrap();
        assert_eq!(series.len(), 1);
        // Merged distribution is 4 in (0,10] and 4 in (10,50]; the median lands
        // at the boundary between them.
        assert!((series[0].value - 10.0).abs() < 1e-9, "{:?}", series[0]);
    }

    #[test]
    fn points_without_buckets_yield_no_quantile_series() {
        let engine = TestBackend::new();
        engine
            .backend
            .insert_metrics(&[MetricPoint {
                timestamp: Utc::now(),
                service: "api".into(),
                name: "http_duration".into(),
                metric_type: MetricType::Histogram,
                value: 42.0,
                unit: "ms".into(),
                attributes: HashMap::new(),
                histogram: None,
            }])
            .unwrap();

        let expr = parse("histogram_quantile(0.95, http_duration)").unwrap();
        let series = evaluate(engine.backend.as_ref(), &expr, 300).unwrap();
        assert!(
            series.is_empty(),
            "a point with no bucket layout must not fabricate a quantile"
        );
    }

    #[test]
    fn grouping_merges_histograms_across_services() {
        let engine = TestBackend::new();
        engine
            .backend
            .insert_metrics(&[
                histogram_point("api", vec![8, 0, 0, 0], Temporality::Cumulative, 10),
                histogram_point("worker", vec![0, 8, 0, 0], Temporality::Cumulative, 10),
            ])
            .unwrap();

        let ungrouped = parse("histogram_quantile(0.5, http_duration)").unwrap();
        assert_eq!(
            evaluate(engine.backend.as_ref(), &ungrouped, 300)
                .unwrap()
                .len(),
            2,
            "without grouping each service reports its own quantile"
        );

        let grouped = parse("histogram_quantile(0.5, http_duration) by (__name__)").unwrap();
        let series = evaluate(engine.backend.as_ref(), &grouped, 300).unwrap();
        assert_eq!(series.len(), 1, "grouping collapses to one distribution");
        assert!((series[0].value - 10.0).abs() < 1e-9, "{:?}", series[0]);
    }
}
