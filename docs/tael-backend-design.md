# tael-backend: A Storage Engine for OTel + LLM Traces

> Status: Draft · Owner: colton@thousandbirds.ai · Last updated: 2026-05-25
>
> Companion to the top-level [`DESIGN.md`](../DESIGN.md). That doc describes
> tael as an agent-native observability *platform*; this doc specifies the
> storage engine underneath it.

## Problem

Tael v1 stores traces, metrics, and logs in an embedded DuckDB file (see
`tael-server/src/storage/duckdb_store.rs`). That was the right call for getting
to a single-binary, zero-dependency product fast. Two pressures now push past
what a single DuckDB file does well:

1. **LLM traces have an awkward shape.** A span for `chat.completion` carries a
   model name, provider, token counts, and a USD cost — plus a *prompt* and a
   *completion* that can each be tens of kilobytes. Stuffing those payloads into
   the same columnar table as numeric span data wrecks compression and makes
   every scan drag the big text columns along.
2. **DuckDB is single-writer.** Under bursty OTLP ingest we already serialize
   writes behind a batching layer. As ingest volume grows we want append-heavy
   writes decoupled from analytical reads, with retention that can keep "30 days
   of full payloads, 1 year of metadata" without a rewrite.

`tael-backend` is a purpose-built engine that keeps the embedded, single-binary
story for v1 while giving traces — especially LLM traces — a storage layout
that fits their access pattern: append-heavy, read by time range, queried
analytically, with large payloads kept out of the hot path.

## Goals

1. **One model for OTel spans and LLM spans.** An LLM call *is* a span. Model it
   as the existing `Span` plus a typed `llm_span` extension, not a separate
   table that has to be joined back.
2. **Tiered storage.** A hot tier for the last ~24h (low-latency writes and
   reads) that rolls into immutable columnar files for everything older.
3. **Columnar-first analytics.** Time-range scans, `GROUP BY model`, cost
   rollups, and span-tree reconstruction should all be cheap.
4. **Keep big payloads out of the columnar table.** Prompts and completions are
   content-addressed blobs, referenced by hash.
5. **Stay OTLP-native.** Ingest is the existing OTLP gRPC/HTTP receivers — no
   new SDK. The backend is a storage swap behind the `Store` trait, not a
   protocol change.
6. **Retention and GC are first-class**, configurable per signal and per tier,
   from day one.

## Non-Goals

- A general-purpose OLTP database. No row-level updates, no transactions across
  arbitrary keys. Traces are append-only.
- Replacing the query *surface*. The CLI and REST/gRPC API in `DESIGN.md` stay
  exactly as they are; this changes what sits behind them.
- Distributed/clustered storage in v1 (the engine should not *preclude* it —
  see "Scale path" — but single-node is the v1 target).
- Multi-tenancy enforcement in v1. The layout is partitioned by tenant so the
  primitive exists, but auth/isolation stays as described in `DESIGN.md`.

## Architecture Overview

```
        OTLP gRPC/HTTP (4317/4318)   ·   Prometheus remote-write (9090)
                              │
                              ▼
              ┌────────────────────────────────┐
              │        Ingestion (existing)     │
              │  decode → normalize → enrich    │
              └───────────────┬─────────────────┘
                              │  Span(+LlmSpan) · LogRecord · MetricPoint  + blobs
                              ▼
        ┌──────────────────────────────────────────────┐
        │                tael-backend                    │
        │                                                │
        │   ┌──────────┐   write   ┌──────────────────┐  │
        │   │   WAL     │◀─────────│  Ingest buffer    │  │
        │   │ (walrus)  │  fsync   │  (in-memory)      │  │
        │   └────┬─────┘   + ack    └────────┬─────────┘  │
        │        │                           │            │
        │        ▼                           ▼            │
        │   ┌────────────────────────┐ ┌──────────────┐  │
        │   │  HOT TIER  LSM, ~24h    │ │  BLOB STORE  │  │
        │   │  spans │ logs │ metrics │ │  sha256-keyed │  │
        │   │  (separate keyspaces)   │ │  prompts +    │  │
        │   │  (fjall / RocksDB)      │ │  big bodies   │  │
        │   └──────────┬─────────────┘ └──────────────┘  │
        │          │  background compactor (+ downsample) │
        │          ▼                                       │
        │   ┌──────────────────────────────────────────┐  │
        │   │   COLD TIER: Parquet  tenant/date/hour     │  │
        │   │   spans  → sorted by trace_id              │  │
        │   │   logs   → sorted by (service, ts)         │  │
        │   │   metrics→ sorted by (name, labels, ts)    │  │
        │   │   metrics_5m → downsampled rollups (daily) │  │
        │   └──────────────────────────────────────────┘  │
        │                                                  │
        │   ┌──────────────┐   ┌───────────────────────┐  │
        │   │  Tantivy      │   │  HNSW (optional)       │  │
        │   │  full-text on │   │  semantic, per-tenant  │  │
        │   │  payloads+logs│   │  per-time-window       │  │
        │   └──────────────┘   └───────────────────────┘  │
        │                                                  │
        │          ▲   all reads unified through           │
        │          │        DataFusion                     │
        └──────────┼───────────────────────────────────────┘
                   │  Store trait (same API as DuckDbStore)
                   ▼
            Query Engine → CLI / REST / gRPC
```

The key idea: **writes land in a WAL-backed LSM hot tier and ack fast; a
background compactor rolls aging data into immutable, sorted Parquet; all reads
go through a single DataFusion context that unions the hot tier and the Parquet
files.** Big payloads never enter either tier — they're hashed and stored once
in the blob store.

## Why not just keep DuckDB?

DuckDB (today's choice) stays excellent for single-node analytics, but two
limits push past it:

- **Single-writer.** Bursty OTLP ingest already serializes behind a batching
  layer; there's no clean way to decouple append-heavy writes from analytical
  reads inside one DuckDB file.
- **No native tiering.** No hot/cold split, no object-store roll-off, no
  per-tier retention — exactly the "30 days of payloads, 1 year of metadata"
  shape we need.

We build `tael-backend` because tael's pitch is an *agent-native* engine where
LLM-trace ergonomics (token/cost columns, payload dedup, semantic search over
prompts) and the embedded single-binary footprint *are* the product — not a
storage layer we'd want to outsource to a separate server process. The Rust
Arrow/Parquet/DataFusion stack gives us a columnar query engine without writing
a planner, so the incremental cost over DuckDB is mostly the hot tier and the
compactor; everything else (SQL, predicate pushdown, aggregation) comes from
DataFusion. Staying all-Rust and embedded also keeps the single-binary,
cross-compile, zero-external-dependency story intact — no separate database
daemon to run, secure, or version.

Decision is deliberately revisitable — see [Open Questions](#open-questions).
The `Store` trait boundary means DuckDB remains a supported backend during the
transition.

## Three signals, shared machinery, specialized layout

Tael ingests **traces (spans), logs, and metrics** today (see `DESIGN.md`), and
the backend must hold all three. The trap is to either (a) jam them into one
table, or (b) build three unrelated engines. We do neither: all three share the
same *machinery* — WAL durability, an LSM hot tier, Parquet cold tier, DataFusion
reads, and per-tier retention — but each gets a **physical layout tuned to its
access pattern**, because their shapes genuinely differ.

| | **Spans** | **Logs** | **Metrics** |
|---|---|---|---|
| Shape | tree of typed records | wide timestamped records w/ a text body | `(name, labels) → (ts, value)` series |
| Volume driver | fan-out per request | log lines per request | scrape interval × cardinality |
| Dominant read | span-tree by `trace_id`; time-range | time-range + severity/service filter; correlate by `trace_id`; full-text on body | range scan per series; aggregation; PromQL |
| Big payloads | prompts/completions (blobbed) | occasional large bodies (blobbed over a threshold) | none — values are tiny numerics |
| Hot-tier sort key | `(tenant, trace_id, span_id)` + `(tenant, start_ns)` | `(tenant, service, ts)` + `(tenant, trace_id)` | `(tenant, name, labels_hash, ts)` |
| Cold Parquet sort | by `trace_id` within hour | by `(service, ts)` within hour | by `(name, labels_hash, ts)` within hour |
| Special compaction | — | full-text index build | **downsampling** to 5m rollups |
| Query surface | trace filters, `get trace`, correlate | log filters, `--text` | PromQL (`promql.rs`) |

The rows that share machinery share the same crash-safe write path and the same
DataFusion read context (so cross-signal correlation by `trace_id` is one query),
while the columns and sort order each table uses are chosen per signal. The rest
of this section specifies each.

## Data Model

### Span (unchanged base)

The base span is the existing `storage::models::Span` — `trace_id`, `span_id`,
`parent_span_id`, `service`, `operation`, `start_time`/`end_time`,
`duration_ms`, `status`, `attributes`, `events`. OTel spans store nothing more.

### LlmSpan extension

LLM spans are ordinary spans whose `kind` marks them as an LLM call and which
carry a typed extension. Well-known LLM attributes are flattened into typed
columns; everything else stays in the attribute map.

```rust
/// Typed extension attached to spans where SpanKind == Llm.
/// Mirrors OpenTelemetry GenAI semantic conventions (gen_ai.*).
pub struct LlmSpan {
    pub span_id: String,          // FK to Span
    pub provider: String,         // "anthropic", "openai", ...
    pub model: String,            // "claude-opus-4-7", ...
    pub operation: LlmOperation,  // chat | completion | embedding | tool

    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub cost_usd: Option<f64>,

    // Streaming: store the metrics, not every chunk (see Hard Parts).
    pub ttft_ms: Option<f64>,        // time to first token
    pub inter_token_ms: Option<f64>, // mean inter-token latency

    // Payloads live in the blob store; we keep only the hashes.
    pub prompt_sha256: Option<String>,
    pub completion_sha256: Option<String>,

    pub finish_reason: Option<String>,
    pub temperature: Option<f64>,
}
```

### Attribute storage

Flatten the well-known LLM attributes (above) into typed columns. Dump the
long tail into an Arrow `Map<String, Utf8>` (today's `HashMap<String,String>`
serializes straight into this) so we never schema-on-write the unbounded set of
custom `gen_ai.*` / app-specific keys. Indexed lookups on hot attributes are
served by the LSM tier and by Parquet column statistics + row-group pruning.

### Payload blobs (content-addressed)

Prompts and completions are stored as compressed blobs keyed by
`sha256(content)`:

```
blobs/<first2>/<next2>/<full-sha256>      # snap-compressed
```

A span references them by hash. Benefits:

- **Free dedup.** Identical system prompts (the common case across thousands of
  calls) are stored once.
- **Compression stays clean.** The columnar tables hold only fixed-width numeric
  and short-string columns, which compress far better without giant text columns
  interleaved.
- **Retention decoupling.** "Keep metadata 1 year, payloads 30 days" is just two
  GC clocks (see [Retention](#retention--gc)). Dropping a blob leaves the span
  intact with a dangling-but-explainable hash.

`snap` is already a workspace dependency; reuse it for blob compression.

### Log records

The base is the existing `storage::models::LogRecord` — `timestamp`,
`observed_timestamp`, `trace_id`, `span_id`, `severity`, `severity_text`,
`body`, `service`, `attributes`. No new typed fields are needed; logs map almost
directly onto the same columnar machinery as spans. Two layout decisions:

- **`trace_id`/`span_id` are first-class columns**, so a log row co-resides with
  its span in the DataFusion context and `tael correlate` is a single
  `trace_id` predicate across the spans and logs tables — no cross-store join.
- **Large bodies blob out.** Most log bodies are short and stay inline. Bodies
  over a threshold (e.g. 8 KiB — stack traces, dumped payloads) go to the same
  content-addressed blob store as prompts, referenced by `body_sha256`. This
  keeps the columnar `body` column compressible and dedups repeated stack traces
  for free. The attribute tail uses the same Arrow `Map<Utf8,Utf8>` as spans.

### Metric points

The base is the existing `storage::models::MetricPoint`. Metrics are the one
signal that does **not** share the span/log row shape — they are time-series:

```
(tenant, metric_name, metric_type, labels) → stream of (ts_ns, value)
```

- **Labels are hashed.** A stable `labels_hash` (e.g. xxhash of the sorted
  label set) identifies each series; the full label map is stored once per series
  (a small dimension table / dictionary) rather than repeated on every point.
  Points carry only `(labels_hash, ts_ns, value)` — tiny, dense, and extremely
  compressible.
- **Histograms** keep their bucket layout (bounds + counts) as typed columns; we
  do not explode buckets into separate series.
- **No blobs.** Metric values are numeric; nothing leaves the columnar tables.

This series-oriented layout is what makes PromQL range scans (`promql.rs`) cheap
and what the downsampling compactor (below) operates on.

## Storage Tiers

### Write path (shared by all three signals)

1. Ingestion hands the backend a normalized record — a `Span` (+ optional
   `LlmSpan`), a `LogRecord`, or a batch of `MetricPoint`s — plus any extracted
   payloads (prompts/completions, or oversized log bodies).
2. Payloads are hashed and written to the blob store (skip if the hash already
   exists). Metrics have none.
3. The record is appended to the **WAL** (`walrus-rust`, already a dependency;
   defaulting to `~/.tael/wal_files`) and fsync'd. A one-byte signal tag in
   each WAL record distinguishes span/log/metric so replay routes correctly.
4. Once durable, the write is **ack'd** to the OTLP/remote-write client and
   inserted into the in-memory buffer / LSM hot tier for its signal.

This is the standard WAL → ack → apply ordering, identical across signals: we
never ack data we can't recover after a crash, and client retry semantics stay
correct.

### Hot tier (LSM, ~24h)

An embedded LSM (`fjall`, pure-Rust, or RocksDB via bindings) holds recent data.
Each signal lives in its own keyspace (column family / partition) with keys
chosen for its dominant reads:

- **Spans** — `(tenant, trace_id, span_id)` for live span-tree reconstruction,
  plus `(tenant, start_ns)` for "what happened recently". (Logs are co-keyed by
  `(tenant, trace_id)` so a trace's spans *and* logs are both prefix scans.)
- **Logs** — `(tenant, service, ts_ns)` for the common severity/service +
  time-range read, plus `(tenant, trace_id)` for correlation.
- **Metrics** — `(tenant, name, labels_hash, ts_ns)` so a single series is a
  contiguous range scan, which is exactly what PromQL range selectors want.

The hot tier absorbs write bursts and serves the "last few minutes/hours" reads
that `tael watch`, `tael summarize`, and the live TUI hit constantly — across
all three signals.

### Cold tier (Parquet on local disk or object store)

A background **compactor** flushes aged data from each hot keyspace into
immutable Parquet files, one file set per signal, each sorted for its access
pattern:

```
<tenant>/date=YYYY-MM-DD/hour=HH/spans-<ulid>.parquet        # sorted by trace_id
<tenant>/date=YYYY-MM-DD/hour=HH/llm_spans-<ulid>.parquet     # sorted by trace_id
<tenant>/date=YYYY-MM-DD/hour=HH/logs-<ulid>.parquet          # sorted by (service, ts)
<tenant>/date=YYYY-MM-DD/hour=HH/metrics-<ulid>.parquet       # sorted by (name, labels_hash, ts)
<tenant>/date=YYYY-MM-DD/metrics_5m-<ulid>.parquet            # downsampled rollups (day-partitioned)
```

Sort order is the key physical decision per signal: **spans by `trace_id`** so a
span tree is one contiguous IO; **logs by `(service, ts)`** so service +
time-range scans prune hard; **metrics by `(name, labels_hash, ts)`** so a
single series reads contiguously. Parquet column statistics give row-group
pruning on the secondary predicates (`start_ns`, `severity`, `model`, etc.).

Downsampled metric rollups are partitioned by **day** (not hour) since they're
sparse and long-lived — see [Retention](#retention--gc).

v1 writes Parquet to the local data dir (single-binary story intact). The same
path string is an S3/R2 key prefix, so the cold tier moves to object storage in
v2 with no schema change — partitioning is already tenant/date/hour.

### Unified reads via DataFusion

All queries run through one DataFusion `SessionContext` that registers, **per
signal**, a hot + cold table pair:

- the hot tier (a custom `TableProvider` over each LSM keyspace), and
- the Parquet partitions (native DataFusion Parquet support, with partition
  pruning on the `date`/`hour` path columns).

So the context exposes logical tables `spans`, `llm_spans`, `logs`, `metrics`,
and `metrics_5m`, each a hot⊎cold union. Because they live in one context,
**cross-signal correlation is a single query** — `tael correlate <trace_id>`
becomes one `WHERE trace_id = …` fanned across `spans` and `logs`, no
cross-store join. DataFusion gives us SQL, predicate pushdown, and aggregation
for free — we don't write a query planner.

**PromQL** (`promql.rs`) does not translate to SQL cleanly, so it stays a
purpose-built evaluator that *sources* its raw samples from the `metrics` /
`metrics_5m` tables via DataFusion range scans, then applies PromQL functions on
top. DataFusion is the storage read path; PromQL semantics live above it.

The existing `Store` trait methods (trace queries, `get trace`, log/metric
queries, summary, anomalies, correlate) are all implemented as DataFusion
queries, so the CLI/API surface is unchanged.

## Search

Two optional indexes, both keyed so they don't grow unbounded:

- **Full-text (Tantivy):** indexes text from two signals — prompt/completion
  blobs keyed by `span_id` (`tael query traces --text "rate limit"`) **and log
  bodies keyed by the log row** (`tael query logs --text "connection refused"`).
  Built at compaction time from the inline body or the resolved body blob.
  Metrics have no text and are not indexed.
- **Semantic (HNSW, `usearch`/`hnsw_rs`):** optional embedding index over
  prompts/completions (and, if useful, log bodies), **per-tenant and
  per-time-window** so each index stays bounded and old windows can be dropped
  wholesale with retention. Off by default (requires an embedding source);
  enabled per deployment.

Both are derived, droppable indexes — losing them never loses data, only the
ability to search until rebuilt.

## The non-obvious hard parts

These are called out explicitly because they're the parts that bite later:

1. **Streaming responses.** Decide *up front* to store only the final completion
   plus `ttft_ms` and mean inter-token latency — **not** every token chunk.
   Per-chunk storage is ~100× the volume and is almost never what an agent
   queries. (Revisit only if a concrete debugging need for per-token timing
   appears.)
2. **Nested attribute explosion.** Flatten the well-known `gen_ai.*` attrs into
   typed columns; everything else goes into the Arrow `Map`. Do not try to
   promote every custom attribute to a column.
3. **trace_id cardinality.** Sort span Parquet by `trace_id` within each hour
   partition so span-tree queries are one IO. This is the single most important
   physical-layout decision for span read latency.
4. **Metric cardinality & downsampling.** Series count = `metric_name ×
   labels_cardinality`; a few high-cardinality labels (user_id, request_id on a
   metric) can blow this up. Hash labels into a series id, store the label set
   once per series, and **downsample raw points to 5m rollups** (min/max/avg/
   sum/count) past the raw window so long-term storage is bounded. Decide the
   rollup functions up front — adding one later means re-aggregating history.
5. **Retention & GC.** Build per-signal, per-tier retention from day one (next
   section). Bolting it on after the layout is frozen is painful.

## Retention & GC

Retention runs per signal *and* per tier, extending the policy already in
`DESIGN.md`:

```yaml
retention:
  traces:
    hot_tier: 24h      # LSM → Parquet rolloff
    metadata: 365d     # span rows in Parquet
    payloads: 30d      # prompt/completion blobs
  logs:
    hot_tier: 24h
    metadata: 14d      # log rows in Parquet
    payloads: 14d      # oversized body blobs (track metadata if longer)
  metrics:
    hot_tier: 24h
    raw: 30d           # raw points
    downsampled: 365d  # 5m rollups
    downsample_interval: 5m
```

Four GC mechanisms, all driven by an hourly scheduler (matching the cadence in
`DESIGN.md`):

- **Hot-tier rolloff** is the compactor: data older than `hot_tier` is flushed to
  Parquet and dropped from the LSM. Applies to all three signals.
- **Metadata GC** drops whole Parquet partitions older than `metadata` (per
  signal). Partition granularity (hour) makes this an `unlink`, not a rewrite.
- **Payload GC** deletes blobs (prompts/completions *and* oversized log bodies)
  whose newest referencing row is older than `payloads`. Because blobs are
  content-addressed and shared across signals, GC is reference-counted (or
  mark-and-sweep over live row hashes) — never delete a blob a live row points
  to. This is why "metadata 1 year, payloads 30 days" is two independent clocks.
- **Downsampling GC (metrics only)** is a second compaction pass: before raw
  points pass the `raw` window, roll each series into 5m aggregates
  (min/max/avg/sum/count) written to `metrics_5m` Parquet, then drop the raw
  partitions. Rollups live until `downsampled`. This preserves long-term trends
  for capacity planning while keeping metric storage bounded.

The "30 days of full payloads, 1 year of metadata" ask falls out naturally:
metadata is small numeric Parquet on a long clock, payloads are the expensive
blobs on a shorter one, and metrics shed resolution rather than history.

## Crate / Module Layout

`tael-backend` slots into the existing workspace behind the `Store` trait so it
can ship incrementally alongside `DuckDbStore`.

```
tael-server/src/storage/
  mod.rs              # Store trait; pub use DuckDbStore, TaelBackend
  models.rs           # Span, LogRecord, MetricPoint (existing) + LlmSpan, SpanKind
  duckdb_store.rs     # existing backend, kept during transition
  backend/
    mod.rs            # TaelBackend: implements Store
    wal.rs            # walrus-rust WAL: tagged append, fsync, replay-on-start
    hot.rs            # LSM hot tier: per-signal keyspaces + TableProviders
    compactor.rs      # hot → Parquet per signal; metric downsampling pass
    parquet/
      mod.rs          #   shared writer/partition helpers
      spans.rs        #   span + llm_span schema, sorted by trace_id
      logs.rs         #   log schema, sorted by (service, ts)
      metrics.rs      #   metric schema + 5m rollups, sorted by (name, labels, ts)
    blobs.rs          # content-addressed store (prompts + oversized log bodies)
    query.rs          # DataFusion ctx: spans/llm_spans/logs/metrics/metrics_5m
    promql.rs         # PromQL eval sourcing samples from the metrics tables
    search.rs         # Tantivy (payloads + log bodies) + optional HNSW
    retention.rs      # per-signal, per-tier GC + downsampling
```

The existing top-level `tael-server/src/promql.rs` evaluator is reused — under
the new backend it sources samples from the `metrics`/`metrics_5m` tables
instead of from DuckDB.

Backend selection at startup (tael-backend is the default):

```
tael-server                          # default: tael-backend (tiered engine)
tael-server --storage duckdb         # opt back into the legacy DuckDB engine
TAEL_STORAGE=duckdb tael-server      # same, via env (flag beats env)
```

## Migration Plan

Spans lead each step (they carry the LLM differentiator and the riskiest
layout); logs follow closely since they share the row machinery; metrics come a
beat later because of the series layout + downsampling.

1. **Define `LlmSpan` + `SpanKind`** in `models.rs`; teach the OTLP receiver to
   populate them from `gen_ai.*` semantic-convention attributes. (Independent of
   the storage swap — DuckDB can hold the new columns too.)
2. **Extract payloads to the blob store** in the ingestion pipeline — prompts/
   completions on spans *and* oversized log bodies — storing hashes on the row.
   Works for both backends.
3. **Build `TaelBackend`** behind the `Store` trait, signal by signal: WAL + hot
   tier + Parquet + DataFusion for spans first, then logs (same row machinery),
   then metrics (series layout + downsampling). Ship behind
   `--storage=tael-backend`, opt-in. Until a signal is migrated, it transparently
   falls back to DuckDB so the backend is never half-broken.
4. **Dual-run** in dev: write each signal to both, compare query results for
   parity on the existing test suite (`tael-test`).
5. **Add search** (Tantivy over payloads + log bodies first, HNSW behind a flag).
6. **Flip the default** once all three signals reach parity + retention is
   validated; keep DuckDB as a supported fallback.

## Milestones

### B1: Model + payloads
- [x] `LlmSpan` / `SpanKind` in `models.rs`
- [x] OTLP receiver maps `gen_ai.*` → typed LLM columns
- [x] Content-addressed blob store (sha256 + snap): prompts + oversized log bodies
- [x] `tael get trace` resolves payload hashes; logs resolve body blobs (via `GET /api/v1/blobs/{sha256}`)

### B2: Hot tier + WAL
- [x] Tagged WAL append/fsync/ack + crash replay routing by signal (walrus-rust)
- [x] LSM hot tier (`fjall`), per-signal keyspaces:
  - [x] spans `trace_id\0span_id` + `be(start_ns)…` time index
  - [x] logs `be(ts)+seq`
  - [x] metrics `name\0be(ts)+seq` (series-dictionary/labels_hash deferred to cold tier)
- [x] `Store` core reads served from hot tier (traces/get/services/logs/metrics)

### B3: Cold tier + query
- [x] Compactor → Parquet, all 3 signals, per-signal sort (spans/trace_id, logs/(service,ts), metrics/(name,ts))
- [x] Metric downsampling pass → `metrics_5m` rollups (5m min/max/sum/count, day-partitioned)
- [x] hot⊎cold unified reads — hand-rolled union (design Open Q #2 sanctions this over DataFusion)
- [x] Read-only SQL surface (`GET /api/v1/sql`, `tael query sql`) over the analytical projection
- [ ] PromQL evaluator sources from the metric tables (still via DuckDB projection)
- [x] Parity test pass vs DuckDB on `tael-test` (spans: filtered query_traces parity)

### B4: Search + retention
- [x] Tantivy full-text on LLM payloads (prompt/completion → trace_id); log-body indexing pending
- [x] Per-signal, per-tier retention/GC: hot rolloff ✅, metadata partition drop (all signals) ✅,
      payload blob GC ✅, metric 5m downsampling ✅
- [ ] HNSW semantic index (feature-gated, off by default — needs embeddings)

### B5: Scale path (v2)
- [◐] Cold tier relocatable via `TAEL_COLD_DIR` (object-store FUSE mount today); native S3/R2 async `object_store` is v2
- [ ] Optional Kafka/Redpanda ingest buffer for bursty traffic (demand-driven)
- [ ] Evaluate separating ingest/query processes
- [ ] DuckDB→tael-backend migration tool (prerequisite for flipping the default)

## Open Questions

1. **Hot-tier engine: `fjall` vs RocksDB.** `fjall` is pure-Rust (no C++ build,
   keeps the single-binary/cross-compile story clean); RocksDB is more proven
   under heavy write load. Lean `fjall` for v1; benchmark before committing.
2. **DataFusion read-latency validation.** Re-confirm at B3 that DataFusion +
   Parquet span-tree queries meet our latency targets. If a hot-tier query path
   needs more than DataFusion gives, the fallback is hand-rolled span-tree reads
   straight from the LSM (the `Store` trait keeps that swappable) — we stay
   all-Rust and embedded either way.
3. **Embedding source for HNSW.** Bring-your-own embedding endpoint, or a small
   local model? Probably BYO + off-by-default for v1.
4. **Tenant key in single-node.** v1 is effectively single-tenant; do we
   hard-code a `default` tenant in the partition path now (cheap forward-compat)
   or add it at v2? Lean toward including it now since it's just a path segment.
5. **WAL vs LSM-internal WAL.** RocksDB/fjall have their own WAL. Do we need a
   *separate* walrus-rust WAL in front, or rely on the LSM's? Separate WAL gives
   us a backend-independent durability boundary (survives a hot-tier engine
   swap); decide at B2.
6. **Metric high-cardinality guard.** A label like `request_id` on a metric
   explodes the series count. Do we (a) cap series per tenant and drop/alert past
   it, (b) auto-detect and strip high-cardinality labels, or (c) just document
   it? Lean (a) — a configurable series cap with a surfaced warning — for v1.
7. **Log body blob threshold.** What size triggers blobbing a log body (8 KiB?
   16 KiB?), and do we keep body-blob metadata (so we can show "body GC'd")
   longer than the blob itself? Tune against real log corpora at B4.
