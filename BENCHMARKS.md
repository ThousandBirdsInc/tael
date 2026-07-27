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

**Run A — 2026-07-27** (`tael-backend` and serialization tables below):

- OS: Linux 6.18.5 x86_64, 4 vCPU Intel Xeon @ 2.80GHz, 15 GiB RAM, container
- Rust: `rustc 1.94.1 (e408947bf 2026-03-25)`
- Cargo: `cargo 1.94.1 (29ea6fb6a 2026-03-24)`
- Plot backend: `plotters` (gnuplot not installed)
- **Shortened sampling**: `--warm-up-time 1 --measurement-time 2`. Still 100
  samples per case, but a full-length run would tighten the intervals. Treat
  these as order-of-magnitude, not as a regression baseline.

**Run B — 2026-05-28** (blob store and the legacy DuckDB appendix):

- OS: Darwin 25.4.0 arm64
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11)`
- Cargo: `cargo 1.93.1 (083ac5135 2025-12-15)`
- Plot backend: `plotters` (gnuplot not installed)
- Criterion's default sampling.

## tael-backend — ingest

Each insert is a WAL append followed by the LSM hot-tier write. The backend is
built once per case and is already holding data, so this measures steady-state
ingest rather than a cold first write.

The "before" column is the same benchmark on the same machine before the
storage engine's write path was reworked (see *What changed* below).

| Benchmark | Before | Mean | Range | Throughput |
| --- | ---: | ---: | ---: | ---: |
| `tael_backend_insert_spans/1` | 2.7224 ms | 16.687 us | 15.981-17.374 us | 59.927 Kelem/s |
| `tael_backend_insert_spans/100` | 2.9776 ms | 953.73 us | 920.60-997.89 us | 104.85 Kelem/s |
| `tael_backend_insert_spans/1000` | 11.147 ms | 8.8998 ms | 8.6859-9.1537 ms | 112.36 Kelem/s |
| `tael_backend_insert_signals/logs/1000` | 9.7420 ms | 5.0698 ms | 4.9534-5.1994 ms | 197.25 Kelem/s |
| `tael_backend_insert_signals/metrics/1000` | 7.6467 ms | 3.9738 ms | 3.8928-4.0917 ms | 251.65 Kelem/s |

**Batch size no longer decides throughput.** A one-span insert used to cost
2.72 ms and a hundred-span insert 2.98 ms, because a fixed per-call durability
cost dominated everything below roughly a thousand records; unbatched ingest
was 367 spans/s. It now runs at 60-112k spans/s across every batch size, and
the remaining variation is per-record work rather than a fixed cost waiting to
be amortized. A client that sends spans one at a time is no longer punished for
it.

## tael-backend — hot-tier queries

Over a backend pre-populated with 10,000 spans (8 services, 1000 traces, all
resident in the hot tier; timestamps spread across the preceding ~17 minutes).

| Benchmark | Before | Mean | Range |
| --- | ---: | ---: | ---: |
| `tael_backend_query_traces/service` | 3.1376 ms | 342.65 us | 339.56-345.69 us |
| `tael_backend_query_traces/service_and_window` | 2.9908 ms | 353.02 us | 345.90-361.81 us |
| `tael_backend_query_traces/error_status` | 18.500 ms | 331.24 us | 328.24-334.55 us |
| `tael_backend_get_trace/10k_spans` | 31.111 us | 23.642 us | 23.468-23.847 us |

`get_trace` is an indexed lookup on `trace_id` and is fast regardless of how
much data surrounds the trace.

`query_traces` used to walk the time index newest-first and deserialize every
span it passed, so its cost was inversely proportional to the filter's
selectivity — `status=error` matches 1 span in 50, so returning 100 results
examined ~5000 rows and took 18.5 ms. There are now indexes for the two filters
that matter (service, and errors only), so the three cases above cost about the
same thing: each visits roughly the 100 rows it returns. **A selective filter is
now the fast case rather than the slow one.**

Filters with no index of their own — duration bounds, operation substrings —
still visit every row in the window, but the index entry carries enough of the
span (service, operation, duration, status) to reject non-matches without
reading the span at all. `explain` reports which index was chosen along with
rows scanned versus rows actually decoded.

## tael-backend — aggregations

Same 10,000-span fixture.

| Benchmark | Before | Mean | Range |
| --- | ---: | ---: | ---: |
| `tael_backend_aggregate/list_services/10k_spans` | 28.724 ms | 5.3365 ms | 5.3027-5.3718 ms |
| `tael_backend_aggregate/query_summary/10k_spans` | 64.901 ms | 5.3519 ms | 5.3089-5.4002 ms |
| `tael_backend_aggregate/query_summary_scoped/10k_spans` | 41.639 ms | 839.50 us | 834.14-845.05 us |

`summarize` is the command SKILL.md tells agents to run first, and at 65 ms per
10k spans a million-span hot tier would have put it in the seconds. It now
computes entirely from index entries: everything it needs about a span (trace
id, service, operation, duration, error) is in the index key and its covering
header, so no span is read. **The reported numbers are unchanged** — the full
duration set is still kept and sorted, so percentiles are exact rather than
bucket estimates.

These still scale linearly with hot-tier size. The constant is now ~0.5 us per
span instead of ~6.5 us, which moves a million-span summary from seconds to
about half a second.

## tael-backend — compaction

`compact_spans(cutoff)` evicts every span older than the cutoff from the LSM
hot tier and writes it to the Parquet cold tier. Each iteration rebuilds the
hot tier in untimed setup, so this is the cost of one full hot→cold roll.

| Benchmark | Before | Mean | Range | Throughput |
| --- | ---: | ---: | ---: | ---: |
| `tael_backend_compact_spans/1000` | 18.065 ms | 19.741 ms | 19.397-20.124 ms | 50.656 Kelem/s |
| `tael_backend_compact_spans/10000` | 154.82 ms | 171.12 ms | 168.93-173.52 ms | 58.437 Kelem/s |

**This one got ~10% slower**, and the reason is the read speedups above:
evicting a span now deletes its entries from three indexes instead of one. That
is the cost side of the trade, paid once per span by a background task, in
exchange for the query numbers in the two tables above. Compaction remains
linear and slightly cheaper per span in bulk; at ~58k spans/s it is still
roughly "one extra pass over everything ingested".

## What changed

The tables above compare against the same benchmarks run on the same machine
before three changes to the storage engine.

**The WAL cursor advance came off the write path.** Profiling the ingest path
showed the fixed ~2.7 ms per `insert_spans` call was almost entirely one thing:
marking the record applied. The append cost 8 us and the hot-tier write cost
8 us; advancing walrus's read cursor cost 1.9 ms, because it persists the
cursor by rewriting and fsyncing an index file, and that happened once per
insert. The cursor now advances in checkpoints — one per 1024 applied records,
consuming them in a batch that persists the cursor once. Between checkpoints
the WAL holds records that are already applied, so a crash replays them; that
made idempotent apply a requirement, and log and metric hot-tier keys (which
carried a process sequence number) are now derived from record content.

**Span scans got secondary indexes.** There are now three time-ordered indexes
over the spans keyspace — by time, by service, and errors only — and each entry
carries a covering header holding the span's service, operation, duration and
status. The scan picks an index from the query's filters, and answers what it
can from the header without the second keyspace lookup or the span decode.
`summarize` and `anomalies` read these indexes directly and never materialize a
span from the hot tier at all.

**Records are stored as MessagePack rather than JSON.** This was the smallest
of the three by a wide margin — see the serialization table below, where the
codec is worth 20-35%, against the ~160x on unbatched ingest that came from the
WAL change. It is in the same commit because the format was already changing.

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

Pure-CPU encode/decode of the telemetry models. MessagePack is what the storage
engine writes; JSON is still the API's wire format, and is kept here because
the gap between them is why storage stopped using it. Run A.

| Benchmark | Mean | Range | Throughput |
| --- | ---: | ---: | ---: |
| `serialize_spans/json/1000` | 944.67 us | 921.87-972.58 us | 1.0586 Melem/s |
| `serialize_spans/msgpack/1000` | 772.52 us | 767.83-778.42 us | 1.2945 Melem/s |
| `deserialize_spans/json/1000` | 2.7971 ms | 2.7301-2.8754 ms | 357.51 Kelem/s |
| `deserialize_spans/msgpack/1000` | 2.0648 ms | 2.0494-2.0834 ms | 484.30 Kelem/s |
| `serialize_signals/json/logs/1000` | 644.47 us | 636.52-653.92 us | 1.5517 Melem/s |
| `serialize_signals/msgpack/logs/1000` | 595.81 us | 587.59-604.35 us | 1.6784 Melem/s |
| `serialize_signals/json/metrics/1000` | 431.54 us | 423.43-441.83 us | 2.3173 Melem/s |
| `serialize_signals/msgpack/metrics/1000` | 346.11 us | 343.76-348.65 us | 2.8892 Melem/s |

MessagePack is 20-35% faster than JSON on these models — worth having, and much
smaller than the difference the indexes made. Decode is the slower direction for
both, and at ~484 Kelem/s it is the floor under any read path that has to
materialize spans. That floor is why the aggregation paths were changed to read
index entries instead: the fastest decode is the one that does not happen.

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
