# Benchmarks

Criterion benchmark results for the tael server hot paths.

`tael-backend` is the default storage engine and leads this document. The
DuckDB numbers in the appendix are from the legacy backend, which is off by
default and only compiled with `--features duckdb`.

HTML reports are written to `target/criterion/`.

## Commands

```sh
cargo bench -p tael-server                                       # default targets
cargo bench -p tael-server --bench tael_backend                  # storage engine only
cargo bench -p tael-server --features bench-storage --bench storage  # legacy DuckDB
```

The default command runs `tael_backend`, `blob_store`, and `serialization`.
The `storage` target is feature-gated because it links DuckDB.

## How these numbers were collected

Two separate runs, on different machines. **They are not comparable to each
other** — in particular, do not read the tael-backend and DuckDB tables as a
head-to-head.

**Run A — 2026-07-26** (`tael-backend` tables below):

- OS: Linux 6.18.5 x86_64, 4 vCPU Intel Xeon @ 2.80GHz, 15 GiB RAM, container
- Rust: `rustc 1.94.1 (e408947bf 2026-03-25)`
- Cargo: `cargo 1.94.1 (29ea6fb6a 2026-03-24)`
- Plot backend: `plotters` (gnuplot not installed)
- **Shortened sampling**: `--warm-up-time 1 --measurement-time 2`. Still 100
  samples per case, but a full-length run would tighten the intervals. Treat
  these as order-of-magnitude, not as a regression baseline.

**Run B — 2026-05-28** (blob store, serialization, and the legacy DuckDB
appendix):

- OS: Darwin 25.4.0 arm64
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Cargo: `cargo 1.93.1 (083ac5135 2025-12-15)`
- Plot backend: `plotters` (gnuplot not installed)
- Criterion's default sampling.

## tael-backend — ingest

Each insert is a WAL append + fsync, followed by the LSM hot-tier write. The
backend is built once per case and is already holding data, so this measures
steady-state ingest rather than a cold first write.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `tael_backend_insert_spans/1` | 2.7224 ms | 2.4061-3.0919 ms | 367.32 elem/s |
| `tael_backend_insert_spans/100` | 2.9776 ms | 2.9188-3.0407 ms | 33.584 Kelem/s |
| `tael_backend_insert_spans/1000` | 11.147 ms | 10.490-12.161 ms | 89.709 Kelem/s |
| `tael_backend_insert_signals/logs/1000` | 9.7420 ms | 8.4652-11.494 ms | 102.65 Kelem/s |
| `tael_backend_insert_signals/metrics/1000` | 7.6467 ms | 7.1183-8.3950 ms | 130.78 Kelem/s |

**Batching is the whole story.** A one-span insert costs 2.72 ms and a
hundred-span insert costs 2.98 ms — the fixed cost of the durability barrier
dominates until roughly a thousand records per call, where per-span cost
finally falls to ~11 us. Ingest throughput is therefore a property of the
client's batch size, not of the engine: 367 spans/s unbatched, ~90k spans/s at
batches of 1000. The OTLP receiver already inserts a whole request's spans in
one call, so real ingest sits at the batched end.

## tael-backend — hot-tier queries

Over a backend pre-populated with 10,000 spans (8 services, 1000 traces, all
resident in the hot tier; timestamps spread across the preceding ~17 minutes).

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `tael_backend_query_traces/service` | 3.1376 ms | 3.0398-3.2490 ms | n/a |
| `tael_backend_query_traces/service_and_window` | 2.9908 ms | 2.9488-3.0364 ms | n/a |
| `tael_backend_query_traces/error_status` | 18.500 ms | 18.258-18.775 ms | n/a |
| `tael_backend_get_trace/10k_spans` | 31.111 us | 30.709-31.571 us | n/a |

`get_trace` is an indexed lookup on `trace_id` and is fast regardless of how
much data surrounds the trace.

`query_traces` is not indexed: it walks the time index newest-first,
deserializes each span, and stops once `limit` matches are found. Cost is
therefore proportional to *rows scanned to fill the limit*, i.e. inversely
proportional to the filter's selectivity. `service` matches 1 span in 8, so
~800 rows are examined to return 100 (3.1 ms). `status=error` matches 1 in 50,
so ~5000 rows are examined for the same 100 results (18.5 ms). Adding
`last_seconds` on top of a service filter costs nothing measurable, because the
scan is already in time order. A low-selectivity filter over a large hot tier
is the current worst case for this engine.

## tael-backend — aggregations

Same 10,000-span fixture. Both paths are full scans of the hot tier with
per-row JSON deserialization; neither uses a precomputed rollup.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `tael_backend_aggregate/list_services/10k_spans` | 28.724 ms | 28.507-28.955 ms | n/a |
| `tael_backend_aggregate/query_summary/10k_spans` | 64.901 ms | 64.090-65.784 ms | n/a |
| `tael_backend_aggregate/query_summary_scoped/10k_spans` | 41.639 ms | 40.684-42.695 ms | n/a |

These are the slowest read paths by an order of magnitude, and they scale
linearly with hot-tier size — 10k spans is a small hot tier. `query_summary`
costs about 6.5 us per span; a hot tier holding a million spans would put
`tael summary` in the seconds. Passing a `service` filter cuts the work by
about a third but does not change the scan.

## tael-backend — compaction

`compact_spans(cutoff)` evicts every span older than the cutoff from the LSM
hot tier and writes it to the Parquet cold tier. Each iteration rebuilds the
hot tier in untimed setup, so this is the cost of one full hot→cold roll.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `tael_backend_compact_spans/1000` | 18.065 ms | 17.488-18.719 ms | 55.356 Kelem/s |
| `tael_backend_compact_spans/10000` | 154.82 ms | 149.52-161.54 ms | 64.590 Kelem/s |

Compaction is linear and slightly cheaper per span in bulk. At ~65k spans/s it
is comparable to batched ingest, so the background compactor's cost is roughly
"one extra pass over everything ingested" — sized in seconds per hour of
retained traffic, not minutes.

## Blob Store

Content-addressed store for LLM prompt/completion payloads. Run B.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `blob_put_unique/1` | 207.23 us | 196.27-220.34 us | 4.7125 MiB/s |
| `blob_put_unique/16` | 182.50 us | 181.02-184.17 us | 85.616 MiB/s |
| `blob_put_dedup/4kb` | 9.0938 us | 8.9625-9.2379 us | n/a |
| `blob_get/4kb` | 10.624 us | 10.559-10.701 us | n/a |
| `blob_gc/100` | 15.974 ms | 15.780-16.192 ms | n/a |
| `blob_gc/1000` | 123.10 ms | 121.90-124.40 ms | n/a |

## Serialization

Pure-CPU JSON encode/decode of the telemetry models, which every storage path
pays on both sides. Run B.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `serialize_spans/1` | 456.10 ns | 453.11-459.59 ns | 2.1925 Melem/s |
| `serialize_spans/100` | 41.485 us | 41.395-41.588 us | 2.4105 Melem/s |
| `serialize_spans/1000` | 404.23 us | 403.14-405.47 us | 2.4738 Melem/s |
| `deserialize_spans/1` | 835.86 ns | 832.75-838.97 ns | 1.1964 Melem/s |
| `deserialize_spans/100` | 91.358 us | 91.107-91.629 us | 1.0946 Melem/s |
| `deserialize_spans/1000` | 920.54 us | 918.39-922.91 us | 1.0863 Melem/s |
| `serialize_signals/logs/1000` | 291.89 us | 291.01-292.76 us | 3.4260 Melem/s |
| `serialize_signals/metrics/1000` | 196.80 us | 196.28-197.33 us | 5.0814 Melem/s |

Deserialization at ~1.1 Melem/s is the floor under every scan-based read path
above: a full 10k-span scan cannot beat ~9 ms on Run B's hardware no matter
what the storage layer does.

## Appendix: legacy DuckDB backend

`DuckDbStore` is **not** the default engine. It is retained behind
`--features duckdb` for existing deployments, and these numbers are kept for
continuity only. Collected on Run B's machine, so they cannot be compared
against the tael-backend tables above.

Run with `cargo bench -p tael-server --features bench-storage --bench storage`.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `duckdb_insert_spans/100` | 118.92 ms | 115.11-123.30 ms | 840.89 elem/s |
| `duckdb_insert_spans/1000` | 1.0986 s | 1.0569-1.1516 s | 910.27 elem/s |
| `duckdb_query_traces/10k_spans` | 642.85 us | 634.24-651.71 us | n/a |
| `duckdb_get_trace/10k_spans` | 255.17 us | 242.02-272.69 us | n/a |

Note that the DuckDB insert cases build a fresh store per iteration, so they
include table creation; the tael-backend insert cases measure steady-state
appends into an existing store. That difference alone makes the two ingest
columns incomparable, on top of the different hardware.

Criterion reported prior-run deltas for some storage cases from local
`target/criterion` history. Treat those as machine-local comparisons, not a
project baseline.
