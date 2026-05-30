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
    target
        .http
        .get(format!("{}{}", target.base_url, path))
        .query(params)
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
    let mut params = vec![("limit".to_string(), request.limit.unwrap_or(200).to_string())];
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
    if let Some(text) = request.text {
        if !text.trim().is_empty() {
            params.push(("text".to_string(), text));
        }
    }
    if let Some(attributes) = request.attributes {
        for attr in attributes {
            if !attr.key.trim().is_empty() {
                params.push(("attribute".to_string(), format!("{}={}", attr.key, attr.value)));
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
    get_json(
        &server,
        &format!("/api/v1/traces/{trace_id}/comments"),
        &[],
    )
    .await
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
    get_json(
        &server,
        &format!("/api/v1/evals/runs/{run_id}/cases"),
        &[],
    )
    .await
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

            match sse_read_loop(&target, service.as_deref(), status.as_deref(), &app, &stream_id)
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
                tauri::WebviewUrl::External(inline_gui_data_url().parse()?),
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
            start_live_stream
        ])
        .run(tauri::generate_context!())
        .expect("error while running tael gui");
}

fn inline_gui_data_url() -> String {
    let html = include_str!("../dist/index.html")
        .replace(
            r#"<script type="module" src="./assets/index.js"></script>"#,
            &format!(
                r#"<script type="module">{}</script>"#,
                escape_inline_script(include_str!("../dist/assets/index.js"))
            ),
        )
        .replace(
            r#"<link rel="stylesheet" href="./assets/index.css">"#,
            &format!(
                r#"<style>{}</style>"#,
                escape_inline_style(include_str!("../dist/assets/index.css"))
            ),
        );
    format!("data:text/html;charset=utf-8,{}", percent_encode(&html))
}

fn escape_inline_script(value: &str) -> String {
    value.replace("</script", "<\\/script")
}

fn escape_inline_style(value: &str) -> String {
    value.replace("</style", "<\\/style")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push(hex(byte >> 4));
                encoded.push(hex(byte & 0x0f));
            }
        }
    }
    encoded
}

fn hex(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + (value - 10)) as char,
        _ => unreachable!(),
    }
}
