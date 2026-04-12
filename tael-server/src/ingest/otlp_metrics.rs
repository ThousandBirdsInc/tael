use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
    metrics_service_server::MetricsService,
};
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyVal;
use opentelemetry_proto::tonic::metrics::v1::{
    metric::Data as MetricData, number_data_point::Value as NumberValue,
};
use tonic::{Request, Response, Status};

use crate::storage::DuckDbStore;
use crate::storage::models::{MetricPoint, MetricType};

pub struct OtlpMetricsService {
    store: Arc<DuckDbStore>,
}

impl OtlpMetricsService {
    pub fn new(store: Arc<DuckDbStore>) -> Self {
        Self { store }
    }
}

#[tonic::async_trait]
impl MetricsService for OtlpMetricsService {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        let req = request.into_inner();
        let mut points: Vec<MetricPoint> = Vec::new();

        for resource_metrics in &req.resource_metrics {
            let service_name = resource_metrics
                .resource
                .as_ref()
                .and_then(|r| {
                    r.attributes.iter().find_map(|attr| {
                        if attr.key == "service.name" {
                            attr.value.as_ref().and_then(|v| v.value.as_ref()).and_then(
                                |val| match val {
                                    AnyVal::StringValue(s) => Some(s.clone()),
                                    _ => None,
                                },
                            )
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
                                if let Some(p) = number_point(
                                    dp,
                                    &service_name,
                                    &name,
                                    &unit,
                                    MetricType::Gauge,
                                ) {
                                    points.push(p);
                                }
                            }
                        }
                        MetricData::Sum(s) => {
                            for dp in &s.data_points {
                                if let Some(p) = number_point(
                                    dp,
                                    &service_name,
                                    &name,
                                    &unit,
                                    MetricType::Sum,
                                ) {
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
                                });
                            }
                        }
                    }
                }
            }
        }

        let count = points.len();
        if let Err(e) = self.store.insert_metrics(&points) {
            tracing::error!(error = %e, "failed to insert metrics");
            return Err(Status::internal(format!("storage error: {e}")));
        }

        tracing::debug!(metric_points = count, "ingested metrics");

        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success: None,
        }))
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
    })
}

fn kv_to_map(
    kvs: &[opentelemetry_proto::tonic::common::v1::KeyValue],
) -> HashMap<String, String> {
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
