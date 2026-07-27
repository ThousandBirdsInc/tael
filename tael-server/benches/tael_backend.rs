//! Storage benchmarks for `TaelBackend` — the default storage engine.
//!
//! These cover the paths a running server spends its time in: OTLP ingest
//! (`insert_spans`/`insert_logs`/`insert_metrics`, each of which is a WAL
//! append + fsync followed by an LSM write), hot-tier reads (`query_traces`,
//! `get_trace`), the aggregation paths behind `tael services` / `tael summary`,
//! and the hot→cold Parquet compaction the maintenance task runs.
//!
//! Runs on the default feature set, because tael-backend is the default engine:
//!
//!     cargo bench -p tael-server --bench tael_backend

use chrono::{Duration, Utc};
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use tael_server::{Span, Store, TaelBackend, TraceQuery};

mod common;
use common::{SPANS_PER_TRACE, make_logs, make_metrics, make_span, make_spans};

/// Number of spans in the pre-populated fixture the read benchmarks query.
const FIXTURE_SPANS: usize = 10_000;

/// A backend plus everything that has to be torn down with it.
///
/// walrus resolves its WAL directory against a process-global namespace key, so
/// two instances sharing a key fight over the same files — and that directory
/// lives outside the temp data dir, so it needs its own cleanup. Every instance
/// therefore gets a unique key which `Drop` removes.
struct BenchBackend {
    /// Declared first so the engine releases its files before `_dir` is removed.
    backend: TaelBackend,
    _dir: tempfile::TempDir,
    wal_key: String,
}

impl BenchBackend {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let wal_key = format!("tael-bench-{}", uuid::Uuid::new_v4());
        let backend = TaelBackend::with_wal_key(dir.path().to_str().expect("utf-8 path"), &wal_key)
            .expect("open tael-backend");
        Self {
            backend,
            _dir: dir,
            wal_key,
        }
    }
}

impl Drop for BenchBackend {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(format!("wal_files/{}", self.wal_key));
    }
}

/// A batch of `n` spans whose ids start at `offset`.
///
/// Ingest iterations must write distinct keys: re-inserting one fixed batch
/// would overwrite the same LSM keys and measure an update, not an append.
fn batch_at(offset: usize, n: usize) -> Vec<Span> {
    (offset..offset + n).map(make_span).collect()
}

/// `make_spans` rebased onto a wall-clock window ending now.
///
/// `query_summary` and `last_seconds` filters are evaluated against the current
/// time, so the fixture's fixed 2023 base epoch would make every windowed query
/// match zero rows and measure nothing.
fn recent_spans(n: usize) -> Vec<Span> {
    let now = Utc::now();
    (0..n)
        .map(|i| {
            let mut span = make_span(i);
            // 100ms apart: 10k spans span ~17 minutes, inside a 1h query window.
            let start = now - Duration::milliseconds(100 * (n - i) as i64);
            span.start_time = start;
            span.end_time = start + Duration::milliseconds(5);
            span
        })
        .collect()
}

/// A backend holding [`FIXTURE_SPANS`] recent spans, all in the hot tier.
fn populated_backend() -> BenchBackend {
    let bench = BenchBackend::new();
    // One insert per 1k so the WAL append stays a realistic size.
    for chunk in recent_spans(FIXTURE_SPANS).chunks(1_000) {
        bench.backend.insert_spans(chunk).unwrap();
    }
    bench
}

/// Steady-state ingest into a backend that already holds data, which is what a
/// running server does. The backend is built once per batch size and the span
/// batch is generated in `iter_batched`'s untimed setup, so the measurement is
/// the WAL append + LSM write alone.
fn bench_insert_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("tael_backend_insert_spans");
    for &n in &[1usize, 100, 1_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let bench = BenchBackend::new();
            let mut offset = 0usize;
            b.iter_batched(
                || {
                    offset += n;
                    batch_at(offset, n)
                },
                |spans| bench.backend.insert_spans(black_box(&spans)).unwrap(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_insert_signals(c: &mut Criterion) {
    let mut group = c.benchmark_group("tael_backend_insert_signals");
    group.throughput(Throughput::Elements(1_000));

    group.bench_function("logs/1000", |b| {
        let bench = BenchBackend::new();
        let mut offset = 0usize;
        b.iter_batched(
            || {
                offset += 1_000;
                make_logs(1_000)
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut l)| {
                        l.span_id = Some(format!("{:016x}", offset + i));
                        l
                    })
                    .collect::<Vec<_>>()
            },
            |logs| bench.backend.insert_logs(black_box(&logs)).unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.bench_function("metrics/1000", |b| {
        let bench = BenchBackend::new();
        let mut offset = 0usize;
        b.iter_batched(
            || {
                offset += 1_000;
                make_metrics(1_000)
                    .into_iter()
                    .enumerate()
                    .map(|(i, mut m)| {
                        // Metric keys are (service, name, timestamp); shifting
                        // the timestamp keeps each iteration a fresh append.
                        m.timestamp += Duration::seconds((offset + i) as i64);
                        m
                    })
                    .collect::<Vec<_>>()
            },
            |metrics| bench.backend.insert_metrics(black_box(&metrics)).unwrap(),
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Filtered trace search over a 10k-span hot tier — the `tael query` path.
fn bench_query_traces(c: &mut Criterion) {
    let bench = populated_backend();
    let cases: [(&str, TraceQuery); 3] = [
        (
            "service",
            TraceQuery {
                service: Some("service-0".to_string()),
                limit: Some(100),
                ..Default::default()
            },
        ),
        (
            "service_and_window",
            TraceQuery {
                service: Some("service-0".to_string()),
                last_seconds: Some(3_600),
                limit: Some(100),
                ..Default::default()
            },
        ),
        (
            "error_status",
            TraceQuery {
                status: Some("error".to_string()),
                limit: Some(100),
                ..Default::default()
            },
        ),
    ];

    let mut group = c.benchmark_group("tael_backend_query_traces");
    for (name, query) in &cases {
        group.bench_with_input(BenchmarkId::from_parameter(name), query, |b, query| {
            b.iter(|| bench.backend.query_traces(black_box(query)).unwrap());
        });
    }
    group.finish();
}

fn bench_get_trace(c: &mut Criterion) {
    let bench = populated_backend();
    // A trace id from the middle of the fixture.
    let trace_id = format!("{:032x}", (FIXTURE_SPANS / SPANS_PER_TRACE) / 2);

    c.bench_function("tael_backend_get_trace/10k_spans", |b| {
        b.iter(|| bench.backend.get_trace(black_box(&trace_id)).unwrap());
    });
}

/// The aggregation paths: `list_services` (per-service rollup) and
/// `query_summary` (the cross-signal report behind `tael summary`).
fn bench_aggregations(c: &mut Criterion) {
    let bench = populated_backend();

    let mut group = c.benchmark_group("tael_backend_aggregate");
    group.bench_function("list_services/10k_spans", |b| {
        b.iter(|| bench.backend.list_services().unwrap());
    });
    group.bench_function("query_summary/10k_spans", |b| {
        b.iter(|| bench.backend.query_summary(black_box(3_600), None).unwrap());
    });
    group.bench_function("query_summary_scoped/10k_spans", |b| {
        b.iter(|| {
            bench
                .backend
                .query_summary(black_box(3_600), Some("service-0"))
                .unwrap()
        });
    });
    group.finish();
}

/// Hot→cold compaction: evict every span from the LSM tier and write it to
/// Parquet. Each iteration needs a freshly populated hot tier (compaction is
/// destructive), so the whole fixture is rebuilt in untimed setup.
fn bench_compact_spans(c: &mut Criterion) {
    let mut group = c.benchmark_group("tael_backend_compact_spans");
    for &n in &[1_000usize, 10_000] {
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let bench = BenchBackend::new();
                    for chunk in make_spans(n).chunks(1_000) {
                        bench.backend.insert_spans(chunk).unwrap();
                    }
                    bench
                },
                |bench| {
                    // The fixture's timestamps are historical, so `now` as the
                    // cutoff moves all `n` spans.
                    let moved = bench.backend.compact_spans(black_box(Utc::now())).unwrap();
                    debug_assert_eq!(moved, n);
                },
                // One live engine at a time: each holds LSM and index file
                // handles plus its own WAL namespace.
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_insert_spans,
    bench_insert_signals,
    bench_query_traces,
    bench_get_trace,
    bench_aggregations,
    bench_compact_spans
);
criterion_main!(benches);
