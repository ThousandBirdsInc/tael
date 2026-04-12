//! Prometheus remote-write receiver.
//!
//! Accepts `POST /api/v1/write` with a Snappy-compressed protobuf body
//! following the Prometheus remote-write v1 spec:
//!     https://prometheus.io/docs/concepts/remote_write_spec/
//!
//! The minimal protobuf schema is defined inline to avoid a prost-build
//! step — we only need four message types.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{body::Bytes, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use prost::Message;

use crate::storage::DuckDbStore;
use crate::storage::models::{MetricPoint, MetricType};

// ── Minimal Prometheus remote-write proto ───────────────────────────

#[derive(Clone, PartialEq, Message)]
pub struct WriteRequest {
    #[prost(message, repeated, tag = "1")]
    pub timeseries: Vec<TimeSeries>,
}

#[derive(Clone, PartialEq, Message)]
pub struct TimeSeries {
    #[prost(message, repeated, tag = "1")]
    pub labels: Vec<Label>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Label {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Sample {
    #[prost(double, tag = "1")]
    pub value: f64,
    /// Milliseconds since Unix epoch.
    #[prost(int64, tag = "2")]
    pub timestamp: i64,
}

// ── Handler ─────────────────────────────────────────────────────────

pub async fn handle_write(store: Arc<DuckDbStore>, body: Bytes) -> impl IntoResponse {
    match decode_and_insert(&store, &body) {
        Ok(count) => {
            tracing::debug!(metric_points = count, "ingested prom remote-write");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "prom remote-write failed");
            (
                StatusCode::BAD_REQUEST,
                format!("remote-write error: {e}"),
            )
                .into_response()
        }
    }
}

fn decode_and_insert(store: &DuckDbStore, body: &[u8]) -> Result<usize> {
    let mut decoder = snap::raw::Decoder::new();
    let decompressed = decoder
        .decompress_vec(body)
        .context("snappy decompress failed")?;

    let req = WriteRequest::decode(decompressed.as_slice())
        .context("protobuf decode failed")?;

    let mut points: Vec<MetricPoint> = Vec::new();

    for ts in req.timeseries {
        let mut name = String::new();
        let mut service = String::from("unknown");
        let mut attributes: HashMap<String, String> = HashMap::new();

        for label in ts.labels {
            match label.name.as_str() {
                "__name__" => name = label.value,
                // Prefer OTel-style service.name, fall back to Prom's job.
                "service.name" | "service_name" => service = label.value,
                "job" if service == "unknown" => service = label.value,
                _ => {
                    attributes.insert(label.name, label.value);
                }
            }
        }

        if name.is_empty() {
            continue;
        }

        for sample in ts.samples {
            // Skip NaN — Prometheus uses NaN as "stale" markers.
            if sample.value.is_nan() {
                continue;
            }
            points.push(MetricPoint {
                timestamp: millis_to_datetime(sample.timestamp),
                service: service.clone(),
                name: name.clone(),
                // Remote-write v1 has no type info; treat as Unknown.
                metric_type: MetricType::Unknown,
                value: sample.value,
                unit: String::new(),
                attributes: attributes.clone(),
            });
        }
    }

    let count = points.len();
    store.insert_metrics(&points)?;
    Ok(count)
}

fn millis_to_datetime(millis: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(millis).unwrap_or_default()
}
