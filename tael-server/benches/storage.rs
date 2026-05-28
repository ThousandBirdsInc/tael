//! Storage ingestion + query benchmarks for the DuckDB backend, driven through
//! the public `Store` trait (insert_spans / query_traces / get_trace).
//!
//! Gated behind the `bench-storage` cargo feature:
//!
//!     cargo bench --features bench-storage --bench storage
//!
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use tael_server::{DuckDbStore, Store, TraceQuery};

mod common;
use common::{SPANS_PER_TRACE, make_spans};

fn new_store(dir: &std::path::Path) -> DuckDbStore {
    DuckDbStore::new(dir.to_str().unwrap()).expect("create DuckDbStore")
}

fn bench_insert_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("duckdb_insert_spans");
    for &n in &[100usize, 1_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = new_store(dir.path());
                    (dir, store, make_spans(n))
                },
                |(_dir, store, spans)| {
                    let store: &dyn Store = &store;
                    store.insert_spans(black_box(&spans)).unwrap();
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_query_traces(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = new_store(dir.path());
    store.insert_spans(&make_spans(10_000)).unwrap();

    let query = TraceQuery {
        service: Some("service-0".to_string()),
        limit: Some(100),
        ..Default::default()
    };
    let store: &dyn Store = &store;

    c.bench_function("duckdb_query_traces/10k_spans", |b| {
        b.iter(|| store.query_traces(black_box(&query)).unwrap());
    });
}

fn bench_get_trace(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = new_store(dir.path());
    let total = 10_000;
    store.insert_spans(&make_spans(total)).unwrap();

    // A trace id that exists in the middle of the inserted data.
    let trace_id = format!("{:032x}", (total / SPANS_PER_TRACE) / 2);
    let store: &dyn Store = &store;

    c.bench_function("duckdb_get_trace/10k_spans", |b| {
        b.iter(|| store.get_trace(black_box(&trace_id)).unwrap());
    });
}

criterion_group!(
    benches,
    bench_insert_spans,
    bench_query_traces,
    bench_get_trace
);
criterion_main!(benches);
