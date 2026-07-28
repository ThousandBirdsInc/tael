use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
    metrics_service_server::MetricsService,
};
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyVal;
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, metric::Data as MetricData, number_data_point::Value as NumberValue,
};
use tonic::{Request, Response, Status};

use crate::storage::Store;
use crate::storage::models::{HistogramBuckets, MetricPoint, MetricType, Temporality};

pub struct OtlpMetricsService {
    store: Arc<dyn Store>,
    /// Stamp every record with the writing principal's tenant
    /// (`TAEL_MULTI_TENANT`). See [`crate::tenancy::stamp`].
    multi_tenant: bool,
}

impl OtlpMetricsService {
    pub fn new(store: Arc<dyn Store>, multi_tenant: bool) -> Self {
        Self {
            store,
            multi_tenant,
        }
    }
}

/// Shared-handle wrapper so the gRPC and OTLP/HTTP listeners serve the same
/// metrics service. See [`super::otlp::SharedTraceService`].
pub struct SharedMetricsService(pub Arc<dyn MetricsService>);

#[tonic::async_trait]
impl MetricsService for SharedMetricsService {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        self.0.export(request).await
    }
}

#[tonic::async_trait]
impl MetricsService for OtlpMetricsService {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        let Some(_permit) = super::backpressure::try_acquire() else {
            super::stats::record_shed(super::stats::Pipeline::OtlpMetrics);
            return Err(Status::resource_exhausted(
                "ingest at capacity; retry with backoff",
            ));
        };
        let principal = request
            .extensions()
            .get::<crate::auth::Principal>()
            .cloned();
        let req = request.into_inner();
        let mut points: Vec<MetricPoint> = Vec::new();

        for resource_metrics in &req.resource_metrics {
            let service_name = resource_metrics
                .resource
                .as_ref()
                .and_then(|r| {
                    r.attributes.iter().find_map(|attr| {
                        if attr.key == "service.name" {
                            attr.value
                                .as_ref()
                                .and_then(|v| v.value.as_ref())
                                .and_then(|val| match val {
                                    AnyVal::StringValue(s) => Some(s.clone()),
                                    _ => None,
                                })
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| "unknown".to_string());

            for scope_metrics in &resource_metrics.scope_metrics {
                for metric in &scope_metrics.metrics {
                    let name = metric.name.clone();
                    let unit = metric.unit.clone();
                    let Some(data) = metric.data.as_ref() else {
                        continue;
                    };

                    match data {
                        MetricData::Gauge(g) => {
                            for dp in &g.data_points {
                                if let Some(p) =
                                    number_point(dp, &service_name, &name, &unit, MetricType::Gauge)
                                {
                                    points.push(p);
                                }
                            }
                        }
                        MetricData::Sum(s) => {
                            for dp in &s.data_points {
                                if let Some(p) =
                                    number_point(dp, &service_name, &name, &unit, MetricType::Sum)
                                {
                                    points.push(p);
                                }
                            }
                        }
                        MetricData::Histogram(h) => {
                            for dp in &h.data_points {
                                points.push(MetricPoint {
                                    timestamp: nanos_to_datetime(dp.time_unix_nano),
                                    service: service_name.clone(),
                                    name: name.clone(),
                                    metric_type: MetricType::Histogram,
                                    value: dp.sum.unwrap_or(0.0),
                                    unit: unit.clone(),
                                    attributes: kv_to_map(&dp.attributes),
                                    histogram: explicit_buckets(
                                        dp,
                                        temporality_of(h.aggregation_temporality),
                                    ),
                                });
                            }
                        }
                        MetricData::Summary(s) => {
                            for dp in &s.data_points {
                                points.push(MetricPoint {
                                    timestamp: nanos_to_datetime(dp.time_unix_nano),
                                    service: service_name.clone(),
                                    name: name.clone(),
                                    metric_type: MetricType::Summary,
                                    value: dp.sum,
                                    unit: unit.clone(),
                                    attributes: kv_to_map(&dp.attributes),
                                    // Summaries carry precomputed quantiles,
                                    // not buckets.
                                    histogram: None,
                                });
                            }
                        }
                        MetricData::ExponentialHistogram(h) => {
                            for dp in &h.data_points {
                                points.push(MetricPoint {
                                    timestamp: nanos_to_datetime(dp.time_unix_nano),
                                    service: service_name.clone(),
                                    name: name.clone(),
                                    metric_type: MetricType::Histogram,
                                    value: dp.sum.unwrap_or(0.0),
                                    unit: unit.clone(),
                                    attributes: kv_to_map(&dp.attributes),
                                    histogram: exponential_buckets(
                                        dp,
                                        temporality_of(h.aggregation_temporality),
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        super::cardinality::admit(&mut points);
        let count = points.len();
        // Stamp the writer's tenant last, so it overrides anything the client
        // sent — the attribute is an authorization boundary, not client data.
        crate::tenancy::stamp(
            self.multi_tenant,
            principal.as_ref(),
            points.iter_mut().map(|p| &mut p.attributes),
        );

        if let Err(e) = self.store.insert_metrics(&points) {
            tracing::error!(error = %e, "failed to insert metrics");
            super::stats::record_error(super::stats::Pipeline::OtlpMetrics);
            return Err(Status::internal(format!("storage error: {e}")));
        }
        super::stats::record_accepted(super::stats::Pipeline::OtlpMetrics, count);

        tracing::debug!(metric_points = count, "ingested metrics");

        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: None,
        }))
    }
}

/// Capture an explicit-bounds histogram data point's buckets.
///
/// OTLP sends `explicit_bounds` (N ascending upper bounds) and `bucket_counts`
/// (N+1 per-bucket counts, the last one open-ended). A producer that omits the
/// counts, or sends a mismatched pair, yields `None` — a wrong bucket layout
/// would silently produce wrong quantiles, which is worse than admitting the
/// point has no usable distribution.
fn explicit_buckets(
    dp: &opentelemetry_proto::tonic::metrics::v1::HistogramDataPoint,
    temporality: Temporality,
) -> Option<HistogramBuckets> {
    if dp.bucket_counts.is_empty() || dp.bucket_counts.len() != dp.explicit_bounds.len() + 1 {
        return None;
    }
    Some(HistogramBuckets {
        bounds: dp.explicit_bounds.clone(),
        counts: dp.bucket_counts.clone(),
        count: dp.count,
        sum: dp.sum.unwrap_or(0.0),
        min: dp.min,
        max: dp.max,
        temporality,
    })
}

/// Convert an exponential histogram to explicit bounds.
///
/// Exponential histograms encode bucket `i` as covering `(base^i, base^(i+1)]`
/// where `base = 2^(2^-scale)`. Materializing those bounds at ingest means the
/// query layer has exactly one histogram shape to reason about, at the cost of
/// storing the bounds — a few hundred bytes per point, against a whole class of
/// unanswerable quantile questions.
///
/// Only the positive range and the zero bucket are represented. Negative
/// observations are rare for the measurements histograms are used for (latency,
/// size) and folding them into ascending explicit bounds alongside positives
/// would misreport the distribution, so a point carrying them is left without
/// buckets rather than described incorrectly.
fn exponential_buckets(
    dp: &opentelemetry_proto::tonic::metrics::v1::ExponentialHistogramDataPoint,
    temporality: Temporality,
) -> Option<HistogramBuckets> {
    let positive = dp.positive.as_ref()?;
    if positive.bucket_counts.is_empty() {
        return None;
    }
    if dp
        .negative
        .as_ref()
        .is_some_and(|n| n.bucket_counts.iter().any(|c| *c > 0))
    {
        return None;
    }

    let base = 2f64.powf(2f64.powi(-dp.scale));
    let mut bounds = Vec::with_capacity(positive.bucket_counts.len() + 1);
    let mut counts = Vec::with_capacity(positive.bucket_counts.len() + 2);

    // The zero bucket holds observations at (or very near) zero, so its upper
    // bound is 0 and it leads the ascending sequence.
    bounds.push(0.0);
    counts.push(dp.zero_count);

    for (i, count) in positive.bucket_counts.iter().enumerate() {
        let index = positive.offset as i64 + i as i64 + 1;
        let upper = base.powf(index as f64);
        if !upper.is_finite() {
            return None;
        }
        bounds.push(upper);
        counts.push(*count);
    }
    // Trailing open-ended bucket: nothing lands above the top bound because the
    // encoding's own range ends there.
    counts.push(0);

    Some(HistogramBuckets {
        bounds,
        counts,
        count: dp.count,
        sum: dp.sum.unwrap_or(0.0),
        min: dp.min,
        max: dp.max,
        temporality,
    })
}

/// Map OTLP's aggregation temporality enum onto the stored form. OTLP's
/// `UNSPECIFIED` is treated as cumulative, matching the SDK default.
fn temporality_of(raw: i32) -> Temporality {
    match AggregationTemporality::try_from(raw) {
        Ok(AggregationTemporality::Delta) => Temporality::Delta,
        _ => Temporality::Cumulative,
    }
}

fn number_point(
    dp: &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint,
    service: &str,
    name: &str,
    unit: &str,
    metric_type: MetricType,
) -> Option<MetricPoint> {
    let value = match dp.value.as_ref()? {
        NumberValue::AsDouble(d) => *d,
        NumberValue::AsInt(i) => *i as f64,
    };
    Some(MetricPoint {
        timestamp: nanos_to_datetime(dp.time_unix_nano),
        service: service.to_string(),
        name: name.to_string(),
        metric_type,
        value,
        unit: unit.to_string(),
        attributes: kv_to_map(&dp.attributes),
        histogram: None,
    })
}

fn kv_to_map(kvs: &[opentelemetry_proto::tonic::common::v1::KeyValue]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for attr in kvs {
        if let Some(val) = attr.value.as_ref().and_then(|v| v.value.as_ref()) {
            let s = match val {
                AnyVal::StringValue(s) => s.clone(),
                AnyVal::IntValue(i) => i.to_string(),
                AnyVal::DoubleValue(d) => d.to_string(),
                AnyVal::BoolValue(b) => b.to_string(),
                _ => continue,
            };
            map.insert(attr.key.clone(), s);
        }
    }
    map
}

fn nanos_to_datetime(nanos: u64) -> DateTime<Utc> {
    let secs = (nanos / 1_000_000_000) as i64;
    let nsecs = (nanos % 1_000_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs).unwrap_or_default()
}
