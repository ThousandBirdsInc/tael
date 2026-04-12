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
) -> Result<()> {
    let min_ms = min_duration.as_deref().and_then(parse_duration_ms);
    let max_ms = max_duration.as_deref().and_then(parse_duration_ms);

    let result = client
        .query_traces(
            service.as_deref(),
            operation.as_deref(),
            min_ms,
            max_ms,
            status.as_deref(),
            last.as_deref(),
            limit,
        )
        .await?;

    output::render(format, &result, output::print_spans_table);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn logs(
    client: &TaelClient,
    format: &OutputFormat,
    service: Option<String>,
    severity: Option<String>,
    body_contains: Option<String>,
    trace_id: Option<String>,
    last: Option<String>,
    limit: u32,
) -> Result<()> {
    let result = client
        .query_logs(
            service.as_deref(),
            severity.as_deref(),
            body_contains.as_deref(),
            trace_id.as_deref(),
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
) -> Result<()> {
    if let Some(q) = query {
        let result = client.promql_query(&q, last.as_deref()).await?;
        output::render(format, &result, output::print_series_table);
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
