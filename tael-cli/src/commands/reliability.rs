use anyhow::Result;
use serde_json::Value;

use crate::client::TaelClient;

pub async fn comment_rows(client: &TaelClient, limit: u32) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT id, trace_id, span_id, author, body, created_at::VARCHAR AS created_at \
         FROM trace_comments ORDER BY created_at DESC LIMIT {limit}"
    );
    let result = client.query_sql(&sql).await?;
    Ok(result
        .get("rows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

pub fn enriched_comment(row: &Value) -> Option<Value> {
    let body = row.get("body")?.as_str()?;
    let parsed: Value = serde_json::from_str(body).ok()?;
    let mut obj = parsed.as_object()?.clone();
    obj.insert(
        "comment_id".to_string(),
        row.get("id").cloned().unwrap_or(Value::Null),
    );
    obj.insert(
        "trace_id".to_string(),
        row.get("trace_id").cloned().unwrap_or(Value::Null),
    );
    obj.insert(
        "span_id".to_string(),
        row.get("span_id").cloned().unwrap_or(Value::Null),
    );
    obj.insert(
        "author".to_string(),
        row.get("author").cloned().unwrap_or(Value::Null),
    );
    obj.insert(
        "created_at".to_string(),
        row.get("created_at").cloned().unwrap_or(Value::Null),
    );
    Some(Value::Object(obj))
}

pub fn kind(value: &Value) -> Option<&str> {
    value.get("kind").and_then(|v| v.as_str())
}

pub fn field<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(|v| v.as_str()).unwrap_or("-")
}

pub fn short_trace(trace: &str) -> String {
    if trace.len() > 12 {
        format!("{}...", &trace[..12])
    } else {
        trace.to_string()
    }
}
