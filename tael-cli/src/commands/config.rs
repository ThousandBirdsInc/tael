//! `tael config` — inspect and scaffold the retention/compaction config.
//!
//! Retention is invisible until it deletes something, which is the worst time
//! to discover what it was set to. `config show` reports the policy actually in
//! effect (after the file and environment have been layered on), and
//! `config init` writes a commented file with the recommended windows so
//! tightening them is a deliberate act rather than a guess at key names.

use anyhow::{Context, Result};
use tael_server::retention::{RECOMMENDED_CONFIG, RetentionPolicy};

use crate::OutputFormat;
use crate::output::print_json;

/// Print the effective policy and where it came from.
pub fn show(format: &OutputFormat, config_path: Option<&str>, data_dir: &str) -> Result<()> {
    let policy = RetentionPolicy::resolve(config_path, data_dir)?;
    let path = resolved_path(config_path, data_dir);
    let exists = path.exists();

    let payload = serde_json::json!({
        "config_file": path.display().to_string(),
        "config_file_exists": exists,
        "storage": {
            "hot_tier_hours": policy.hot_tier_hours,
            "compact_interval_secs": policy.compact_interval_secs,
        },
        "retention_days": {
            "traces": policy.traces,
            "llm_payloads": policy.llm_payloads,
            "logs": policy.logs,
            "metrics_raw": policy.metrics_raw,
            "metrics_rollups": policy.metrics_rollups,
        },
    });

    match format {
        OutputFormat::Json => print_json(&payload),
        OutputFormat::Table => {
            println!("config file  {}", path.display());
            if !exists {
                println!("             (not present — built-in defaults in effect)");
            }
            println!();
            println!("storage");
            println!("  hot tier          {}h", policy.hot_tier_hours);
            println!("  compact interval  {}s", policy.compact_interval_secs);
            println!();
            println!("retention");
            for (name, days) in [
                ("traces", policy.traces),
                ("llm_payloads", policy.llm_payloads),
                ("logs", policy.logs),
                ("metrics_raw", policy.metrics_raw),
                ("metrics_rollups", policy.metrics_rollups),
            ] {
                println!("  {name:<17} {days}d");
            }
            if !exists {
                println!();
                println!(
                    "Run `tael config init` to write a file with shorter recommended windows."
                );
            }
        }
    }
    Ok(())
}

/// Write the recommended config. Refuses to clobber an existing file without
/// `--force`, since that file is the only record of a deliberate policy.
pub fn init(
    format: &OutputFormat,
    config_path: Option<&str>,
    data_dir: &str,
    force: bool,
) -> Result<()> {
    let path = resolved_path(config_path, data_dir);
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite it",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, RECOMMENDED_CONFIG)
        .with_context(|| format!("writing {}", path.display()))?;

    match format {
        OutputFormat::Json => print_json(&serde_json::json!({
            "written": path.display().to_string(),
            "note": "restart the server for the new policy to take effect",
        })),
        OutputFormat::Table => {
            println!("Wrote {}", path.display());
            println!("Restart the server for the new policy to take effect.");
        }
    }
    Ok(())
}

/// Where the config file lives: the explicit path, or `config.toml` beside the
/// data directory. Mirrors the server's own resolution.
fn resolved_path(config_path: Option<&str>, data_dir: &str) -> std::path::PathBuf {
    match config_path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::Path::new(data_dir)
            .parent()
            .unwrap_or(std::path::Path::new(data_dir))
            .join("config.toml"),
    }
}
