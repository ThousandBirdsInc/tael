//! Datadog trace-agent (dd-trace) receiver.
//!
//! Implements the APM intake surface of the Datadog agent so services
//! instrumented with a `dd-trace` library can point straight at Tael:
//!
//! ```sh
//! export DD_TRACE_AGENT_URL=http://127.0.0.1:7701
//! ```
//!
//! Supported endpoints (mounted on the REST listener, see `api::rest`):
//! - `PUT|POST /v0.3/traces`, `/v0.4/traces` — msgpack (or JSON) payload of
//!   `[[span, ...], ...]` where each span is a map of named fields.
//! - `PUT|POST /v0.5/traces` — msgpack `[string_table, traces]` payload where
//!   each span is a 12-element array of string-table indices and scalars.
//! - `GET /info` — agent discovery; advertises the endpoints above so clients
//!   negotiate a supported version.
//! - `POST /v0.6/stats` and `POST /telemetry/proxy/*` — accepted and
//!   discarded so client background loops stay quiet.
//!
//! Spans are converted to Tael [`Span`]s: `meta`/`metrics` become attributes,
//! `error` maps to the span status, `span.kind` in meta maps to the kind, and
//! 64-bit Datadog trace ids are widened to 128-bit ids using the
//! `_dd.p.tid` propagated tag (the upper 64 bits) when present.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::span_bus::SpanBus;
use crate::storage::models::{Span, SpanKind, SpanStatus};
use crate::storage::{BlobStore, Store};

/// Wire format of a traces payload. `V04` covers v0.3 and v0.4 (identical
/// map-encoded spans; v0.4 only adds optional fields we skip); `V05` is the
/// string-table array encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracesVersion {
    V04,
    V05,
}

/// One span as sent by dd-trace clients on v0.3/v0.4 (msgpack map or JSON
/// object). Every field is optional on the wire; unknown fields
/// (`meta_struct`, `span_links`, ...) are skipped.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DdSpan {
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub trace_id: u64,
    #[serde(default)]
    pub span_id: u64,
    #[serde(default)]
    pub parent_id: u64,
    /// Start time, nanoseconds since the Unix epoch.
    #[serde(default)]
    pub start: i64,
    /// Duration in nanoseconds.
    #[serde(default)]
    pub duration: i64,
    #[serde(default)]
    pub error: i32,
    #[serde(default)]
    pub meta: HashMap<String, String>,
    #[serde(default)]
    pub metrics: HashMap<String, f64>,
    #[serde(default, rename = "type")]
    pub span_type: String,
}

// ── Handlers ────────────────────────────────────────────────────────

/// `GET /info` — the discovery document dd-trace clients fetch to pick an
/// intake version. Only advertise what we actually decode; clients fall back
/// to v0.4 when v0.5 is absent and vice versa. `client_drop_p0s: false` tells
/// clients to keep sending unsampled traces (Tael has no sampler; it stores
/// everything).
pub fn handle_info() -> Response {
    let body = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": [
            "/v0.3/traces",
            "/v0.4/traces",
            "/v0.5/traces",
        ],
        "client_drop_p0s": false,
        "feature_flags": [],
        "config": { "default_env": "none" },
    });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// `PUT|POST /v0.x/traces` — decode, convert, store, and publish the spans.
pub async fn handle_traces(
    store: Arc<dyn Store>,
    blobs: Arc<BlobStore>,
    bus: Arc<SpanBus>,
    version: TracesVersion,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // dd-trace clients flush on a timer and may send an empty body (or an
    // empty msgpack array) as a keep-alive; ack those without decoding.
    if body.is_empty() {
        return sampling_response();
    }

    let chunks = match decode_traces(version, &headers, &body) {
        Ok(chunks) => chunks,
        Err(e) => {
            tracing::warn!(error = %e, ?version, "datadog trace decode failed");
            return (StatusCode::BAD_REQUEST, format!("trace decode error: {e}")).into_response();
        }
    };

    let mut spans: Vec<Span> = Vec::new();
    for chunk in &chunks {
        spans.extend(convert_chunk(chunk));
    }

    // Mirror the OTLP ingest path: move LLM payload text out of the columnar
    // attributes into the content-addressed blob store, keeping only hashes.
    for span in &mut spans {
        if span.llm.is_some() {
            let prompt = span.attributes.remove("gen_ai.prompt");
            let completion = span.attributes.remove("gen_ai.completion");
            if let Some(l) = span.llm.as_mut() {
                l.prompt_sha256 = blob_value(&blobs, prompt.as_deref());
                l.completion_sha256 = blob_value(&blobs, completion.as_deref());
            }
        }
    }

    let span_count = spans.len();
    if let Err(e) = store.insert_spans(&spans) {
        tracing::error!(error = %e, "failed to insert datadog spans");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("storage error: {e}"),
        )
            .into_response();
    }
    if let Err(e) = bus.publish(&spans) {
        tracing::warn!(error = %e, "failed to publish spans to bus");
    }
    tracing::debug!(span_count, ?version, "ingested datadog spans");

    sampling_response()
}

/// The priority-sampling response body every traces endpoint returns. Rate 1.0
/// on the catch-all key keeps clients sampling (and therefore sending)
/// everything.
fn sampling_response() -> Response {
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "rate_by_service": { "service:,env:": 1.0 } })),
    )
        .into_response()
}

fn blob_value(blobs: &BlobStore, value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    match blobs.put(value.as_bytes()) {
        Ok(hash) => Some(hash),
        Err(e) => {
            tracing::warn!(error = %e, "failed to store payload blob");
            None
        }
    }
}

// ── Decoding ────────────────────────────────────────────────────────

fn decode_traces(
    version: TracesVersion,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Vec<Vec<DdSpan>>> {
    match version {
        TracesVersion::V04 => {
            let json = headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct.contains("json"));
            if json {
                serde_json::from_slice(body).context("JSON traces decode failed")
            } else {
                rmp_serde::from_slice(body).context("msgpack traces decode failed")
            }
        }
        TracesVersion::V05 => decode_v05(body),
    }
}

/// Decode the v0.5 string-table format: `[strings, traces]` where each span is
/// a 12-element array
/// `[service, name, resource, trace_id, span_id, parent_id, start, duration,
///   error, meta, metrics, type]`
/// with strings replaced by indices into the table (including meta keys/values
/// and metrics keys).
fn decode_v05(body: &[u8]) -> Result<Vec<Vec<DdSpan>>> {
    let mut cursor = body;
    let value = rmpv::decode::read_value(&mut cursor).context("msgpack v0.5 decode failed")?;
    let top = match value.as_array() {
        Some(top) if top.len() == 2 => top,
        _ => bail!("v0.5 payload must be a 2-element array [strings, traces]"),
    };

    let strings: Vec<&str> = top[0]
        .as_array()
        .context("v0.5 string table must be an array")?
        .iter()
        .map(|v| v.as_str().unwrap_or_default())
        .collect();
    let lookup = |v: &rmpv::Value| -> String {
        v.as_u64()
            .and_then(|i| strings.get(i as usize))
            .map(|s| s.to_string())
            .unwrap_or_default()
    };

    let mut chunks = Vec::new();
    for trace in top[1].as_array().context("v0.5 traces must be an array")? {
        let mut chunk = Vec::new();
        for span in trace.as_array().context("v0.5 trace must be an array")? {
            let f = span.as_array().context("v0.5 span must be an array")?;
            if f.len() < 12 {
                bail!("v0.5 span must have 12 fields, got {}", f.len());
            }
            let meta = f[9]
                .as_map()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| (lookup(k), lookup(v)))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            let metrics = f[10]
                .as_map()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((lookup(k), v.as_f64()?)))
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            chunk.push(DdSpan {
                service: lookup(&f[0]),
                name: lookup(&f[1]),
                resource: lookup(&f[2]),
                trace_id: f[3].as_u64().unwrap_or_default(),
                span_id: f[4].as_u64().unwrap_or_default(),
                parent_id: f[5].as_u64().unwrap_or_default(),
                start: f[6].as_i64().unwrap_or_default(),
                duration: f[7].as_i64().unwrap_or_default(),
                error: f[8].as_i64().unwrap_or_default() as i32,
                meta,
                metrics,
                span_type: lookup(&f[11]),
            });
        }
        chunks.push(chunk);
    }
    Ok(chunks)
}

// ── Conversion ──────────────────────────────────────────────────────

/// Convert one trace chunk. Chunk-level because the `_dd.p.tid` propagated tag
/// (the upper 64 bits of a 128-bit trace id) is only set on the chunk root —
/// resolving it per-chunk keeps every span of the trace on the same id.
fn convert_chunk(chunk: &[DdSpan]) -> Vec<Span> {
    let mut tid_upper: HashMap<u64, u64> = HashMap::new();
    for dd in chunk {
        if let Some(upper) = dd
            .meta
            .get("_dd.p.tid")
            .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
        {
            tid_upper.insert(dd.trace_id, upper);
        }
    }
    chunk
        .iter()
        .map(|dd| convert_span(dd, tid_upper.get(&dd.trace_id).copied().unwrap_or(0)))
        .collect()
}

fn convert_span(dd: &DdSpan, trace_id_upper: u64) -> Span {
    let trace_id = format!("{trace_id_upper:016x}{:016x}", dd.trace_id);
    let span_id = format!("{:016x}", dd.span_id);
    let parent_span_id = (dd.parent_id != 0).then(|| format!("{:016x}", dd.parent_id));

    let start_time = nanos_to_datetime(dd.start);
    let end_time = nanos_to_datetime(dd.start.saturating_add(dd.duration));
    let duration_ms = dd.duration as f64 / 1_000_000.0;

    let mut attributes = dd.meta.clone();
    // Numeric tags ride in `metrics`; fold them into the string attribute map
    // so filters see one namespace. Meta wins on key collisions.
    for (k, v) in &dd.metrics {
        attributes.entry(k.clone()).or_insert_with(|| v.to_string());
    }
    if !dd.resource.is_empty() {
        attributes
            .entry("resource.name".to_string())
            .or_insert_with(|| dd.resource.clone());
    }
    if !dd.span_type.is_empty() {
        attributes
            .entry("span.type".to_string())
            .or_insert_with(|| dd.span_type.clone());
    }

    let status = if dd.error != 0 {
        SpanStatus::Error
    } else {
        SpanStatus::Unset
    };

    // GenAI attributes get the same typed-LLM promotion as the OTLP path.
    let llm = super::otlp::extract_llm_span(&attributes);
    let kind = if llm.is_some() {
        SpanKind::Llm
    } else {
        match dd.meta.get("span.kind").map(String::as_str) {
            Some("server") => SpanKind::Server,
            Some("client") => SpanKind::Client,
            Some("producer") => SpanKind::Producer,
            Some("consumer") => SpanKind::Consumer,
            _ => SpanKind::Internal,
        }
    };

    let service = if dd.service.is_empty() {
        "unknown".to_string()
    } else {
        dd.service.clone()
    };

    Span {
        trace_id,
        span_id,
        parent_span_id,
        service,
        operation: dd.name.clone(),
        start_time,
        end_time,
        duration_ms,
        status,
        attributes,
        events: Vec::new(),
        kind,
        llm,
    }
}

fn nanos_to_datetime(nanos: i64) -> DateTime<Utc> {
    let secs = nanos.div_euclid(1_000_000_000);
    let nsecs = nanos.rem_euclid(1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmpv::Value;

    fn s(v: &str) -> Value {
        Value::from(v)
    }

    /// Encode a msgpack v0.4-style span map.
    fn v04_span(trace_id: u64, span_id: u64, parent_id: u64, meta: Vec<(&str, &str)>) -> Value {
        Value::Map(vec![
            (s("service"), s("billing")),
            (s("name"), s("http.request")),
            (s("resource"), s("GET /users/:id")),
            (s("trace_id"), Value::from(trace_id)),
            (s("span_id"), Value::from(span_id)),
            (s("parent_id"), Value::from(parent_id)),
            (s("start"), Value::from(1_700_000_000_000_000_000_i64)),
            (s("duration"), Value::from(250_000_000_i64)),
            (s("error"), Value::from(0)),
            (
                s("meta"),
                Value::Map(meta.into_iter().map(|(k, v)| (s(k), s(v))).collect()),
            ),
            (
                s("metrics"),
                Value::Map(vec![(s("_sampling_priority_v1"), Value::F64(1.0))]),
            ),
            (s("type"), s("web")),
        ])
    }

    fn encode(v: &Value) -> Vec<u8> {
        let mut buf = Vec::new();
        rmpv::encode::write_value(&mut buf, v).unwrap();
        buf
    }

    #[test]
    fn decodes_v04_msgpack_payload() {
        let payload = Value::Array(vec![Value::Array(vec![
            v04_span(42, 7, 0, vec![("span.kind", "server")]),
            v04_span(42, 8, 7, vec![]),
        ])]);
        let headers = HeaderMap::new();
        let chunks = decode_traces(TracesVersion::V04, &headers, &encode(&payload)).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2);
        let dd = &chunks[0][0];
        assert_eq!(dd.service, "billing");
        assert_eq!(dd.name, "http.request");
        assert_eq!(dd.trace_id, 42);
        assert_eq!(dd.meta.get("span.kind").map(String::as_str), Some("server"));
        assert_eq!(dd.metrics.get("_sampling_priority_v1"), Some(&1.0));
    }

    #[test]
    fn decodes_v04_json_payload() {
        let body = serde_json::json!([[{
            "service": "billing",
            "name": "http.request",
            "resource": "GET /",
            "trace_id": 42u64,
            "span_id": 7u64,
            "parent_id": 0u64,
            "start": 1_700_000_000_000_000_000_i64,
            "duration": 1_000_000i64,
            "error": 1,
            "meta": {"http.status_code": "500"},
            "metrics": {},
            "type": "web"
        }]])
        .to_string();
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        let chunks = decode_traces(TracesVersion::V04, &headers, body.as_bytes()).unwrap();
        assert_eq!(chunks[0][0].error, 1);
        assert_eq!(
            chunks[0][0]
                .meta
                .get("http.status_code")
                .map(String::as_str),
            Some("500")
        );
    }

    #[test]
    fn decodes_v05_string_table_payload() {
        // strings: ["", "billing", "db.query", "SELECT 1", "sql", "peer.host", "db1"]
        let strings = Value::Array(vec![
            s(""),
            s("billing"),
            s("db.query"),
            s("SELECT 1"),
            s("sql"),
            s("peer.host"),
            s("db1"),
        ]);
        let span = Value::Array(vec![
            Value::from(1u64),                                        // service
            Value::from(2u64),                                        // name
            Value::from(3u64),                                        // resource
            Value::from(42u64),                                       // trace_id
            Value::from(7u64),                                        // span_id
            Value::from(0u64),                                        // parent_id
            Value::from(1_700_000_000_000_000_000_i64),               // start
            Value::from(5_000_000_i64),                               // duration
            Value::from(0),                                           // error
            Value::Map(vec![(Value::from(5u64), Value::from(6u64))]), // meta
            Value::Map(vec![]),                                       // metrics
            Value::from(4u64),                                        // type
        ]);
        let payload = Value::Array(vec![strings, Value::Array(vec![Value::Array(vec![span])])]);
        let chunks = decode_v05(&encode(&payload)).unwrap();
        let dd = &chunks[0][0];
        assert_eq!(dd.service, "billing");
        assert_eq!(dd.name, "db.query");
        assert_eq!(dd.resource, "SELECT 1");
        assert_eq!(dd.span_type, "sql");
        assert_eq!(dd.meta.get("peer.host").map(String::as_str), Some("db1"));
    }

    #[test]
    fn converts_span_fields() {
        let dd = DdSpan {
            service: "billing".into(),
            name: "http.request".into(),
            resource: "GET /users/:id".into(),
            trace_id: 42,
            span_id: 7,
            parent_id: 3,
            start: 1_700_000_000_000_000_000,
            duration: 250_000_000,
            error: 1,
            meta: [("span.kind".to_string(), "server".to_string())]
                .into_iter()
                .collect(),
            metrics: [("retries".to_string(), 2.0)].into_iter().collect(),
            span_type: "web".into(),
        };
        let spans = convert_chunk(&[dd]);
        let span = &spans[0];
        assert_eq!(span.trace_id, format!("{:016x}{:016x}", 0, 42));
        assert_eq!(span.span_id, format!("{:016x}", 7));
        assert_eq!(span.parent_span_id.as_deref(), Some("0000000000000003"));
        assert_eq!(span.service, "billing");
        assert_eq!(span.operation, "http.request");
        assert_eq!(span.duration_ms, 250.0);
        assert_eq!(span.status, SpanStatus::Error);
        assert_eq!(span.kind, SpanKind::Server);
        assert_eq!(
            span.attributes.get("resource.name").map(String::as_str),
            Some("GET /users/:id")
        );
        assert_eq!(
            span.attributes.get("span.type").map(String::as_str),
            Some("web")
        );
        assert_eq!(
            span.attributes.get("retries").map(String::as_str),
            Some("2")
        );
        assert_eq!((span.end_time - span.start_time).num_milliseconds(), 250);
    }

    #[test]
    fn widens_trace_id_from_chunk_root_tid_tag() {
        let root = DdSpan {
            trace_id: 42,
            span_id: 1,
            meta: [("_dd.p.tid".to_string(), "640cfd8d00000000".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        // Child span has no _dd.p.tid but must land on the same 128-bit id.
        let child = DdSpan {
            trace_id: 42,
            span_id: 2,
            parent_id: 1,
            ..Default::default()
        };
        let spans = convert_chunk(&[root, child]);
        assert_eq!(spans[0].trace_id, "640cfd8d00000000000000000000002a");
        assert_eq!(spans[1].trace_id, spans[0].trace_id);
    }

    #[test]
    fn promotes_genai_meta_to_llm_span() {
        let dd = DdSpan {
            service: "agent".into(),
            name: "llm.call".into(),
            trace_id: 1,
            span_id: 1,
            meta: [
                ("gen_ai.system".to_string(), "anthropic".to_string()),
                (
                    "gen_ai.request.model".to_string(),
                    "claude-sonnet-5".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let spans = convert_chunk(&[dd]);
        assert_eq!(spans[0].kind, SpanKind::Llm);
        let llm = spans[0].llm.as_ref().expect("LLM extension");
        assert_eq!(llm.provider, "anthropic");
        assert_eq!(llm.model, "claude-sonnet-5");
    }

    #[test]
    fn empty_service_falls_back_to_unknown() {
        let spans = convert_chunk(&[DdSpan {
            trace_id: 1,
            span_id: 1,
            ..Default::default()
        }]);
        assert_eq!(spans[0].service, "unknown");
    }
}
