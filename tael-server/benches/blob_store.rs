//! Content-addressed blob store benchmarks.
//!
//! Every LLM prompt/completion payload flows through `BlobStore` during ingest
//! (`put` → SHA-256 + snappy compress + atomic write, with dedup) and during
//! trace inspection (`get` → decompress). `gc` is the periodic mark-and-sweep.

use std::collections::HashSet;

use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use tael_server::BlobStore;

mod common;
use common::make_payload;

/// Each iteration stores never-before-seen content, exercising the full
/// hash + compress + write path.
fn bench_put_unique(c: &mut Criterion) {
    let mut group = c.benchmark_group("blob_put_unique");
    for &kb in &[1usize, 16] {
        group.throughput(Throughput::Bytes((kb * 1024) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(kb), &kb, |b, &kb| {
            let dir = tempfile::tempdir().unwrap();
            let store = BlobStore::new(dir.path().to_str().unwrap()).unwrap();
            let mut seed = 0usize;
            b.iter(|| {
                seed += 1;
                let payload = make_payload(seed, kb);
                store.put(black_box(&payload)).unwrap()
            });
        });
    }
    group.finish();
}

/// Same content every iteration: after the first write this is the dedup fast
/// path (hash + `path.exists()` check, no compression or write).
fn bench_put_dedup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().to_str().unwrap()).unwrap();
    let payload = make_payload(42, 4);
    store.put(&payload).unwrap();

    c.bench_function("blob_put_dedup/4kb", |b| {
        b.iter(|| store.put(black_box(&payload)).unwrap());
    });
}

fn bench_get(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().to_str().unwrap()).unwrap();
    let payload = make_payload(7, 4);
    let hash = store.put(&payload).unwrap();

    c.bench_function("blob_get/4kb", |b| {
        b.iter(|| store.get(black_box(&hash)).unwrap());
    });
}

/// Mark-and-sweep GC over a store with `n` blobs, half of them live.
fn bench_gc(c: &mut Criterion) {
    let mut group = c.benchmark_group("blob_gc");
    for &n in &[100usize, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let dir = tempfile::tempdir().unwrap();
                    let store = BlobStore::new(dir.path().to_str().unwrap()).unwrap();
                    let mut live = HashSet::new();
                    for i in 0..n {
                        let hash = store.put(&make_payload(i, 1)).unwrap();
                        if i % 2 == 0 {
                            live.insert(hash);
                        }
                    }
                    (dir, store, live)
                },
                |(_dir, store, live)| store.gc(black_box(&live)).unwrap(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_put_unique,
    bench_put_dedup,
    bench_get,
    bench_gc
);
criterion_main!(benches);
