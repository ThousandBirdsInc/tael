use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;

/// `tael server migrate` — copy a legacy DuckDB datastore into the
/// tael-backend engine. Offline: operates on the data directory directly.
#[cfg(feature = "duckdb")]
pub fn migrate(format: &OutputFormat, source: &str, target: &str, dry_run: bool) -> Result<()> {
    let report = tael_server::migrate::migrate_duckdb(source, target, dry_run)?;
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string(&report)?),
        OutputFormat::Table => {
            let verb = if dry_run { "Would migrate" } else { "Migrated" };
            println!(
                "{verb} {} span(s), {} log(s), {} metric point(s), {} comment(s) \
                 from the DuckDB store in {source} to the tael-backend engine in {target}.",
                report.spans, report.logs, report.metrics, report.comments
            );
            if dry_run {
                println!("Re-run without --dry-run to apply.");
            } else {
                println!(
                    "Note: migrated comments carry new ids/timestamps, and migrated \
                     spans are not full-text searchable (the index is built at ingest)."
                );
            }
        }
    }
    Ok(())
}

/// Without the `duckdb` feature there is no DuckDB engine in the binary to
/// read from — say so instead of failing with a missing-file error.
#[cfg(not(feature = "duckdb"))]
pub fn migrate(_format: &OutputFormat, _source: &str, _target: &str, _dry_run: bool) -> Result<()> {
    Err(crate::exit::CategorizedError::new(
        crate::exit::ExitCategory::BadQuery,
        "this build does not include the DuckDB engine; reinstall with \
         `cargo install tael-cli --features duckdb` to run the migration"
            .to_string(),
    )
    .into())
}

pub async fn status(client: &TaelClient, format: &OutputFormat) -> Result<()> {
    match client.healthz().await {
        Ok(resp) => match format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({ "status": "healthy", "response": resp })
                );
            }
            OutputFormat::Table => {
                println!("Server: healthy");
            }
        },
        Err(e) => match format {
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({ "status": "unreachable", "error": e.to_string() })
                );
            }
            OutputFormat::Table => {
                eprintln!("Server unreachable: {e}");
            }
        },
    }
    Ok(())
}
