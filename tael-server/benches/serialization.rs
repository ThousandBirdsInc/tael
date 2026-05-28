//! Serialization benchmarks for the core telemetry models.
//!
//! Every span/log/metric is JSON-serialized on the ingest write path (and
//! deserialized on the read path), so these are pure-CPU hot paths that gate
//! ingest and query throughput independent of any storage backend.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tael_server::Span;

mod common;
use common::{make_logs, make_metrics, make_spans};

fn bench_serialize_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialize_spans");
    for &n in &[1usize, 100, 1_000] {
        let spans = make_spans(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &spans, |b, spans| {
            b.iter(|| serde_json::to_vec(black_box(spans)).unwrap());
        });
    }
    group.finish();
}

fn bench_deserialize_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("deserialize_spans");
    for &n in &[1usize, 100, 1_000] {
        let bytes = serde_json::to_vec(&make_spans(n)).unwrap();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &bytes, |b, bytes| {
            b.iter(|| serde_json::from_slice::<Vec<Span>>(black_box(bytes)).unwrap());
        });
    }
    group.finish();
}

fn bench_serialize_logs_metrics(c: &mut Criterion) {
    let logs = make_logs(1_000);
    let metrics = make_metrics(1_000);

    let mut group = c.benchmark_group("serialize_signals");
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("logs/1000", |b| {
        b.iter(|| serde_json::to_vec(black_box(&logs)).unwrap());
    });
    group.bench_function("metrics/1000", |b| {
        b.iter(|| serde_json::to_vec(black_box(&metrics)).unwrap());
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_serialize_spans,
    bench_deserialize_spans,
    bench_serialize_logs_metrics
);
criterion_main!(benches);
