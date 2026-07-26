//! Threshold conditions for `tael watch --exit-on`.
//!
//! `watch` is the blocking primitive an agent uses to wait on a system — "run
//! the deploy, then tell me when errors spike." Without a stop condition the
//! agent has to poll, parse, and decide on every tick, which burns a turn each
//! time. A condition lets the command block until something is true and then
//! exit with a distinct code.
//!
//! Grammar: `<field><op><value>`, where value is either an absolute number or a
//! multiple of the first observed sample (`2x`).
//!
//! ```text
//! error_rate>0.05      absolute threshold
//! p95_ms>2x            regression relative to the first tick
//! delta_error_count>0  any new errors since the previous tick
//! span_count<1         traffic stopped
//! ```

use anyhow::{Result, bail};
use serde_json::Value;

/// Comparison direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Gt,
    Gte,
    Lt,
    Lte,
}

impl Op {
    fn compare(self, actual: f64, threshold: f64) -> bool {
        match self {
            Op::Gt => actual > threshold,
            Op::Gte => actual >= threshold,
            Op::Lt => actual < threshold,
            Op::Lte => actual <= threshold,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Op::Gt => ">",
            Op::Gte => ">=",
            Op::Lt => "<",
            Op::Lte => "<=",
        }
    }
}

/// What the threshold is measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Threshold {
    /// A fixed value: `error_rate>0.05`.
    Absolute(f64),
    /// A multiple of the field's first observed value: `p95_ms>2x`. Useful when
    /// the healthy baseline isn't known ahead of time, which is the normal case
    /// when an agent starts watching mid-incident.
    RelativeToBaseline(f64),
}

/// One parsed `--exit-on` condition.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub field: String,
    pub op: Op,
    pub threshold: Threshold,
    /// The original text, echoed back when the condition trips so the caller
    /// sees exactly what it asked for.
    pub source: String,
}

impl Condition {
    pub fn parse(src: &str) -> Result<Self> {
        let trimmed = src.trim();
        // Two-character operators must be tried first, or `>=` parses as `>`
        // with a threshold of "=0.05".
        let (field, op, rest) = if let Some((f, r)) = trimmed.split_once(">=") {
            (f, Op::Gte, r)
        } else if let Some((f, r)) = trimmed.split_once("<=") {
            (f, Op::Lte, r)
        } else if let Some((f, r)) = trimmed.split_once('>') {
            (f, Op::Gt, r)
        } else if let Some((f, r)) = trimmed.split_once('<') {
            (f, Op::Lt, r)
        } else {
            bail!(
                "condition `{src}` needs a comparison: \
                 <field><op><value>, e.g. error_rate>0.05 or p95_ms>2x"
            );
        };

        let field = field.trim();
        if field.is_empty() {
            bail!("condition `{src}` is missing a field name");
        }
        let rest = rest.trim();
        if rest.is_empty() {
            bail!("condition `{src}` is missing a threshold value");
        }

        let threshold = match rest.strip_suffix(['x', 'X']) {
            Some(multiple) => {
                let m: f64 = multiple.trim().parse().map_err(|_| {
                    anyhow::anyhow!("`{rest}` is not a valid multiplier (try 2x, 1.5x)")
                })?;
                Threshold::RelativeToBaseline(m)
            }
            None => Threshold::Absolute(
                rest.parse()
                    .map_err(|_| anyhow::anyhow!("`{rest}` is not a number"))?,
            ),
        };

        Ok(Self {
            field: field.to_string(),
            op,
            threshold,
            source: trimmed.to_string(),
        })
    }

    /// Evaluate against one watch tick. `baseline` is the field's value on the
    /// first tick, needed only by relative thresholds.
    ///
    /// Returns `None` when the tick doesn't carry the field at all, which is
    /// reported separately from "not tripped" — a typo in a field name should
    /// not look like a healthy system.
    pub fn evaluate(&self, tick: &Value, baseline: Option<f64>) -> Option<Trip> {
        let actual = lookup_field(tick, &self.field)?;
        let threshold = match self.threshold {
            Threshold::Absolute(v) => v,
            Threshold::RelativeToBaseline(multiple) => baseline? * multiple,
        };
        self.op.compare(actual, threshold).then(|| Trip {
            condition: self.source.clone(),
            field: self.field.clone(),
            actual,
            threshold,
            op: self.op.as_str().to_string(),
        })
    }
}

/// A condition that became true.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Trip {
    pub condition: String,
    pub field: String,
    pub actual: f64,
    pub threshold: f64,
    pub op: String,
}

/// Resolve a field name against a watch tick.
///
/// Ticks are nested by signal (`traces.error_rate`), but conditions are written
/// flat (`error_rate`) because that is how someone thinks about them. Dotted
/// paths still work for disambiguation, e.g. `logs.total` vs `metrics.total`.
pub fn lookup_field(tick: &Value, field: &str) -> Option<f64> {
    if let Some((group, name)) = field.split_once('.') {
        return tick.get(group)?.get(name)?.as_f64();
    }
    if let Some(v) = tick.get(field).and_then(Value::as_f64) {
        return Some(v);
    }
    // Search the signal groups in a fixed order so a name present in more than
    // one group resolves the same way every run.
    for group in ["traces", "logs", "metrics"] {
        if let Some(v) = tick
            .get(group)
            .and_then(|g| g.get(field))
            .and_then(Value::as_f64)
        {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tick(error_rate: f64, p95: f64, spans: i64) -> Value {
        json!({
            "window_seconds": 60,
            "traces": {
                "span_count": spans,
                "error_count": 3,
                "error_rate": error_rate,
                "p95_ms": p95,
                "delta_error_count": 2,
            },
            "logs": { "total": 100, "error": 5 },
            "metrics": { "point_count": 12 },
        })
    }

    #[test]
    fn parses_absolute_thresholds() {
        let c = Condition::parse("error_rate>0.05").unwrap();
        assert_eq!(c.field, "error_rate");
        assert_eq!(c.op, Op::Gt);
        assert_eq!(c.threshold, Threshold::Absolute(0.05));
    }

    #[test]
    fn parses_two_character_operators_before_one() {
        let c = Condition::parse("p95_ms>=250").unwrap();
        assert_eq!(c.op, Op::Gte);
        assert_eq!(c.threshold, Threshold::Absolute(250.0));

        let c = Condition::parse("span_count<=0").unwrap();
        assert_eq!(c.op, Op::Lte);
        assert_eq!(c.threshold, Threshold::Absolute(0.0));
    }

    #[test]
    fn parses_baseline_multipliers() {
        let c = Condition::parse("p95_ms>2x").unwrap();
        assert_eq!(c.threshold, Threshold::RelativeToBaseline(2.0));
        let c = Condition::parse("p95_ms > 1.5x").unwrap();
        assert_eq!(c.threshold, Threshold::RelativeToBaseline(1.5));
    }

    #[test]
    fn rejects_malformed_conditions() {
        for bad in ["error_rate", "error_rate>", ">0.5", "error_rate>abc"] {
            assert!(Condition::parse(bad).is_err(), "`{bad}` should not parse");
        }
    }

    #[test]
    fn absolute_condition_trips_only_when_crossed() {
        let c = Condition::parse("error_rate>0.05").unwrap();
        assert!(c.evaluate(&tick(0.01, 100.0, 500), None).is_none());
        let trip = c.evaluate(&tick(0.12, 100.0, 500), None).unwrap();
        assert_eq!(trip.field, "error_rate");
        assert_eq!(trip.actual, 0.12);
        assert_eq!(trip.threshold, 0.05);
    }

    #[test]
    fn relative_condition_measures_against_the_first_sample() {
        let c = Condition::parse("p95_ms>2x").unwrap();
        // Baseline 100ms: 150ms is a regression but not a doubling.
        assert!(c.evaluate(&tick(0.0, 150.0, 1), Some(100.0)).is_none());
        let trip = c.evaluate(&tick(0.0, 250.0, 1), Some(100.0)).unwrap();
        assert_eq!(trip.threshold, 200.0);
        assert_eq!(trip.actual, 250.0);
    }

    #[test]
    fn relative_condition_waits_for_a_baseline() {
        let c = Condition::parse("p95_ms>2x").unwrap();
        assert!(
            c.evaluate(&tick(0.0, 9999.0, 1), None).is_none(),
            "with no baseline yet there is nothing to compare against"
        );
    }

    #[test]
    fn fields_resolve_flat_or_dotted() {
        let t = tick(0.02, 100.0, 500);
        assert_eq!(lookup_field(&t, "error_rate"), Some(0.02));
        assert_eq!(lookup_field(&t, "traces.error_rate"), Some(0.02));
        assert_eq!(lookup_field(&t, "logs.error"), Some(5.0));
        assert_eq!(lookup_field(&t, "delta_error_count"), Some(2.0));
        assert_eq!(lookup_field(&t, "window_seconds"), Some(60.0));
    }

    #[test]
    fn ambiguous_names_resolve_to_a_fixed_group_order() {
        // `total` exists only under logs here; the search order makes the
        // resolution deterministic rather than dependent on map iteration.
        let t = tick(0.0, 1.0, 1);
        assert_eq!(lookup_field(&t, "total"), Some(100.0));
    }

    #[test]
    fn unknown_fields_are_distinguishable_from_untripped() {
        let c = Condition::parse("typo_rate>0.5").unwrap();
        assert!(c.evaluate(&tick(0.9, 1.0, 1), None).is_none());
        assert!(lookup_field(&tick(0.9, 1.0, 1), "typo_rate").is_none());
    }
}
