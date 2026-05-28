# Benchmarks

Criterion benchmark results for the tael server hot paths.

Collected on 2026-05-28 on:

- OS: Darwin 25.4.0 arm64
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Cargo: `cargo 1.93.1 (083ac5135 2025-12-15)`
- Plot backend: Criterion used `plotters` because `gnuplot` was not installed.

HTML reports are written to `target/criterion/`.

## Commands

```sh
cargo bench -p tael-server
cargo bench -p tael-server --features bench-storage --bench storage
```

The default command runs the compile-verified `blob_store` and `serialization`
targets. The `storage` target is feature-gated because it exercises the DuckDB
backend directly through the `Store` trait.

## Blob Store

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `blob_put_unique/1` | 207.23 us | 196.27-220.34 us | 4.7125 MiB/s |
| `blob_put_unique/16` | 182.50 us | 181.02-184.17 us | 85.616 MiB/s |
| `blob_put_dedup/4kb` | 9.0938 us | 8.9625-9.2379 us | n/a |
| `blob_get/4kb` | 10.624 us | 10.559-10.701 us | n/a |
| `blob_gc/100` | 15.974 ms | 15.780-16.192 ms | n/a |
| `blob_gc/1000` | 123.10 ms | 121.90-124.40 ms | n/a |

## Serialization

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

## DuckDB Storage

Run with `cargo bench -p tael-server --features bench-storage --bench storage`.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `duckdb_insert_spans/100` | 118.92 ms | 115.11-123.30 ms | 840.89 elem/s |
| `duckdb_insert_spans/1000` | 1.0986 s | 1.0569-1.1516 s | 910.27 elem/s |
| `duckdb_query_traces/10k_spans` | 642.85 us | 634.24-651.71 us | n/a |
| `duckdb_get_trace/10k_spans` | 255.17 us | 242.02-272.69 us | n/a |

Criterion reported prior-run deltas for some storage cases from local
`target/criterion` history. Treat those as machine-local comparisons, not a
project baseline.
