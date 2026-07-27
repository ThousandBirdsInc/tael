//! Serialization benchmarks for the core telemetry models.
//!
//! Every span/log/metric is encoded on the ingest write path and decoded on
//! every scan-based read path, so this is a pure-CPU floor under both,
//! independent of any storage backend.
//!
//! Two codecs are measured. MessagePack is what the storage engine writes:
//! `storage::backend::codec` encodes with `rmp_serde::to_vec_named`, and these
//! cases mirror that call exactly. JSON is kept alongside it because it is
//! still the API's wire format, and because the gap between the two is the
//! reason storage stopped using it.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use tael_server::Span;

mod common;
use common::{make_logs, make_metrics, make_spans};

fn bench_serialize_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("serialize_spans");
    for &n in &[1usize, 100, 1_000] {
        let spans = make_spans(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("json", n), &spans, |b, spans| {
            b.iter(|| serde_json::to_vec(black_box(spans)).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("msgpack", n), &spans, |b, spans| {
            b.iter(|| rmp_serde::to_vec_named(black_box(spans)).unwrap());
        });
    }
    group.finish();
}

fn bench_deserialize_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("deserialize_spans");
    for &n in &[1usize, 100, 1_000] {
        let spans = make_spans(n);
        let json = serde_json::to_vec(&spans).unwrap();
        let msgpack = rmp_serde::to_vec_named(&spans).unwrap();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::new("json", n), &json, |b, bytes| {
            b.iter(|| serde_json::from_slice::<Vec<Span>>(black_box(bytes)).unwrap());
        });
        group.bench_with_input(BenchmarkId::new("msgpack", n), &msgpack, |b, bytes| {
            b.iter(|| rmp_serde::from_slice::<Vec<Span>>(black_box(bytes)).unwrap());
        });
    }
    group.finish();
}

fn bench_serialize_logs_metrics(c: &mut Criterion) {
    let logs = make_logs(1_000);
    let metrics = make_metrics(1_000);

    let mut group = c.benchmark_group("serialize_signals");
    group.throughput(Throughput::Elements(1_000));
    group.bench_function("json/logs/1000", |b| {
        b.iter(|| serde_json::to_vec(black_box(&logs)).unwrap());
    });
    group.bench_function("msgpack/logs/1000", |b| {
        b.iter(|| rmp_serde::to_vec_named(black_box(&logs)).unwrap());
    });
    group.bench_function("json/metrics/1000", |b| {
        b.iter(|| serde_json::to_vec(black_box(&metrics)).unwrap());
    });
    group.bench_function("msgpack/metrics/1000", |b| {
        b.iter(|| rmp_serde::to_vec_named(black_box(&metrics)).unwrap());
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
