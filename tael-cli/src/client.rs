use std::time::Duration;

use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::mpsc;

pub struct TaelClient {
    base_url: String,
    http: Client,
}

impl TaelClient {
    pub fn new(base_url: &str) -> Self {
        if let Some(socket_path) = base_url.strip_prefix("unix://") {
            return Self::new_unix_socket(socket_path)
                .expect("failed to build Unix-socket HTTP client");
        }

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    #[cfg(unix)]
    pub fn new_unix_socket(socket_path: &str) -> Result<Self> {
        Ok(Self {
            base_url: "http://tael".to_string(),
            http: Client::builder().unix_socket(socket_path).build()?,
        })
    }

    #[cfg(not(unix))]
    pub fn new_unix_socket(_socket_path: &str) -> Result<Self> {
        anyhow::bail!("Unix sockets are only supported on Unix platforms");
    }

    pub fn subscribe_live(
        &self,
        service: Option<String>,
        status: Option<String>,
    ) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let http = self.http.clone();
        let base_url = self.base_url.clone();

        tokio::spawn(async move {
            loop {
                match sse_read_loop(&http, &base_url, service.as_deref(), status.as_deref(), &tx)
                    .await
                {
                    Ok(()) => break,
                    Err(_) => {
                        if tx.is_closed() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        });

        rx
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn query_traces(
        &self,
        service: Option<&str>,
        operation: Option<&str>,
        min_duration_ms: Option<f64>,
        max_duration_ms: Option<f64>,
        status: Option<&str>,
        last: Option<&str>,
        limit: u32,
        attributes: &[(String, String)],
        text: Option<&str>,
    ) -> Result<Value> {
        let mut params = vec![("limit", limit.to_string())];
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        if let Some(o) = operation {
            params.push(("operation", o.to_string()));
        }
        if let Some(d) = min_duration_ms {
            params.push(("min_duration_ms", d.to_string()));
        }
        if let Some(d) = max_duration_ms {
            params.push(("max_duration_ms", d.to_string()));
        }
        if let Some(s) = status {
            params.push(("status", s.to_string()));
        }
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }
        for (k, v) in attributes {
            params.push(("attribute", format!("{k}={v}")));
        }
        if let Some(t) = text {
            params.push(("text", t.to_string()));
        }

        let resp = self
            .http
            .get(format!("{}/api/v1/traces", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(resp)
    }

    /// The most recent comments across all traces, newest first
    /// (`/api/v1/comments`). Errors on older servers without the endpoint —
    /// callers fall back to the SQL layer.
    pub async fn list_comments(&self, limit: u32) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/comments", self.base_url))
            .query(&[("limit", limit.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn query_sql(&self, query: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/sql", self.base_url))
            .query(&[("q", query)])
            .send()
            .await?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn get_trace(&self, trace_id: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/traces/{}", self.base_url, trace_id))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(resp)
    }

    pub async fn list_services(&self) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/services", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(resp)
    }

    pub async fn query_logs(
        &self,
        service: Option<&str>,
        severity: Option<&str>,
        body_contains: Option<&str>,
        trace_id: Option<&str>,
        last: Option<&str>,
        limit: u32,
    ) -> Result<Value> {
        let mut params = vec![("limit", limit.to_string())];
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        if let Some(s) = severity {
            params.push(("severity", s.to_string()));
        }
        if let Some(b) = body_contains {
            params.push(("body_contains", b.to_string()));
        }
        if let Some(t) = trace_id {
            params.push(("trace_id", t.to_string()));
        }
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }

        let resp = self
            .http
            .get(format!("{}/api/v1/logs", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(resp)
    }

    pub async fn query_metrics(
        &self,
        service: Option<&str>,
        name: Option<&str>,
        metric_type: Option<&str>,
        last: Option<&str>,
        limit: u32,
    ) -> Result<Value> {
        let mut params = vec![("limit", limit.to_string())];
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        if let Some(n) = name {
            params.push(("name", n.to_string()));
        }
        if let Some(t) = metric_type {
            params.push(("metric_type", t.to_string()));
        }
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }

        let resp = self
            .http
            .get(format!("{}/api/v1/metrics", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;

        Ok(resp)
    }

    pub async fn promql_query(&self, query: &str, last: Option<&str>) -> Result<Value> {
        let mut params = vec![("query", query.to_string())];
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }
        let resp = self
            .http
            .get(format!("{}/api/v1/metrics/query", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn add_comment(
        &self,
        trace_id: &str,
        body: &str,
        author: Option<&str>,
        span_id: Option<&str>,
    ) -> Result<Value> {
        let mut payload = serde_json::json!({ "body": body });
        if let Some(a) = author {
            payload["author"] = serde_json::json!(a);
        }
        if let Some(s) = span_id {
            payload["span_id"] = serde_json::json!(s);
        }
        let resp = self
            .http
            .post(format!(
                "{}/api/v1/traces/{}/comments",
                self.base_url, trace_id
            ))
            .json(&payload)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn get_comments(&self, trace_id: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/traces/{}/comments",
                self.base_url, trace_id
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn summary(&self, last: Option<&str>, service: Option<&str>) -> Result<Value> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        let resp = self
            .http
            .get(format!("{}/api/v1/summary", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn anomalies(
        &self,
        last: Option<&str>,
        baseline: Option<&str>,
        service: Option<&str>,
    ) -> Result<Value> {
        let mut params: Vec<(&str, String)> = Vec::new();
        if let Some(l) = last {
            params.push(("last", l.to_string()));
        }
        if let Some(b) = baseline {
            params.push(("baseline", b.to_string()));
        }
        if let Some(s) = service {
            params.push(("service", s.to_string()));
        }
        let resp = self
            .http
            .get(format!("{}/api/v1/anomalies", self.base_url))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn correlate(&self, trace: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/correlate", self.base_url))
            .query(&[("trace", trace)])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn eval_runs(&self) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/evals/runs", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn eval_status(&self, run_id: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{}/api/v1/evals/runs/{}", self.base_url, run_id))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn eval_cases(&self, run_id: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/evals/runs/{}/cases",
                self.base_url, run_id
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn eval_scores(&self, run_id: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/evals/runs/{}/scores",
                self.base_url, run_id
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn eval_compare(&self, run_id: &str, baseline: &str) -> Result<Value> {
        let resp = self
            .http
            .get(format!(
                "{}/api/v1/evals/runs/{}/compare",
                self.base_url, run_id
            ))
            .query(&[("baseline", baseline)])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn add_eval_score(&self, payload: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/api/v1/evals/scores", self.base_url))
            .json(payload)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn put_blob(&self, content: &str) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/api/v1/blobs", self.base_url))
            .body(content.to_string())
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn add_eval_runner_span(&self, payload: &Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}/api/v1/evals/runner-spans", self.base_url))
            .json(payload)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn healthz(&self) -> Result<String> {
        let resp = self
            .http
            .get(format!("{}/healthz", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        Ok(resp)
    }
}

async fn sse_read_loop(
    http: &Client,
    base_url: &str,
    service: Option<&str>,
    status: Option<&str>,
    tx: &mpsc::UnboundedSender<String>,
) -> Result<()> {
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = service {
        params.push(("service", s));
    }
    if let Some(s) = status {
        params.push(("status", s));
    }

    let mut response = http
        .get(format!("{base_url}/api/v1/traces/live"))
        .query(&params)
        .send()
        .await?
        .error_for_status()?;

    let mut buffer = String::new();
    loop {
        match response.chunk().await? {
            Some(chunk) => {
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(pos) = buffer.find("\n\n") {
                    let event_block = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    for line in event_block.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            let data = data.trim();
                            if !data.is_empty() {
                                if tx.send(data.to_string()).is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
            None => break,
        }
    }

    Ok(())
}
