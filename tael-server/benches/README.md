# tael-server benchmarks

Criterion micro-benchmarks for the hot paths of the observability server.

## Running

```sh
# Default set (compile-verified APIs): serialization + blob store
cargo bench -p tael-server

# A single target
cargo bench -p tael-server --bench serialization
cargo bench -p tael-server --bench blob_store

# Storage backend benches (DuckDB, via the Store trait) — feature-gated
cargo bench -p tael-server --features bench-storage --bench storage
```

HTML reports are written to `target/criterion/`.

## Targets

| Bench           | Covers                                                               |
|-----------------|---------------------------------------------------------------------|
| `serialization` | JSON serialize/deserialize of `Span`/`LogRecord`/`MetricPoint` — the pure-CPU cost on every ingest write and query read. |
| `blob_store`    | `BlobStore` `put` (unique vs. dedup fast path), `get`, and `gc` mark-and-sweep — the content-addressed payload store for LLM prompts/completions. |
| `storage`       | `DuckDbStore` `insert_spans` / `query_traces` / `get_trace` through the `Store` trait. Feature-gated behind `bench-storage`. |

Shared deterministic fixtures live in `benches/common/mod.rs`.

## Note on `storage`

The `storage` target is gated behind the `bench-storage` feature so the default
bench run stays focused on CPU and blob-store hot paths. Run it separately when
you want DuckDB-backed ingest/query numbers.

## Suggested follow-up targets

High-value hot paths not yet covered (each needs its constructor/signature
confirmed first):

- OTLP ingest parsing — `ingest/otlp.rs` `export()` / `extract_llm_span()`.
- TaelBackend hot tier (LSM) `insert_spans` / `query_traces` / `get_trace`.
- TaelBackend cold tier Parquet `write_spans` and `write_downsampled` (rollup).
- `SearchIndex` `index_span` / `commit` / `search_trace_ids` (Tantivy).
