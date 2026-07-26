//! Retention and compaction policy.
//!
//! Retention used to be a handful of `TAEL_*` environment variables applied
//! uniformly to every signal, which the code itself described as a stopgap.
//! That conflates things with genuinely different economics: span payloads are
//! large and investigated within days, metric rollups are tiny and wanted for a
//! year. This module gives each signal its own clock, read from a TOML file.
//!
//! Precedence, highest first: command-line flag, environment variable, config
//! file, built-in default. The environment keeps winning over the file so an
//! existing deployment's `TAEL_HOT_TIER_HOURS` is not silently overridden by a
//! config file someone adds later.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Default config filename inside the tael home directory.
const CONFIG_FILE: &str = "config.toml";

/// How long each signal is kept, and how the hot/cold split is managed.
#[derive(Debug, Clone, PartialEq)]
pub struct RetentionPolicy {
    /// How long spans, logs, and metrics stay in the LSM hot tier before
    /// rolling into Parquet. Short by design: the hot tier optimizes for recent
    /// reads, not capacity.
    pub hot_tier_hours: i64,
    /// How often the maintenance pass runs.
    pub compact_interval_secs: u64,
    /// Span metadata (the searchable row).
    pub traces: i64,
    /// LLM prompt/completion payload blobs. Kept on their own clock because
    /// they dominate storage while the spans referencing them stay cheap — an
    /// investigation usually needs the span long after it needs the payload.
    pub llm_payloads: i64,
    pub logs: i64,
    /// Raw metric points.
    pub metrics_raw: i64,
    /// 5-minute metric rollups, which survive raw points so long-range trends
    /// stay answerable at a fraction of the size.
    pub metrics_rollups: i64,
}

/// The built-in retention window, in days, for every signal.
///
/// DESIGN.md proposes shorter per-signal windows (traces 7d, logs 14d, metrics
/// 30d raw), and they are good recommendations for a busy deployment — but
/// before this module existed, everything was retained for a year. Adopting the
/// shorter windows as defaults would silently delete a year of history the
/// first time an existing server restarted after an upgrade, which is not a
/// change that should happen without someone asking for it.
///
/// So the default preserves the previous behavior, and the recommended policy
/// ships as a config file to opt into. See `docs/config.example.toml`.
const DEFAULT_RETENTION_DAYS: i64 = 365;

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            hot_tier_hours: 24,
            compact_interval_secs: 3600,
            traces: DEFAULT_RETENTION_DAYS,
            llm_payloads: DEFAULT_RETENTION_DAYS,
            logs: DEFAULT_RETENTION_DAYS,
            metrics_raw: DEFAULT_RETENTION_DAYS,
            metrics_rollups: DEFAULT_RETENTION_DAYS,
        }
    }
}

/// The policy DESIGN.md recommends: each signal on the clock that matches how
/// it is actually used. Written to `config.toml` by `tael config init`.
pub const RECOMMENDED_CONFIG: &str = r#"# tael configuration.
#
# Precedence, highest first: command-line flag, environment variable, this
# file, built-in default. Durations accept h, d, or w.

[storage]
# How long signals stay in the LSM hot tier before rolling into Parquet.
hot_tier_hours = 24
# How often the maintenance pass runs.
compact_interval_secs = 3600

[retention]
# Span metadata. Large and high-cardinality; a week covers most investigations.
traces = "7d"
# LLM prompt/completion payloads. These dominate storage, and an investigation
# usually needs the span long after it needs the payload — so they can expire
# well before the spans that reference them.
llm_payloads = "3d"
# Verbose but searchable.
logs = "14d"
# Raw metric points.
metrics_raw = "30d"
# 5-minute rollups. Deliberately much longer than metrics_raw: they exist so
# year-scale trends survive at a fraction of the size.
metrics_rollups = "365d"
"#;

impl RetentionPolicy {
    /// Resolve the effective policy: defaults, then the config file, then the
    /// environment.
    pub fn resolve(config_path: Option<&str>, data_dir: &str) -> Result<Self> {
        let mut policy = Self::default();

        if let Some(file) = load_config_file(config_path, data_dir)? {
            policy.apply_file(&file)?;
        }
        policy.apply_env()?;
        policy.validate()?;
        Ok(policy)
    }

    fn apply_file(&mut self, file: &ConfigFile) -> Result<()> {
        if let Some(storage) = &file.storage {
            if let Some(hours) = storage.hot_tier_hours {
                self.hot_tier_hours = hours;
            }
            if let Some(secs) = storage.compact_interval_secs {
                self.compact_interval_secs = secs;
            }
        }
        if let Some(r) = &file.retention {
            for (target, raw) in [
                (&mut self.traces, &r.traces),
                (&mut self.llm_payloads, &r.llm_payloads),
                (&mut self.logs, &r.logs),
                (&mut self.metrics_raw, &r.metrics_raw),
                (&mut self.metrics_rollups, &r.metrics_rollups),
            ] {
                if let Some(value) = raw {
                    *target = parse_days(value)?;
                }
            }
        }
        Ok(())
    }

    /// Environment overrides. The legacy `TAEL_TRACE_RETENTION_DAYS` and
    /// `TAEL_HOT_TIER_HOURS` names are kept so existing deployments keep
    /// working unchanged.
    fn apply_env(&mut self) -> Result<()> {
        if let Some(v) = env_i64("TAEL_HOT_TIER_HOURS")? {
            self.hot_tier_hours = v;
        }
        if let Some(v) = env_u64("TAEL_COMPACT_INTERVAL_SECS")? {
            self.compact_interval_secs = v;
        }
        if let Some(v) = env_i64("TAEL_TRACE_RETENTION_DAYS")? {
            self.traces = v;
        }
        if let Some(v) = env_i64("TAEL_LLM_PAYLOAD_RETENTION_DAYS")? {
            self.llm_payloads = v;
        }
        if let Some(v) = env_i64("TAEL_LOG_RETENTION_DAYS")? {
            self.logs = v;
        }
        if let Some(v) = env_i64("TAEL_METRIC_RETENTION_DAYS")? {
            self.metrics_raw = v;
        }
        if let Some(v) = env_i64("TAEL_METRIC_ROLLUP_RETENTION_DAYS")? {
            self.metrics_rollups = v;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.compact_interval_secs == 0 {
            bail!("compact_interval_secs must be greater than 0");
        }
        for (name, days) in [
            ("traces", self.traces),
            ("llm_payloads", self.llm_payloads),
            ("logs", self.logs),
            ("metrics_raw", self.metrics_raw),
            ("metrics_rollups", self.metrics_rollups),
        ] {
            if days < 0 {
                bail!("retention.{name} must not be negative (got {days})");
            }
        }
        if self.hot_tier_hours < 0 {
            bail!("hot_tier_hours must not be negative");
        }
        // A rollup window shorter than the raw window is almost certainly a
        // mistake: rollups exist to outlive raw points, and the reverse order
        // silently discards the long-range trends they were kept for.
        if self.metrics_rollups < self.metrics_raw {
            tracing::warn!(
                rollups = self.metrics_rollups,
                raw = self.metrics_raw,
                "metric rollups are being dropped before the raw points they summarize; \
                 long-range trends will be lost"
            );
        }
        Ok(())
    }

    /// Cutoff instants for one maintenance pass, computed from `now` so every
    /// signal in a pass measures against the same clock.
    pub fn cutoffs(&self, now: chrono::DateTime<chrono::Utc>) -> RetentionCutoffs {
        RetentionCutoffs {
            hot_tier: now - chrono::Duration::hours(self.hot_tier_hours),
            traces: now - chrono::Duration::days(self.traces),
            llm_payloads: now - chrono::Duration::days(self.llm_payloads),
            logs: now - chrono::Duration::days(self.logs),
            metrics_raw: now - chrono::Duration::days(self.metrics_raw),
            metrics_rollups: now - chrono::Duration::days(self.metrics_rollups),
        }
    }
}

/// Resolved cutoff instants for a single maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct RetentionCutoffs {
    pub hot_tier: chrono::DateTime<chrono::Utc>,
    pub traces: chrono::DateTime<chrono::Utc>,
    pub llm_payloads: chrono::DateTime<chrono::Utc>,
    pub logs: chrono::DateTime<chrono::Utc>,
    pub metrics_raw: chrono::DateTime<chrono::Utc>,
    pub metrics_rollups: chrono::DateTime<chrono::Utc>,
}

// ── Config file ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ConfigFile {
    storage: Option<StorageSection>,
    retention: Option<RetentionSection>,
}

#[derive(Debug, Deserialize)]
struct StorageSection {
    hot_tier_hours: Option<i64>,
    compact_interval_secs: Option<u64>,
}

/// Durations are strings (`"7d"`) rather than numbers so the unit is visible in
/// the file — `traces = 7` reads ambiguously next to `compact_interval_secs`.
#[derive(Debug, Deserialize)]
struct RetentionSection {
    traces: Option<String>,
    llm_payloads: Option<String>,
    logs: Option<String>,
    metrics_raw: Option<String>,
    metrics_rollups: Option<String>,
}

/// Load the config file, if there is one.
///
/// An explicitly requested path that does not exist is an error — the operator
/// asked for it. The default path being absent is not.
fn load_config_file(config_path: Option<&str>, data_dir: &str) -> Result<Option<ConfigFile>> {
    let (path, explicit) = match config_path {
        Some(p) => (PathBuf::from(p), true),
        None => (default_config_path(data_dir), false),
    };

    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let parsed = toml::from_str(&text)
                .with_context(|| format!("parsing config file {}", path.display()))?;
            tracing::info!(config = %path.display(), "loaded config file");
            Ok(Some(parsed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading config file {}", path.display())),
    }
}

/// `<data_dir>/../config.toml` — i.e. `~/.tael/config.toml` for the default
/// data directory, so config sits beside the data it governs.
fn default_config_path(data_dir: &str) -> PathBuf {
    Path::new(data_dir)
        .parent()
        .unwrap_or(Path::new(data_dir))
        .join(CONFIG_FILE)
}

/// Parse a retention duration like `7d`, `36h`, or a bare number of days.
fn parse_days(raw: &str) -> Result<i64> {
    let s = raw.trim();
    if s.is_empty() {
        bail!("empty retention duration");
    }
    let (value, unit) = s.split_at(
        s.find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(s.len()),
    );
    let value: f64 = value
        .parse()
        .with_context(|| format!("`{raw}` is not a valid duration"))?;
    let days = match unit.trim() {
        "" | "d" => value,
        "h" => value / 24.0,
        "w" => value * 7.0,
        other => bail!("unknown duration unit `{other}` in `{raw}` (use h, d, or w)"),
    };
    Ok(days.round() as i64)
}

fn env_i64(name: &str) -> Result<Option<i64>> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            Ok(Some(v.trim().parse().with_context(|| {
                format!("{name} must be an integer, got `{v}`")
            })?))
        }
        _ => Ok(None),
    }
}

fn env_u64(name: &str) -> Result<Option<u64>> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => {
            Ok(Some(v.trim().parse().with_context(|| {
                format!("{name} must be a positive integer, got `{v}`")
            })?))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_preserve_the_pre_config_behavior() {
        // Every signal was retained for a year before per-signal policy
        // existed. Shortening a default would delete history on upgrade, so
        // the shorter windows ship as an opt-in config instead.
        let p = RetentionPolicy::default();
        assert_eq!(p.traces, DEFAULT_RETENTION_DAYS);
        assert_eq!(p.logs, DEFAULT_RETENTION_DAYS);
        assert_eq!(p.metrics_raw, DEFAULT_RETENTION_DAYS);
        assert_eq!(p.metrics_rollups, DEFAULT_RETENTION_DAYS);
        assert_eq!(p.hot_tier_hours, 24);
    }

    #[test]
    fn the_recommended_config_parses_and_shortens_every_window() {
        let file: ConfigFile = toml::from_str(RECOMMENDED_CONFIG).expect("shipped config parses");
        let mut policy = RetentionPolicy::default();
        policy.apply_file(&file).unwrap();
        policy.validate().unwrap();

        assert_eq!(policy.traces, 7);
        assert_eq!(policy.llm_payloads, 3);
        assert_eq!(policy.logs, 14);
        assert_eq!(policy.metrics_raw, 30);
        assert_eq!(policy.metrics_rollups, 365);
        // Rollups must outlive the raw points they summarize.
        assert!(policy.metrics_rollups > policy.metrics_raw);
    }

    #[test]
    fn parses_duration_units() {
        assert_eq!(parse_days("7d").unwrap(), 7);
        assert_eq!(parse_days("14").unwrap(), 14);
        assert_eq!(parse_days("48h").unwrap(), 2);
        assert_eq!(parse_days("2w").unwrap(), 14);
        assert!(parse_days("7y").is_err());
        assert!(parse_days("").is_err());
    }

    #[test]
    fn config_file_overrides_defaults_per_signal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[storage]
hot_tier_hours = 6

[retention]
traces = "3d"
logs = "1w"
metrics_rollups = "730d"
"#,
        )
        .unwrap();

        let mut policy = RetentionPolicy::default();
        let file = load_config_file(Some(path.to_str().unwrap()), "unused")
            .unwrap()
            .unwrap();
        policy.apply_file(&file).unwrap();

        assert_eq!(policy.hot_tier_hours, 6);
        assert_eq!(policy.traces, 3);
        assert_eq!(policy.logs, 7);
        assert_eq!(policy.metrics_rollups, 730);
        // Unset keys keep their defaults rather than resetting to zero.
        assert_eq!(policy.metrics_raw, DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn a_missing_default_config_is_fine_but_a_named_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        assert!(
            load_config_file(None, data_dir.to_str().unwrap())
                .unwrap()
                .is_none()
        );
        assert!(load_config_file(Some("/nonexistent/tael.toml"), "unused").is_err());
    }

    #[test]
    fn malformed_config_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[retention]\ntraces = ").unwrap();
        let err = load_config_file(Some(path.to_str().unwrap()), "unused").unwrap_err();
        assert!(
            err.to_string().contains("config.toml"),
            "error should name the file: {err}"
        );
    }

    #[test]
    fn negative_retention_is_rejected() {
        let mut policy = RetentionPolicy::default();
        policy.logs = -1;
        assert!(policy.validate().is_err());

        let mut policy = RetentionPolicy::default();
        policy.compact_interval_secs = 0;
        assert!(policy.validate().is_err());
    }

    #[test]
    fn cutoffs_share_one_clock_across_signals() {
        let policy = RetentionPolicy {
            traces: 7,
            logs: 14,
            metrics_raw: 30,
            metrics_rollups: 365,
            ..RetentionPolicy::default()
        };
        let now = chrono::Utc::now();
        let c = policy.cutoffs(now);
        assert_eq!((now - c.traces).num_days(), 7);
        assert_eq!((now - c.logs).num_days(), 14);
        assert_eq!((now - c.metrics_raw).num_days(), 30);
        assert_eq!((now - c.metrics_rollups).num_days(), 365);
        assert_eq!((now - c.hot_tier).num_hours(), 24);
    }

    #[test]
    fn default_config_path_sits_beside_the_data_directory() {
        assert_eq!(
            default_config_path("/home/u/.tael/data"),
            PathBuf::from("/home/u/.tael/config.toml")
        );
    }
}
