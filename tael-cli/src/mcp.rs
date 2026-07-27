//! Model Context Protocol server (`tael mcp serve`).
//!
//! This is the same surface as the CLI, reachable without a shell. An agent
//! that has tael configured as an MCP server can query traces, correlate
//! signals, and file issues as native tool calls instead of shelling out and
//! parsing stdout — which is the difference between telemetry being a first
//! class part of its reasoning and being a subprocess it has to remember to run.
//!
//! Every tool maps one-to-one onto an existing REST endpoint and returns the
//! JSON shapes already documented in `llm.txt`. Nothing new is invented here:
//! an agent that has read `SKILL.md` already knows how to interpret every
//! response, and both documents are served as MCP resources so a freshly
//! connected agent can read them without being told to.
//!
//! The transport is line-delimited JSON-RPC 2.0 over stdio, which is what MCP
//! clients spawn by default.

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::{Value, json};

use crate::client::TaelClient;

/// MCP revision this server implements. A client asking for a different
/// revision still gets served — the negotiated value is echoed back — since the
/// method set used here has been stable across revisions.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes used by this server (from the JSON-RPC 2.0 spec).
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;

/// Run the stdio server until the client closes the connection.
pub async fn serve(client: TaelClient, server_url: &str) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    // Diagnostics go to stderr: stdout is the protocol channel, and a stray
    // line there desynchronizes the client.
    eprintln!("tael MCP server ready (target {server_url})");

    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // A parse failure has no id to correlate against, so the only
                // useful thing is to say so and keep the loop alive.
                eprintln!("tael mcp: ignoring unparseable message: {e}");
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = request.get("params").cloned().unwrap_or(json!({}));

        // Notifications carry no id and must not be answered.
        if id.is_none() {
            continue;
        }

        let response = match dispatch(&client, &method, &params).await {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(err) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": err.code, "message": err.message },
            }),
        };

        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    Ok(())
}

#[derive(Debug)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

async fn dispatch(client: &TaelClient, method: &str, params: &Value) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {}, "resources": {} },
            "serverInfo": { "name": "tael", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(client, params).await,
        "resources/list" => Ok(json!({ "resources": resource_definitions() })),
        "resources/read" => read_resource(params),
        other => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("unknown method `{other}`"),
        )),
    }
}

/// Shown to the agent at connection time. Deliberately short: the detailed
/// contract lives in the `SKILL.md` and `llm.txt` resources, and duplicating it
/// here would guarantee the two drift.
const INSTRUCTIONS: &str = "\
tael is an observability backend holding OpenTelemetry traces, logs, and \
metrics. Start an investigation with `summarize` and `anomalies` to find what \
is wrong, then `query_traces` to find failing traces, then `correlate` to pull \
every signal for one trace at once. Read the `tael://skill` resource for the \
full debugging playbook and `tael://llm-contract` for exact response shapes.";

// ── Tools ───────────────────────────────────────────────────────────

/// Shorthand for a JSON Schema string property.
fn s(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn n(description: &str) -> Value {
    json!({ "type": "number", "description": description })
}

fn tool(name: &str, description: &str, properties: Value, required: Vec<&str>) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        },
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "query_traces",
            "Search spans by service, operation, status, duration, time window, \
             span attributes, or full-text over LLM prompt/completion payloads. \
             The primary way to find failing or slow requests.",
            json!({
                "service": s("Exact service name"),
                "operation": s("Operation name (substring match)"),
                "status": s("ok, error, or unset"),
                "min_duration_ms": n("Only spans at least this long"),
                "max_duration_ms": n("Only spans at most this long"),
                "last": s("Time window, e.g. 15m, 1h, 7d"),
                "limit": n("Max spans to return (default 100)"),
                "attributes": json!({
                    "type": "object",
                    "description": "Span attribute equality filters, ANDed. Matching is exact.",
                    "additionalProperties": { "type": "string" },
                }),
                "text": s("Full-text query over indexed LLM prompt/completion payloads"),
                "explain": json!({
                    "type": "boolean",
                    "description": "Also report how the query executed: access path, \
                                    tiers consulted, rows scanned vs returned",
                }),
            }),
            vec![],
        ),
        tool(
            "get_trace",
            "Fetch every span of one trace, ordered by start time. Use after \
             query_traces to see the full call tree.",
            json!({ "trace_id": s("The trace ID") }),
            vec!["trace_id"],
        ),
        tool(
            "correlate",
            "Pull spans, logs, and surrounding metrics for a single trace in one \
             call. The fastest way to see everything about one failed request.",
            json!({ "trace_id": s("The trace ID") }),
            vec!["trace_id"],
        ),
        tool(
            "query_logs",
            "Search logs by service, severity, body substring, or trace ID.",
            json!({
                "service": s("Exact service name"),
                "severity": s("trace, debug, info, warn, error, or fatal"),
                "body_contains": s("Substring match on the log body (not a regex)"),
                "trace_id": s("Only logs belonging to this trace"),
                "last": s("Time window, e.g. 15m, 1h, 7d"),
                "limit": n("Max records to return (default 100)"),
            }),
            vec![],
        ),
        tool(
            "query_metrics",
            "Query metrics by filter, or evaluate a PromQL-subset expression. \
             Supports rate(), sum/avg/min/max/count with `by`, and \
             histogram_quantile(phi, metric) for percentiles from stored buckets.",
            json!({
                "query": s("PromQL-subset expression. Takes precedence over the filters below."),
                "service": s("Exact service name"),
                "name": s("Metric name"),
                "metric_type": s("gauge, sum, histogram, or summary"),
                "last": s("Time window, e.g. 15m, 1h, 7d"),
                "limit": n("Max points to return (default 500)"),
            }),
            vec![],
        ),
        tool(
            "services",
            "List known services with span count, trace count, average duration, \
             and error rate. A good first look at overall health.",
            json!({}),
            vec![],
        ),
        tool(
            "summarize",
            "Aggregated health digest over a window: span and error counts, \
             latency percentiles, top services, top error operations, log \
             severity breakdown, metric volume.",
            json!({
                "last": s("Time window, e.g. 15m, 1h, 7d (default 1h)"),
                "service": s("Restrict to one service"),
            }),
            vec![],
        ),
        tool(
            "anomalies",
            "Surface services whose error rate or p95 latency regressed against a \
             baseline window. Use right after summarize to find what changed.",
            json!({
                "last": s("Current window (default 1h)"),
                "baseline": s("Baseline window to compare against (default 6x current)"),
                "service": s("Restrict to one service"),
            }),
            vec![],
        ),
        tool(
            "query_sql",
            "Read-only SQL (SELECT/WITH only) over the spans, logs, metrics, and \
             trace_comments tables. The escape hatch for aggregations the other \
             tools do not cover.",
            json!({ "query": s("The SQL query") }),
            vec!["query"],
        ),
        tool(
            "add_comment",
            "Annotate a trace (or one span) with a durable note. Comments persist \
             across sessions and are how findings are recorded for later.",
            json!({
                "trace_id": s("The trace to annotate"),
                "body": s("Comment text"),
                "author": s("Who is commenting (default: mcp)"),
                "span_id": s("Optional span to attach the comment to"),
            }),
            vec!["trace_id", "body"],
        ),
        tool(
            "get_comments",
            "Read comments on a trace, including issue records, eval case \
             provenance, and self-diagnostics stored as structured comments.",
            json!({ "trace_id": s("The trace ID") }),
            vec!["trace_id"],
        ),
        tool(
            "topology",
            "Service dependency graph reconstructed from span parent/child \
             edges, with call counts and error rates per edge. Use it to find \
             which downstream dependency an error rate is coming from.",
            json!({
                "last": s("Time window, e.g. 1h, 24h"),
                "limit": n("Max spans to examine (default 50000)"),
            }),
            vec![],
        ),
        tool(
            "diff",
            "Compare every summary metric between a window and a baseline \
             window, reporting current, baseline, delta, and ratio with no \
             threshold applied. Use for a specific change; use anomalies to \
             ask whether anything is wrong at all.",
            json!({
                "last": s("Current window (default 1h)"),
                "baseline": s("Baseline window (default 6x current)"),
                "service": s("Restrict to one service"),
            }),
            vec![],
        ),
        tool(
            "get_metric",
            "Describe one metric before querying it: type, unit, label keys, \
             series count, value range, and whether its points retained \
             histogram buckets (which decides if histogram_quantile works).",
            json!({
                "name": s("The metric name"),
                "last": s("Time window, e.g. 1h, 24h"),
            }),
            vec!["name"],
        ),
        tool(
            "list_alerts",
            "Alert rules and their current state (ok, pending, firing).",
            json!({}),
            vec![],
        ),
        tool(
            "alert_events",
            "Recent alert state transitions, newest first.",
            json!({ "limit": n("Max events to return (default 50)") }),
            vec![],
        ),
        tool("eval_runs", "List recent eval runs.", json!({}), vec![]),
        tool(
            "eval_status",
            "Summary of one eval run: case counts, scores, pass rate.",
            json!({ "run_id": s("The eval run ID") }),
            vec!["run_id"],
        ),
        tool(
            "eval_compare",
            "Compare an eval run against a baseline run, reporting per-metric \
             score deltas. Use to check whether a change improved or regressed.",
            json!({
                "run_id": s("Current eval run ID"),
                "baseline_run_id": s("Baseline eval run ID"),
            }),
            vec!["run_id", "baseline_run_id"],
        ),
    ]
}

async fn call_tool(client: &TaelClient, params: &Value) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call requires a `name`"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let str_arg = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
    let num_arg = |key: &str| args.get(key).and_then(Value::as_f64);
    let required = |key: &str| -> Result<String, RpcError> {
        str_arg(key)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("`{name}` requires `{key}`")))
    };

    let result: Result<Value> = match name {
        "query_traces" => {
            let attributes: Vec<(String, String)> = args
                .get("attributes")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            client
                .query_traces(
                    str_arg("service").as_deref(),
                    str_arg("operation").as_deref(),
                    num_arg("min_duration_ms"),
                    num_arg("max_duration_ms"),
                    str_arg("status").as_deref(),
                    str_arg("last").as_deref(),
                    num_arg("limit").unwrap_or(100.0) as u32,
                    &attributes,
                    str_arg("text").as_deref(),
                    args.get("explain")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
                .await
        }
        "get_trace" => client.get_trace(&required("trace_id")?).await,
        "correlate" => client.correlate(&required("trace_id")?).await,
        "query_logs" => {
            client
                .query_logs(
                    str_arg("service").as_deref(),
                    str_arg("severity").as_deref(),
                    str_arg("body_contains").as_deref(),
                    str_arg("trace_id").as_deref(),
                    str_arg("last").as_deref(),
                    num_arg("limit").unwrap_or(100.0) as u32,
                )
                .await
        }
        "query_metrics" => match str_arg("query") {
            Some(q) => client.promql_query(&q, str_arg("last").as_deref()).await,
            None => {
                client
                    .query_metrics(
                        str_arg("service").as_deref(),
                        str_arg("name").as_deref(),
                        str_arg("metric_type").as_deref(),
                        str_arg("last").as_deref(),
                        num_arg("limit").unwrap_or(500.0) as u32,
                    )
                    .await
            }
        },
        "services" => client.list_services().await,
        "summarize" => {
            client
                .summary(str_arg("last").as_deref(), str_arg("service").as_deref())
                .await
        }
        "anomalies" => {
            client
                .anomalies(
                    str_arg("last").as_deref(),
                    str_arg("baseline").as_deref(),
                    str_arg("service").as_deref(),
                )
                .await
        }
        "query_sql" => client.query_sql(&required("query")?).await,
        "add_comment" => {
            client
                .add_comment(
                    &required("trace_id")?,
                    &required("body")?,
                    Some(str_arg("author").as_deref().unwrap_or("mcp")),
                    str_arg("span_id").as_deref(),
                )
                .await
        }
        "get_comments" => client.get_comments(&required("trace_id")?).await,
        "topology" => {
            client
                .topology(
                    str_arg("last").as_deref(),
                    num_arg("limit").unwrap_or(50_000.0) as u32,
                )
                .await
        }
        "diff" => {
            client
                .diff(
                    str_arg("last").as_deref(),
                    str_arg("baseline").as_deref(),
                    str_arg("service").as_deref(),
                )
                .await
        }
        "get_metric" => {
            client
                .get_metric(
                    &required("name")?,
                    str_arg("last").as_deref(),
                    num_arg("limit").unwrap_or(500.0) as u32,
                )
                .await
        }
        "list_alerts" => client.list_alerts().await,
        "alert_events" => {
            client
                .alert_events(num_arg("limit").unwrap_or(50.0) as u32)
                .await
        }
        "eval_runs" => client.eval_runs().await,
        "eval_status" => client.eval_status(&required("run_id")?).await,
        "eval_compare" => {
            client
                .eval_compare(&required("run_id")?, &required("baseline_run_id")?)
                .await
        }
        other => {
            return Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown tool `{other}`"),
            ));
        }
    };

    match result {
        Ok(value) => Ok(tool_result(&value, false)),
        // A tool failure is reported inside the result with isError, not as a
        // JSON-RPC error: the agent should see it and adapt, not treat the
        // whole call as malformed.
        Err(e) => Ok(tool_result(&json!({ "error": e.to_string() }), true)),
    }
}

/// Wrap a JSON payload as an MCP tool result.
///
/// The payload goes in a text block as compact JSON rather than a structured
/// field, because that is what every current MCP client renders into the
/// model's context, and the JSON shapes are already the documented contract.
fn tool_result(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        }],
        "isError": is_error,
    })
}

// ── Resources ───────────────────────────────────────────────────────

/// `SKILL.md` and `llm.txt` shipped inside the binary, so a connected agent can
/// self-onboard without the files being installed anywhere.
const SKILL_MD: &str = include_str!("../../SKILL.md");
const LLM_TXT: &str = include_str!("../../llm.txt");

fn resource_definitions() -> Vec<Value> {
    vec![
        json!({
            "uri": "tael://skill",
            "name": "tael debugging playbook",
            "description": "How to investigate a production problem with tael: \
                            order of operations, investigation playbook, \
                            instrumentation doctrine, and data caveats.",
            "mimeType": "text/markdown",
        }),
        json!({
            "uri": "tael://llm-contract",
            "name": "tael query contract",
            "description": "Exact flag semantics, JSON response shapes, PromQL \
                            subset grammar, and data-fidelity caveats.",
            "mimeType": "text/plain",
        }),
    ]
}

fn read_resource(params: &Value) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, "resources/read requires a `uri`"))?;

    let (text, mime) = match uri {
        "tael://skill" => (SKILL_MD, "text/markdown"),
        "tael://llm-contract" => (LLM_TXT, "text/plain"),
        other => {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown resource `{other}`"),
            ));
        }
    };

    Ok(json!({
        "contents": [{ "uri": uri, "mimeType": mime, "text": text }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> TaelClient {
        TaelClient::new("http://127.0.0.1:1")
    }

    #[tokio::test]
    async fn initialize_reports_capabilities_and_echoes_the_client_version() {
        let result = dispatch(
            &client(),
            "initialize",
            &json!({ "protocolVersion": "2024-11-05" }),
        )
        .await
        .unwrap_or_else(|e| panic!("{}", e.message));

        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "tael");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
        assert!(
            result["instructions"]
                .as_str()
                .unwrap()
                .contains("summarize"),
            "instructions should point at the entry-point tool"
        );
    }

    #[tokio::test]
    async fn every_tool_has_a_schema_and_a_handler() {
        let listed = dispatch(&client(), "tools/list", &json!({}))
            .await
            .ok()
            .unwrap();
        let tools = listed["tools"].as_array().unwrap();
        assert!(!tools.is_empty());

        for t in tools {
            let name = t["name"].as_str().unwrap();
            assert!(
                !t["description"].as_str().unwrap_or_default().is_empty(),
                "{name} needs a description — it is what the agent selects on"
            );
            assert_eq!(t["inputSchema"]["type"], "object", "{name}");

            // Required fields must be declared in properties, or a client
            // cannot construct a valid call.
            for required in t["inputSchema"]["required"].as_array().unwrap() {
                let key = required.as_str().unwrap();
                assert!(
                    !t["inputSchema"]["properties"][key].is_null(),
                    "{name} requires `{key}` but does not describe it"
                );
            }

            // Dispatching with empty args must reach the handler rather than
            // fall through to "unknown tool".
            let called = call_tool(&client(), &json!({ "name": name, "arguments": {} })).await;
            match called {
                Ok(result) => assert!(
                    result["content"].is_array(),
                    "{name} returned a malformed tool result"
                ),
                Err(e) => assert_ne!(
                    e.code, METHOD_NOT_FOUND,
                    "{name} is listed but has no handler"
                ),
            }
        }
    }

    #[tokio::test]
    async fn unknown_methods_and_tools_are_reported_distinctly() {
        let err = dispatch(&client(), "nope/nope", &json!({}))
            .await
            .expect_err("unknown method should fail");
        assert_eq!(err.code, METHOD_NOT_FOUND);

        let err = call_tool(&client(), &json!({ "name": "not_a_tool" }))
            .await
            .expect_err("unknown tool should fail");
        assert_eq!(err.code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_required_arguments_are_rejected_before_any_request() {
        let err = call_tool(&client(), &json!({ "name": "get_trace", "arguments": {} }))
            .await
            .expect_err("get_trace without a trace_id should fail");
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("trace_id"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_failed_backend_call_is_an_error_result_not_a_protocol_error() {
        // The client points at a closed port, so the request cannot succeed.
        let result = call_tool(&client(), &json!({ "name": "services", "arguments": {} }))
            .await
            .expect("backend failure should not be a JSON-RPC error");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("error")
        );
    }

    #[tokio::test]
    async fn resources_serve_the_onboarding_documents() {
        let listed = dispatch(&client(), "resources/list", &json!({}))
            .await
            .ok()
            .unwrap();
        let uris: Vec<&str> = listed["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"tael://skill"));
        assert!(uris.contains(&"tael://llm-contract"));

        for uri in uris {
            let read = read_resource(&json!({ "uri": uri })).unwrap();
            let text = read["contents"][0]["text"].as_str().unwrap();
            assert!(!text.is_empty(), "{uri} served an empty document");
        }

        assert!(read_resource(&json!({ "uri": "tael://nope" })).is_err());
    }
}
