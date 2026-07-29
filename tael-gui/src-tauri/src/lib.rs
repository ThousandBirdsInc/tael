use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Emitter;

#[derive(Clone)]
struct InitialServer(String);

#[derive(Clone)]
struct HttpTarget {
    base_url: String,
    http: Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceQueryRequest {
    service: Option<String>,
    operation: Option<String>,
    min_duration_ms: Option<f64>,
    max_duration_ms: Option<f64>,
    status: Option<String>,
    last: Option<String>,
    limit: Option<u32>,
    attributes: Option<Vec<AttributeFilter>>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AttributeFilter {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddCommentRequest {
    trace_id: String,
    body: String,
    author: Option<String>,
    span_id: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LivePayload {
    stream_id: String,
    data: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveStatusPayload {
    stream_id: String,
    status: String,
    message: Option<String>,
}

fn target(server: &str) -> Result<HttpTarget, String> {
    if let Some(socket_path) = server.strip_prefix("unix://") {
        return unix_target(socket_path);
    }

    Ok(HttpTarget {
        base_url: server.trim_end_matches('/').to_string(),
        http: Client::new(),
    })
}

#[cfg(unix)]
fn unix_target(socket_path: &str) -> Result<HttpTarget, String> {
    Ok(HttpTarget {
        base_url: "http://tael".to_string(),
        http: Client::builder()
            .unix_socket(socket_path)
            .build()
            .map_err(to_string)?,
    })
}

#[cfg(not(unix))]
fn unix_target(_socket_path: &str) -> Result<HttpTarget, String> {
    Err("Unix sockets are only supported on Unix platforms".to_string())
}

fn to_string<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

async fn get_json(server: &str, path: &str, params: &[(String, String)]) -> Result<Value, String> {
    let target = target(server)?;
    let response = target
        .http
        .get(format!("{}{}", target.base_url, path))
        .query(params)
        .send()
        .await
        .map_err(to_string)?;

    // The server explains its refusals in the body — a build without the SQL
    // engine answers 400 with the feature to install, a bad PromQL expression
    // answers with the parse error. `error_for_status` would throw all of that
    // away and leave the user with a status code, so the body is read either
    // way and its `error` field preferred over the status line.
    let status = response.status();
    let body = response.text().await.map_err(to_string)?;
    if !status.is_success() {
        return Err(error_message(status, &body));
    }
    serde_json::from_str::<Value>(&body).map_err(to_string)
}

/// The most useful thing that can be said about a failed request.
///
/// In order of preference: the server's own `error` field, the raw body, then
/// the status line. Only the first is actually helpful — "this build has no SQL
/// engine, reinstall with --features sql" versus "400 Bad Request" — which is
/// why the body is read even on failure.
fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                format!("request failed: {status}")
            } else {
                body.to_string()
            }
        })
}

#[tauri::command]
async fn healthz(server: String) -> Result<String, String> {
    let target = target(&server)?;
    target
        .http
        .get(format!("{}/healthz", target.base_url))
        .send()
        .await
        .map_err(to_string)?
        .error_for_status()
        .map_err(to_string)?
        .text()
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn query_traces(server: String, request: TraceQueryRequest) -> Result<Value, String> {
    let mut params = vec![(
        "limit".to_string(),
        request.limit.unwrap_or(200).to_string(),
    )];
    if let Some(service) = request.service {
        params.push(("service".to_string(), service));
    }
    if let Some(operation) = request.operation {
        params.push(("operation".to_string(), operation));
    }
    if let Some(duration) = request.min_duration_ms {
        params.push(("min_duration_ms".to_string(), duration.to_string()));
    }
    if let Some(duration) = request.max_duration_ms {
        params.push(("max_duration_ms".to_string(), duration.to_string()));
    }
    if let Some(status) = request.status {
        params.push(("status".to_string(), status));
    }
    if let Some(last) = request.last {
        params.push(("last".to_string(), last));
    }
    if let Some(text) = request.text
        && !text.trim().is_empty()
    {
        params.push(("text".to_string(), text));
    }
    if let Some(attributes) = request.attributes {
        for attr in attributes {
            if !attr.key.trim().is_empty() {
                params.push((
                    "attribute".to_string(),
                    format!("{}={}", attr.key, attr.value),
                ));
            }
        }
    }

    get_json(&server, "/api/v1/traces", &params).await
}

#[tauri::command]
async fn list_services(server: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/services", &[]).await
}

#[tauri::command]
async fn get_trace(server: String, trace_id: String) -> Result<Value, String> {
    get_json(&server, &format!("/api/v1/traces/{trace_id}"), &[]).await
}

#[tauri::command]
async fn get_comments(server: String, trace_id: String) -> Result<Value, String> {
    get_json(&server, &format!("/api/v1/traces/{trace_id}/comments"), &[]).await
}

#[tauri::command]
async fn add_comment(server: String, request: AddCommentRequest) -> Result<Value, String> {
    let target = target(&server)?;
    let mut payload = serde_json::json!({ "body": request.body });
    if let Some(author) = request.author {
        payload["author"] = serde_json::json!(author);
    }
    if let Some(span_id) = request.span_id {
        payload["span_id"] = serde_json::json!(span_id);
    }

    target
        .http
        .post(format!(
            "{}/api/v1/traces/{}/comments",
            target.base_url, request.trace_id
        ))
        .json(&payload)
        .send()
        .await
        .map_err(to_string)?
        .error_for_status()
        .map_err(to_string)?
        .json::<Value>()
        .await
        .map_err(to_string)
}

#[tauri::command]
async fn eval_runs(server: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/evals/runs", &[]).await
}

#[tauri::command]
async fn eval_status(server: String, run_id: String) -> Result<Value, String> {
    get_json(&server, &format!("/api/v1/evals/runs/{run_id}"), &[]).await
}

#[tauri::command]
async fn eval_cases(server: String, run_id: String) -> Result<Value, String> {
    get_json(&server, &format!("/api/v1/evals/runs/{run_id}/cases"), &[]).await
}

#[tauri::command]
async fn eval_compare(server: String, run_id: String, baseline: String) -> Result<Value, String> {
    let params = vec![("baseline".to_string(), baseline)];
    get_json(
        &server,
        &format!("/api/v1/evals/runs/{run_id}/compare"),
        &params,
    )
    .await
}

// ── Panels beyond traces/services/evals ─────────────────────────────
//
// Each is a thin pass-through to the REST surface the CLI already uses, so the
// GUI and `tael live` are looking at the same numbers. They are read-only:
// creating an alert rule or filing a review request is an agent's job and stays
// in the CLI where it can be scripted and its exit code checked.

#[tauri::command]
async fn query_summary(server: String, last: Option<String>) -> Result<Value, String> {
    let params = last
        .map(|l| vec![("last".to_string(), l)])
        .unwrap_or_default();
    get_json(&server, "/api/v1/summary", &params).await
}

#[tauri::command]
async fn query_anomalies(
    server: String,
    last: Option<String>,
    baseline: Option<String>,
) -> Result<Value, String> {
    let mut params = Vec::new();
    if let Some(last) = last {
        params.push(("last".to_string(), last));
    }
    if let Some(baseline) = baseline {
        params.push(("baseline".to_string(), baseline));
    }
    get_json(&server, "/api/v1/anomalies", &params).await
}

#[tauri::command]
async fn query_topology(server: String, last: Option<String>) -> Result<Value, String> {
    let mut params = vec![("limit".to_string(), "50000".to_string())];
    if let Some(last) = last {
        params.push(("last".to_string(), last));
    }
    get_json(&server, "/api/v1/topology", &params).await
}

#[tauri::command]
async fn list_alerts(server: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/alerts", &[]).await
}

#[tauri::command]
async fn alert_events(server: String, limit: Option<u32>) -> Result<Value, String> {
    let params = vec![("limit".to_string(), limit.unwrap_or(20).to_string())];
    get_json(&server, "/api/v1/alerts/events", &params).await
}

#[tauri::command]
async fn list_score_rules(server: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/scores/rules", &[]).await
}

#[tauri::command]
async fn cluster_traces(server: String, k: Option<u32>) -> Result<Value, String> {
    let params = vec![("k".to_string(), k.unwrap_or(5).to_string())];
    get_json(&server, "/api/v1/cluster", &params).await
}

#[tauri::command]
async fn similar_traces(
    server: String,
    trace_id: String,
    limit: Option<u32>,
) -> Result<Value, String> {
    let params = vec![("limit".to_string(), limit.unwrap_or(10).to_string())];
    get_json(&server, &format!("/api/v1/similar/{trace_id}"), &params).await
}

#[tauri::command]
async fn list_comments(server: String, limit: Option<u32>) -> Result<Value, String> {
    let params = vec![("limit".to_string(), limit.unwrap_or(500).to_string())];
    get_json(&server, "/api/v1/comments", &params).await
}

#[tauri::command]
async fn list_suites(server: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/evals/suites", &[]).await
}

/// Read-only SQL. The server answers a build without the engine with a 400
/// whose body names the feature to install, so the error is forwarded verbatim
/// rather than flattened into "request failed" — that message is the useful
/// part.
#[tauri::command]
async fn query_sql(server: String, query: String) -> Result<Value, String> {
    get_json(&server, "/api/v1/sql", &[("q".to_string(), query)]).await
}

#[tauri::command]
async fn start_live_stream(
    app: tauri::AppHandle,
    server: String,
    service: Option<String>,
    status: Option<String>,
    stream_id: String,
) -> Result<(), String> {
    let target = target(&server)?;
    tauri::async_runtime::spawn(async move {
        loop {
            let _ = app.emit(
                "tael://live-status",
                LiveStatusPayload {
                    stream_id: stream_id.clone(),
                    status: "connecting".to_string(),
                    message: None,
                },
            );

            match sse_read_loop(
                &target,
                service.as_deref(),
                status.as_deref(),
                &app,
                &stream_id,
            )
            .await
            {
                Ok(()) => {
                    let _ = app.emit(
                        "tael://live-status",
                        LiveStatusPayload {
                            stream_id: stream_id.clone(),
                            status: "closed".to_string(),
                            message: None,
                        },
                    );
                    break;
                }
                Err(err) => {
                    let _ = app.emit(
                        "tael://live-status",
                        LiveStatusPayload {
                            stream_id: stream_id.clone(),
                            status: "retrying".to_string(),
                            message: Some(err),
                        },
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn initial_server(server: tauri::State<'_, InitialServer>) -> String {
    server.0.clone()
}

async fn sse_read_loop(
    target: &HttpTarget,
    service: Option<&str>,
    status: Option<&str>,
    app: &tauri::AppHandle,
    stream_id: &str,
) -> Result<(), String> {
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(service) = service {
        params.push(("service", service));
    }
    if let Some(status) = status {
        params.push(("status", status));
    }

    let mut response = target
        .http
        .get(format!("{}/api/v1/traces/live", target.base_url))
        .query(&params)
        .send()
        .await
        .map_err(to_string)?
        .error_for_status()
        .map_err(to_string)?;

    let _ = app.emit(
        "tael://live-status",
        LiveStatusPayload {
            stream_id: stream_id.to_string(),
            status: "connected".to_string(),
            message: None,
        },
    );

    let mut buffer = String::new();
    loop {
        let Some(chunk) = response.chunk().await.map_err(to_string)? else {
            break;
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let event_block = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in event_block.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if !data.is_empty() {
                        app.emit(
                            "tael://live-spans",
                            LivePayload {
                                stream_id: stream_id.to_string(),
                                data: data.to_string(),
                            },
                        )
                        .map_err(to_string)?;
                    }
                }
            }
        }
    }

    Ok(())
}

pub fn run() {
    run_with_server("http://127.0.0.1:7701".to_string())
}

pub fn run_with_server(server: String) {
    tauri::Builder::default()
        .manage(InitialServer(server))
        .setup(|app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("Tael")
            .inner_size(1280.0, 820.0)
            .min_inner_size(980.0, 620.0)
            .resizable(true)
            .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            initial_server,
            healthz,
            query_traces,
            list_services,
            get_trace,
            get_comments,
            add_comment,
            eval_runs,
            eval_status,
            eval_cases,
            eval_compare,
            query_summary,
            query_anomalies,
            query_topology,
            list_alerts,
            alert_events,
            list_score_rules,
            cluster_traces,
            similar_traces,
            list_comments,
            list_suites,
            query_sql,
            start_live_stream
        ])
        .run(tauri::generate_context!())
        .expect("error while running tael gui");
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn a_servers_own_error_message_survives_to_the_ui() {
        // The case this exists for: a build without the SQL engine answers 400
        // with the feature to install. `error_for_status` would replace that
        // with the status line, leaving the user a number instead of the fix.
        let body = r#"{"error":"this build has no SQL engine. Reinstall with `--features sql`"}"#;
        assert_eq!(
            error_message(StatusCode::BAD_REQUEST, body),
            "this build has no SQL engine. Reinstall with `--features sql`"
        );
    }

    #[test]
    fn a_body_without_an_error_field_is_shown_verbatim() {
        // Better a raw body than a status code: an HTML error page from a proxy
        // at least says a proxy answered.
        assert_eq!(
            error_message(StatusCode::BAD_GATEWAY, "upstream connect error"),
            "upstream connect error"
        );
        assert_eq!(
            error_message(StatusCode::BAD_REQUEST, r#"{"detail":"nope"}"#),
            r#"{"detail":"nope"}"#
        );
    }

    #[test]
    fn an_empty_body_falls_back_to_the_status() {
        assert_eq!(
            error_message(StatusCode::NOT_FOUND, "   "),
            "request failed: 404 Not Found"
        );
    }
}
