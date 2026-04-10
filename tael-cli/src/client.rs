use anyhow::Result;
use reqwest::Client;
use serde_json::Value;

pub struct TaelClient {
    base_url: String,
    http: Client,
}

impl TaelClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    pub async fn query_traces(
        &self,
        service: Option<&str>,
        operation: Option<&str>,
        min_duration_ms: Option<f64>,
        max_duration_ms: Option<f64>,
        status: Option<&str>,
        last: Option<&str>,
        limit: u32,
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
            .post(format!("{}/api/v1/traces/{}/comments", self.base_url, trace_id))
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
            .get(format!("{}/api/v1/traces/{}/comments", self.base_url, trace_id))
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
