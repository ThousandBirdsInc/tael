use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{
    ResourceSpans, ScopeSpans, Span, Status,
    span::Event,
    status::StatusCode,
};
use rand::Rng;

pub fn trace_id() -> Vec<u8> {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill(&mut buf);
    buf.to_vec()
}

pub fn span_id() -> Vec<u8> {
    let mut buf = [0u8; 8];
    rand::thread_rng().fill(&mut buf);
    buf.to_vec()
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

pub fn str_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

pub fn int_attr(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::IntValue(value)),
        }),
    }
}

pub struct SpanSpec {
    pub name: String,
    pub trace_id: Vec<u8>,
    pub span_id: Vec<u8>,
    pub parent_span_id: Vec<u8>,
    pub duration_ms: u64,
    pub offset_ms: u64,
    pub status: StatusCode,
    pub attributes: Vec<KeyValue>,
    pub events: Vec<Event>,
}

impl SpanSpec {
    pub fn new(name: &str, trace_id: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            trace_id: trace_id.to_vec(),
            span_id: span_id(),
            parent_span_id: vec![],
            duration_ms: 10,
            offset_ms: 0,
            status: StatusCode::Ok,
            attributes: vec![],
            events: vec![],
        }
    }

    pub fn parent(mut self, parent: &[u8]) -> Self {
        self.parent_span_id = parent.to_vec();
        self
    }

    pub fn duration(mut self, ms: u64) -> Self {
        self.duration_ms = ms;
        self
    }

    pub fn offset(mut self, ms: u64) -> Self {
        self.offset_ms = ms;
        self
    }

    pub fn error(mut self) -> Self {
        self.status = StatusCode::Error;
        self
    }

    pub fn attr(mut self, kv: KeyValue) -> Self {
        self.attributes.push(kv);
        self
    }

    pub fn event(mut self, name: &str, attrs: Vec<KeyValue>) -> Self {
        self.events.push(Event {
            name: name.to_string(),
            time_unix_nano: now_nanos(),
            attributes: attrs,
            dropped_attributes_count: 0,
        });
        self
    }

    pub fn build(self) -> Span {
        let base = now_nanos() - 5_000_000_000; // 5s ago base
        let start = base + self.offset_ms * 1_000_000;
        let end = start + self.duration_ms * 1_000_000;

        Span {
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: self.parent_span_id,
            name: self.name,
            kind: 1, // INTERNAL
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            attributes: self.attributes,
            events: self.events,
            status: Some(Status {
                code: self.status.into(),
                message: String::new(),
            }),
            trace_state: String::new(),
            dropped_attributes_count: 0,
            dropped_events_count: 0,
            dropped_links_count: 0,
            links: vec![],
            flags: 0,
        }
    }
}

pub fn resource_spans(service: &str, spans: Vec<Span>) -> ResourceSpans {
    ResourceSpans {
        resource: Some(Resource {
            attributes: vec![str_attr("service.name", service)],
            dropped_attributes_count: 0,
        }),
        scope_spans: vec![ScopeSpans {
            scope: None,
            spans,
            schema_url: String::new(),
        }],
        schema_url: String::new(),
    }
}
