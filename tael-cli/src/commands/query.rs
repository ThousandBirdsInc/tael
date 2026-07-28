use anyhow::Result;

use crate::OutputFormat;
use crate::client::TaelClient;
use crate::output;

fn parse_duration_ms(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        rest.parse().ok()
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.parse::<f64>().ok().map(|v| v * 1000.0)
    } else {
        s.parse().ok()
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn traces(
    client: &TaelClient,
    format: &OutputFormat,
    service: Option<String>,
    operation: Option<String>,
    min_duration: Option<String>,
    max_duration: Option<String>,
    status: Option<String>,
    last: Option<String>,
    limit: u32,
    attribute: Vec<String>,
    text: Option<String>,
    explain: bool,
) -> Result<()> {
    let min_ms = min_duration.as_deref().and_then(parse_duration_ms);
    let max_ms = max_duration.as_deref().and_then(parse_duration_ms);
    let attributes = parse_attribute_args(&attribute)?;

    let result = client
        .query_traces(
            service.as_deref(),
            operation.as_deref(),
            min_ms,
            max_ms,
            status.as_deref(),
            last.as_deref(),
            limit,
            &attributes,
            text.as_deref(),
            explain,
        )
        .await?;

    output::render(format, &result, output::print_spans_table);
    if explain && matches!(format, OutputFormat::Table) {
        output::print_explain(&result["explain"]);
    }
    if result["spans"].as_array().is_none_or(|s| s.is_empty()) {
        return Err(crate::exit::CategorizedError::no_results());
    }
    Ok(())
}

/// Validate `--attribute` specs and split them into (key, operator+value)
/// pairs for the wire.
///
/// Three operators are accepted, and the two-character forms must be tried
/// before the one-character form or the operator ends up inside the key:
/// `k=v` (exact), `k~=v` (contains), `k=~pattern` (regex). The operator is
/// preserved in the value half so the server sees the original spec; this
/// function's job is to reject a malformed one here, with the offending text
/// in hand, rather than at the far end of an HTTP round trip.
fn parse_attribute_args(args: &[String]) -> Result<Vec<(String, String)>> {
    args.iter()
        .map(|raw| {
            let (key, rest) = if let Some((k, v)) = raw.split_once("~=") {
                (k, format!("~={v}"))
            } else if let Some((k, v)) = raw.split_once("=~") {
                (k, format!("=~{v}"))
            } else {
                let (k, v) = raw.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!(
                        "--attribute expects key=value, key~=value (contains), \
                         or key=~pattern (regex); got {raw:?}"
                    )
                })?;
                (k, format!("={v}"))
            };
            let key = key.trim();
            if key.is_empty() {
                anyhow::bail!("--attribute key cannot be empty (got {raw:?})");
            }
            Ok((key.to_string(), rest))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn logs(
    client: &TaelClient,
    format: &OutputFormat,
    service: Option<String>,
    severity: Option<String>,
    body_contains: Option<String>,
    trace_id: Option<String>,
    attribute: Vec<String>,
    last: Option<String>,
    limit: u32,
) -> Result<()> {
    let attributes = parse_attribute_args(&attribute)?;
    let result = client
        .query_logs(
            service.as_deref(),
            severity.as_deref(),
            body_contains.as_deref(),
            trace_id.as_deref(),
            &attributes,
            last.as_deref(),
            limit,
        )
        .await?;

    output::render(format, &result, output::print_logs_table);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn metrics(
    client: &TaelClient,
    format: &OutputFormat,
    query: Option<String>,
    service: Option<String>,
    name: Option<String>,
    metric_type: Option<String>,
    last: Option<String>,
    limit: u32,
    rollups: bool,
) -> Result<()> {
    if let Some(q) = query {
        let result = client.promql_query(&q, last.as_deref()).await?;
        output::render(format, &result, output::print_series_table);
        return Ok(());
    }

    if rollups {
        let result = client
            .metric_rollups(name.as_deref(), service.as_deref(), last.as_deref(), limit)
            .await?;
        output::render(format, &result, output::print_rollups_table);
        if result["rollups"].as_array().is_none_or(|r| r.is_empty()) {
            return Err(crate::exit::CategorizedError::no_results());
        }
        return Ok(());
    }

    let result = client
        .query_metrics(
            service.as_deref(),
            name.as_deref(),
            metric_type.as_deref(),
            last.as_deref(),
            limit,
        )
        .await?;

    output::render(format, &result, output::print_metrics_table);
    Ok(())
}

pub async fn sql(client: &TaelClient, format: &OutputFormat, query: &str) -> Result<()> {
    let result = client.query_sql(query).await?;
    output::render(format, &result, output::print_sql_rows);
    // The SQL endpoint reports rejections in the body rather than by status,
    // so the error has to be lifted here for the exit code to reflect it.
    if let Some(error) = result["error"].as_str() {
        return Err(
            crate::exit::CategorizedError::new(crate::exit::ExitCategory::BadQuery, error).into(),
        );
    }
    if result["rows"].as_array().is_none_or(|r| r.is_empty()) {
        return Err(crate::exit::CategorizedError::no_results());
    }
    Ok(())
}
