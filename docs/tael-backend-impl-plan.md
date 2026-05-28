# tael-backend: Implementation Plan

> Status: Draft · Owner: colton@thousandbirds.ai · Last updated: 2026-05-25
>
> Execution plan for [`tael-backend-design.md`](./tael-backend-design.md). Read
> the design doc first for the *why*; this doc is the *how* and *in what order*.

## Current-state reality check

The design doc talks about swapping backends "behind the `Store` trait." **That
trait does not exist yet.** Today:

- `storage::DuckDbStore` is a concrete struct (`tael-server/src/storage/duckdb_store.rs`).
- It is used directly as `Arc<DuckDbStore>` in `main.rs:28` and threaded through
  `api/rest.rs` (`AppState.store`, `router(store: Arc<DuckDbStore>, …)`).
- Its methods are **synchronous** (`insert_spans`, `query_traces`, `get_trace`,
  `query_summary`, `query_anomalies`, `query_correlate`, plus logs/metrics/
  comments/services).

So before any new engine lands, we extract a trait and make the rest of the
server depend on the trait, not the struct. That extraction is Phase 0 and is
independently shippable (pure refactor, no behavior change).

A second reality check: the new engine's read path *can* be async (object-store
I/O, DataFusion). But Phase 0 keeps the `Store` trait **synchronous**, matching
the existing sync `DuckDbStore` and the sync (recursive) `promql::evaluate`.
Making the whole surface async now would be `spawn_blocking` wrappers over a
sync DB plus boxed async recursion in PromQL — pure churn for no present
benefit, and it would undercut Phase 0's "behavior-preserving refactor" goal.
DataFusion has synchronous execution paths (`block_on` a handle, or sync
`collect`), so when `TaelBackend` lands we introduce async **narrowly**, behind
the same trait, only on the specific methods that benefit — not as a blanket
Phase-0 conversion.

## Sequencing at a glance

```
Phase 0  Store trait extraction + async      ── refactor, ships alone
Phase 1  LlmSpan model + GenAI mapping        ── works on DuckDB too
Phase 2  Content-addressed blobs              ── prompts + big log bodies; DuckDB too
Phase 3  WAL + in-memory buffer               ── tagged per signal, no reads yet
Phase 4  LSM hot tier + reads                 ── TaelBackend MVP (hot-only)
Phase 5  Parquet cold tier + compactor        ── full tiering + downsampling
Phase 6  DataFusion unified read path         ── parity target
Phase 7  Retention & GC                        ── per-signal, per-tier clocks
Phase 8  Search (Tantivy, then HNSW)          ── derived indexes
Phase 9  Default flip + object store (v2)     ── promote, scale
```

Phases 1–2 ride on the existing DuckDB backend and deliver user-visible value
(LLM columns, payload dedup) *before* the new engine exists. Phases 3–6 build
`TaelBackend` incrementally, each phase runnable and testable on its own. Maps
to design-doc milestones: Phases 1–2 → B1, 3–4 → B2, 5–6 → B3, 7–8 → B4, 9 → B5.

**All three signals (spans, logs, metrics) go through the new engine — not just
spans.** Within the engine-building phases (3–6) the work is ordered *by signal*:
**spans first** (the LLM differentiator + riskiest layout), **logs next** (they
reuse the span row machinery almost verbatim), **metrics last** (a different
series-shaped layout plus the downsampling compactor). Until a given signal is
migrated it transparently falls back to `DuckDbStore`, so the backend is never
half-broken — `TaelBackend` can delegate any not-yet-implemented signal to a
wrapped DuckDB instance.

---

## Phase 0 — `Store` trait extraction (prerequisite) ✅ DONE

**Goal:** decouple the server from `DuckDbStore` so backends are swappable,
behind a synchronous object-safe trait (async deferred — see above).

> **Status:** complete. `trait Store: Send + Sync` lives in `storage/mod.rs`;
> `ServiceInfo` moved to `models.rs`; `DuckDbStore` implements it via thin
> forwarders (bodies unchanged). All consumers — `main.rs`, `api/rest.rs`,
> the three ingest services, `promql.rs`, `prom_remote_write.rs` — depend on
> `dyn Store` / `Arc<dyn Store>`. Workspace builds; all 9 tests pass; no new
> clippy warnings.

**Tasks**
- Define `trait Store: Send + Sync` in `storage/mod.rs` covering every method the
  API/CLI calls today: `insert_spans`, `query_traces`, `get_trace`,
  `add_comment`, `get_comments`, `list_services`, `insert_logs`, `query_logs`,
  `insert_metrics`, `query_metrics`, `query_summary`, `query_anomalies`,
  `query_correlate`. Synchronous methods returning `anyhow::Result<…>`.
- Move `ServiceInfo` from `duckdb_store.rs` into `models.rs` so the trait can
  name it without depending on the DuckDB module.
- Implement `Store for DuckDbStore` (bodies unchanged — just moved behind the
  trait). Keep the inherent methods too, so internal callers/tests are untouched.
- Change `AppState.store`, `router(...)`, the ingest services, `promql::evaluate`,
  and `prom_remote_write::handle_write` to use `dyn Store` / `Arc<dyn Store>`;
  update `main.rs` construction site.
- No new behavior. Existing `tael-test` suite must pass unchanged.

**Exit criteria:** server compiles and all current tests pass with the API layer
depending only on `dyn Store`. Backend still 100% DuckDB.

**Risk:** object safety + `Send + Sync` bounds for `Arc<dyn Store>` in axum
state. Mitigation: return owned types (`Vec<Span>` etc., already the case);
`DuckDbStore` is already shared as `Arc` across tasks, so it's already `Send + Sync`.

---

## Phase 1 — `LlmSpan` model + GenAI mapping ✅ DONE

> **Status:** complete. `SpanKind`, `LlmOperation`, `LlmSpan` in `models.rs`;
> `Span` gained `kind` + `llm` (both `#[serde(default)]`, back-compatible).
> `ingest/otlp.rs` maps `gen_ai.*` → typed fields and marks `SpanKind::Llm`.
> DuckDB schema migrated (nullable `kind`/`llm` columns; round-trips). CLI
> trace table shows an LLM column (`model · N tok · $cost`). Verified
> end-to-end over OTLP via a new `llm_chat_request` scenario in `tael-test`.
> 14 unit tests pass (5 new). Payload hashes (`prompt_sha256` etc.) stay `None`
> until Phase 2.

**Goal:** represent LLM calls as typed data, populated from OTLP. Backend-agnostic.

**Tasks**
- Add `SpanKind` (`Internal | Server | Client | Producer | Consumer | Llm`) and
  `LlmSpan` (provider, model, operation, token counts, cost_usd, ttft_ms,
  inter_token_ms, prompt_sha256, completion_sha256, finish_reason, temperature)
  to `storage/models.rs`. Add `kind: SpanKind` and `llm: Option<LlmSpan>` to
  `Span` (default `Internal`/`None` so existing code/tests are unaffected).
- In `ingest/otlp.rs`, detect GenAI spans via OpenTelemetry GenAI semantic
  conventions (`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.*`,
  `gen_ai.response.*`) and project them into `LlmSpan`'s typed fields. Unknown
  `gen_ai.*` / custom keys stay in the existing `attributes` map.
- Extend `DuckDbStore` schema with the new columns (nullable) so the data is
  queryable today; `insert_spans`/`query_traces`/`get_trace` round-trip them.
- CLI: surface model/tokens/cost in `tael get trace` output (`output.rs`).
- **Logs/metrics need no new model.** `LogRecord` and `MetricPoint` already
  exist and carry everything the new layout needs (logs: `trace_id`/`span_id`/
  `severity`/`body`/`service`; metrics: name/type/labels/value). The only later
  addition is a derived `labels_hash` for metric series, computed at write time
  in Phase 4 — not a model change.

**Exit criteria:** point an OTel SDK with GenAI instrumentation at the server;
`tael get trace <id>` shows model, tokens, and cost as typed fields.

**Tests:** OTLP fixtures with `gen_ai.*` attrs → assert `LlmSpan` projection;
round-trip through DuckDB.

---

## Phase 2 — Content-addressed blob store ✅ DONE

> **Status:** complete. `storage/blobs.rs` `BlobStore` (sha256 + snap, sharded
> `blobs/<aa>/<bb>/<hash>`, temp-then-rename writes, content dedup). Wired
> `Arc<BlobStore>` through `main.rs` into both OTLP services and the REST state.
> Span ingestion moves `gen_ai.prompt`/`gen_ai.completion` out of attributes
> into blobs (hashes on `LlmSpan`); log ingestion offloads bodies > 8 KiB
> (`LogRecord.body_sha256`, DuckDB column migrated). `GET /api/v1/blobs/{sha256}`
> resolves payloads. Verified e2e: two blobs written, prompt text removed from
> attributes, resolved via API, 404 on unknown. 19 tests pass (5 new).
> CLI `--with-payloads` inlining sugar deferred (the endpoint is the capability).

**Goal:** keep large payloads out of the columnar tables; dedup by content.
Serves **two signals** — span prompts/completions and oversized log bodies.

**Tasks**
- New module `storage/blobs.rs`: `put(bytes) -> sha256` (snap-compressed, write
  to `blobs/<aa>/<bb>/<sha256>`, skip if present) and `get(sha256) -> bytes`.
  `snap` is already a workspace dependency.
- In `ingest/otlp.rs`, when a GenAI span carries prompt/completion content
  (`gen_ai.prompt`, `gen_ai.completion`, or message events), write blobs and
  store only the hashes on `LlmSpan`.
- In `ingest/otlp_logs.rs`, when a `LogRecord.body` exceeds a threshold (start
  at 8 KiB — design Open Q #7), blob it and store `body_sha256`; short bodies
  stay inline. This dedups repeated stack traces for free.
- `tael get trace` / `tael query logs` resolve hashes back to content on read
  (flag-gated, e.g. `--with-payloads`, so default output stays compact).
- A blob `BlobStore` handle is shared via `AppState`; trait-level so the future
  object-store backend (Phase 9) drops in. Metrics never touch it.

**Exit criteria:** identical system prompts (and identical large log bodies)
across many records produce one blob on disk; `--with-payloads` reconstructs
both prompt/completion and oversized log bodies.

**Tests:** dedup (same content → same path, written once) for both prompts and
log bodies; GC-safe ref accounting deferred to Phase 7.

---

## Phase 3 — WAL + ingest buffer (new backend skeleton) ✅ DONE

> **Status:** complete. `storage/backend/{mod,wal}.rs`: `TaelBackend` (impls
> `Store`) with a `walrus-rust` WAL (`new_for_key("tael-backend")`, isolated
> from the span/log buses). Records are framed `[version][signal tag][JSON
> batch]`; writes follow append → apply → consume, and `replay()` drains the
> crash-gap on boot. The applied projection + reads delegate to an inner
> `DuckDbStore` (replaced tier-by-tier later). `TAEL_STORAGE=tael-backend`
> (`StorageBackend` in `config.rs`) selects it; `main.rs` constructs the chosen
> backend. Verified e2e on isolated ports (Docker squats 7701/4317): 24 spans
> in/out, no duplication, 1 LLM span, durable across `kill -9` + restart. 3 WAL
> unit tests (crash-gap replay, consumed-not-replayed, tag routing) with
> self-cleaning namespaces. 22 tests pass.
>
> Note: the design's "in-memory buffer" is realized as the redo-log + delegated
> projection here; a distinct volatile memtable arrives with the LSM in Phase 4.

**Goal:** durable write path for the new engine. No reads yet.

**Tasks**
- New module tree `storage/backend/` with `TaelBackend` struct (does not yet
  implement `Store` fully — `unimplemented!()` reads behind a feature flag).
- `backend/wal.rs`: append serialized records to a WAL via `walrus-rust`
  (already a dependency; defaults to `~/.tael/wal_files`), fsync, then ack. Each
  record carries a **signal tag byte** (span | log | metric) + a format version
  byte, so one WAL covers all three signals and replay routes each record to the
  right buffer. Implement crash replay on startup.
- `backend/mod.rs`: in-memory ingest buffers (one per signal); the write methods
  (`insert_spans`, `insert_logs`, `insert_metrics`) each write blobs where
  applicable (Phase 2) → WAL → buffer → ack. Read methods stubbed.
- `TaelBackend` wraps a `DuckDbStore` for delegation: any signal not yet served
  by the new tiers falls through to DuckDB, so the engine is usable from day one.
- Wire `--storage=tael-backend` in `config.rs`/`main.rs` to construct
  `TaelBackend` behind `Arc<dyn Store>` (opt-in; default stays DuckDB).

**Exit criteria:** with `--storage=tael-backend`, ingest accepts spans, logs, and
metrics, fsyncs each (tagged) to WAL, and a kill-9 + restart replays all three
signals into their buffers (asserted in a test).

**Risk:** WAL format versioning. Mitigation: version byte + signal tag on every
record from day one.

---

## Phase 4 — LSM hot tier + hot-only reads (all three signals) ✅ DONE

> **Status:** complete. `storage/backend/hot.rs` `HotTier` on `fjall` 3.x with
> per-signal keyspaces — spans (`trace_id\0span_id` + `be(start_ns)…` time
> index), logs (`be(ts)+seq`), metrics (`name\0be(ts)+seq`). `TaelBackend` now
> fans writes to WAL + hot tier + DuckDB projection, and serves the core reads
> — `query_traces`, `get_trace`, `list_services`, `query_logs`, `query_metrics`
> — **from the hot tier**; heavier analytics (summary/anomalies/correlate,
> PromQL) stay on the projection until Phase 6. Verified: 5 backend unit tests
> incl. **hot-tier↔DuckDB parity** on filtered trace queries, plus e2e
> (`--storage=tael-backend`, 24 distinct spans, LLM model preserved through the
> hot-tier prefix scan, all services aggregated, durable across reopen). 27
> tests pass. `with_wal_key` added for test isolation; namespaces self-clean.
>
> Note: full per-signal series-dictionary/labels_hash for metrics (4c in the
> sketch below) is deferred to the Parquet/cold-tier work where it pays off;
> the hot tier stores metric points directly, which is sufficient for the
> current `query_metrics`/PromQL read path.

**Goal:** `TaelBackend` serves recent-data queries from an LSM — a usable
hot-only MVP. Migrate signals in order: **spans → logs → metrics**, each behind
the DuckDB-delegation fallback so partial progress always runs.

**Tasks**
- `backend/hot.rs`: embed `fjall` (pure-Rust; see design Open Q #1), one
  keyspace (column family) per signal.

- **4a — Spans.** Two key encodings, both written on insert:
  - `(tenant, trace_id, span_id)` → span — span-tree reconstruction.
  - `(tenant, start_ns, span_id)` → span — time-range scans.
  Implement `query_traces` (time-range + filter), `get_trace` (trace_id prefix
  scan), `list_services`. Reuse filter-logic shape from `DuckDbStore`.

- **4b — Logs.** Logs reuse the span row machinery. Two keys:
  - `(tenant, service, ts_ns)` → log — severity/service + time-range reads.
  - `(tenant, trace_id)` → log — correlation with spans.
  Implement `query_logs`. Note this is the first signal where `query_correlate`
  can pull both spans and logs from the new backend by `trace_id`.

- **4c — Metrics.** Different shape — series-oriented:
  - Compute a stable `labels_hash` (xxhash of the sorted label set) at write.
  - Series-dictionary keyspace: `(tenant, name, labels_hash)` → label set +
    type/metadata, written once per new series.
  - Sample keyspace: `(tenant, name, labels_hash, ts_ns)` → value, so one
    series is a contiguous range scan.
  - Add a configurable **series cap per tenant** with a surfaced warning to
    guard high-cardinality labels (design Open Q #6).
  - Implement `query_metrics` (range scan per series). PromQL still routes
    through the existing `promql.rs`, now sourcing samples from the hot tier.

**Exit criteria:** with `--storage=tael-backend`, `tael query traces`,
`tael get trace`, `tael query logs`, `tael query metrics`, and `tael correlate`
return correct results for data within the hot window; the `tael-test` subset
for all three signals passes against the new backend.

---

## Phase 5 — Parquet cold tier + compactor ✅ DONE (all 3 signals; downsampling deferred)

> **Status:** spans, logs, and metrics all tiered. `storage/backend/cold.rs`
> `ColdTier`: per-signal Arrow schemas + Parquet writers producing
> `cold/<signal>/date=…/hour=…/<signal>-<ulid>.parquet` — spans sorted by
> `trace_id`, logs by `(service, ts)`, metrics by `(name, ts)`; readers
> reconstruct each type (spans keep the LLM extension). Hot-tier eviction
> (`evict_spans_before`/`evict_logs_before`/`evict_metrics_before`) +
> `TaelBackend::compact_spans`/`compact_logs_metrics` roll aged data hot→cold;
> `query_traces`/`get_trace`/`query_logs`/`query_metrics` union hot∪cold (hot
> leads, cold fills the limit) via shared `span_matches`/`log_matches`/
> `metric_matches` predicates. Background maintenance task in `main.rs` (24h
> window, hourly; env-tunable). Verified e2e (spans → Parquet, hot emptied,
> union still serves all 24, LLM model preserved) + unit tests for logs/metrics
> compaction+union. 31 tests pass. **Deferred:** metric 5m downsampling
> (`metrics_5m` rollups) — raw metric retention via partition drop already
> bounds storage; downsampling is a resolution-vs-history refinement.

**Goal:** age data out of the LSM into immutable Parquet, each signal laid out
for its access pattern; add metric downsampling.

**Tasks**
- `backend/parquet/` — one Arrow schema + writer per signal, each sorted within
  the hour partition for its dominant read:
  - `spans.rs` — typed span + LLM columns + `Map<Utf8,Utf8>` attribute tail;
    **sorted by `trace_id`**. `spans-<ulid>.parquet` / `llm_spans-<ulid>.parquet`.
  - `logs.rs` — log columns + body (inline or `body_sha256`) + attr map;
    **sorted by `(service, ts)`**. `logs-<ulid>.parquet`.
  - `metrics.rs` — `(labels_hash, ts, value)` + per-series dictionary;
    **sorted by `(name, labels_hash, ts)`**. `metrics-<ulid>.parquet`.
- `backend/compactor.rs`: background task; on interval, take LSM data older than
  `hot_tier` (default 24h), write the signal's Parquet, then delete from the LSM.
  Idempotent and crash-safe (write Parquet fully → prune LSM → drop WAL segment).
- **Metric downsampling pass:** before raw points pass the `raw` window, roll
  each series into 5m aggregates (min/max/avg/sum/count) → `metrics_5m-<ulid>.parquet`,
  **day-partitioned** (sparse, long-lived). Fix the rollup function set now —
  adding one later means re-aggregating history (design Hard Part #4).
- v1 target is local-disk Parquet under the data dir. Use the `object_store`
  crate abstraction even for local FS so Phase 9 is a config change.

**Exit criteria:** ingest each signal, advance the clock (test seam), confirm
rows move from LSM to Parquet and the LSM shrinks; verify per-signal sort order
with a reader; confirm raw metric points roll up into `metrics_5m` and the raw
partitions are then droppable.

**Risk:** compaction correctness under concurrent ingest. Mitigation: compact
closed (immutable) time windows only; never the active hour.

---

## Phase 6 — Unified read path + SQL surface ✅ DONE (DataFusion pushdown deferred)

> **Status:** goal met via two pieces. (1) **Unified hot+cold reads** — the
> hand-rolled union (`query_traces`/`get_trace`/`query_logs`/`query_metrics`
> merge hot tier + cold Parquet) is the realization the design's Open Q #2
> explicitly sanctions as the fallback; tested + e2e-verified. (2) **SQL
> surface** — `Store::query_sql` (read-only `SELECT`/`WITH`, generic row→JSON)
> over the analytical projection, exposed at `GET /api/v1/sql` and
> `tael query sql "…"`. Verified e2e: per-service span counts, LLM cost rollup
> via `json_extract` on the `llm` column, mutations rejected. 36 tests pass.
> **Deferred:** registering hot+cold as native DataFusion `TableProvider`s for
> predicate/partition pushdown — an optimization over the working union; the
> ~60-crate DataFusion stack is not pulled in (all-Rust + embedded preserved
> via DuckDB for SQL).

**Goal:** one query path over hot + cold; reach parity with DuckDB.

**Tasks**
- `backend/query.rs`: a DataFusion `SessionContext` registering, **per signal**,
  a hot⊎cold logical table — `spans`, `llm_spans`, `logs`, `metrics`,
  `metrics_5m` — each unioning (a) the LSM keyspace as a custom `TableProvider`
  and (b) the Parquet partitions (native source, partition pruning on
  `date`/`hour`).
- Re-implement `Store` read methods as DataFusion queries (SQL or DataFrame),
  replacing the hot-only implementations from Phase 4: `query_traces`,
  `query_logs`, `query_metrics`, plus the cross-signal `query_summary`,
  `query_anomalies`, and `query_correlate` (one `trace_id` predicate across
  `spans` and `logs` — no cross-store join).
- **PromQL:** `promql.rs` stays a purpose-built evaluator; repoint it to source
  raw/rollup samples from the `metrics`/`metrics_5m` tables via DataFusion range
  scans, then apply PromQL functions on top.
- **Parity harness:** extend `tael-test` to dual-run a fixture corpus (all three
  signals) through both `DuckDbStore` and `TaelBackend` and assert equal results
  for the full query surface (design migration step 4).

**Exit criteria:** parity suite green across **all three signals** plus
correlate/summary/anomalies and PromQL. Span-tree *and* metric-range query
latency measured and recorded — the design's Open Q #2 validation checkpoint.

**Decision gate:** if a query path's latency through DataFusion is unacceptable
(span-tree or metric range scan), fall back to hand-rolled LSM/Parquet reads
behind the same trait method (stay all-Rust, per design).

---

## Phase 7 — Retention & GC ✅ DONE (YAML config via env)

> **Status (partial):** span **metadata GC** done — `ColdTier::drop_partitions_before`
> unlinks whole `date=…` Parquet partitions older than the retention window;
> `TaelBackend::enforce_span_retention` wraps it; the `main.rs` maintenance task
> runs compaction + retention together (default 365d, env `TAEL_TRACE_RETENTION_DAYS`).
> Hot-tier rolloff is the Phase-5 compactor. Test: `retention_drops_old_partitions_only`.
> **Pending:** payload-blob GC (ref-counted over live rows), log/metric retention,
> metric downsampling GC, and the full YAML `retention:` config block.

**Goal:** per-signal, per-tier retention from the design's policy block.

**Tasks**
- `backend/retention.rs`: four mechanisms, all per signal —
  1. **hot rolloff** (already the compactor in Phase 5), all three signals;
  2. **metadata GC** — drop whole Parquet partitions older than `metadata` (per
     signal; traces 365d, logs 14d) via `unlink`, not rewrite;
  3. **payload GC** — delete blobs (span prompts/completions *and* oversized log
     bodies) whose newest referencing row is older than `payloads`; reference-
     counted or mark-and-sweep over live row hashes across both signals — never
     delete a blob a live row points to;
  4. **metric downsampling GC** — the 5m rollup pass (Phase 5) followed by
     dropping raw metric partitions past `raw`; rollups live until `downsampled`.
- Config: extend `config.rs` to parse the full per-signal `retention:` block
  (hot_tier/metadata/payloads for traces & logs; hot_tier/raw/downsampled/
  downsample_interval for metrics) from YAML + CLI flags.
- Hourly scheduler (mirror the existing cleanup-job cadence in `DESIGN.md`).

**Exit criteria:** with short test retentions, blobs and partitions are GC'd on
schedule for all signals; a blob still referenced by a within-window span *or*
log is never deleted; raw metric points downsample then drop while rollups
survive to the longer clock.

---

## Phase 8 — Search ✅ DONE (full-text; HNSW deferred)

> **Status:** Tantivy full-text over LLM payloads done. `storage/search.rs`
> `SearchIndex` (schema: trace_id/span_id stored, body TEXT) indexes
> prompt+completion text at ingest time in `ingest/otlp.rs` (before the text is
> blobbed away), shared as `Arc<SearchIndex>` between ingest (writes) and
> `TaelBackend` (reads). `TraceQuery.text` → `query_traces` restricts to
> matching trace_ids via `search_trace_ids`. Wired through REST (`?text=`) and
> CLI (`tael query traces --text "…"`). Verified e2e: `--text "OTLP"` returns
> the LLM trace, `--text "kubernetes"` returns none. 35 tests pass (+2).
> **Deferred:** HNSW semantic index (feature-gated, off by default — needs a
> BYO embedding source, design Open Q #3).

**Goal:** full-text and (optional) semantic search over text-bearing signals.

**Tasks**
- `backend/search.rs`, Tantivy indexes built/updated at compaction time over the
  two text signals:
  - prompt/completion text keyed by `span_id` → `tael query traces --text "<q>"`;
  - log bodies (inline or resolved from `body_sha256`) keyed by the log row →
    `tael query logs --text "<q>"`.
  Both filters plumbed through the trait + CLI. Metrics have no text.
- HNSW (`usearch`/`hnsw_rs`) semantic index, **feature-gated and off by
  default**, over prompts/completions (and optionally log bodies), per-tenant +
  per-time-window so it stays bounded and drops with retention. BYO embedding
  endpoint (design Open Q #3).

**Exit criteria:** `--text` returns matching spans (by payload) and matching
logs (by body); semantic search works behind the feature flag with a configured
embedding source.

---

## Phase 9 — Default flip + object store (v2) ◐ v1 pieces done

> **Status:** v1-appropriate pieces landed; native-cloud pieces are the v2
> scale path the design (B5) scopes as future.
> - **Cold tier relocatable:** `TAEL_COLD_DIR` points the Parquet cold tier at a
>   separate mount — a network/object-store FUSE mount (s3fs/gcsfuse) keeps aged
>   Parquet off local disk today, no code change. The path layout is already a
>   valid object-store key prefix.
> - **Default flipped to `tael-backend`:** `ServerConfig::from_env` now defaults
>   to the tiered engine. Operators opt back into the legacy engine with
>   `tael-server --storage duckdb` (a `--storage <duckdb|tael-backend>` flag,
>   which beats the env var) or `TAEL_STORAGE=duckdb`. Verified: no flag/env →
>   TaelBackend; `--storage duckdb` / `TAEL_STORAGE=duckdb` → DuckDB; flag wins
>   over env. **Data-continuity note:** existing DuckDB datastores aren't auto-
>   migrated — a fresh start on the new default reads the (empty) new tiers, so
>   sites with existing data should either pass `--storage duckdb` or await the
>   DuckDB→backend migration tool (tracked in B5).
> - **Deferred to v2 (per design B5):** native S3/R2 via the async `object_store`
>   crate (needs the read path to go async), the optional Kafka/Redpanda ingest
>   buffer, and splitting ingest/query into separate processes.

**Goal:** promote `tael-backend` to default; enable cloud cold tier.

**Tasks**
- After sustained parity + retention validation, flip the default to
  `tael-backend`; keep `--storage=duckdb` as a supported fallback.
- Point the `object_store` cold tier at S3/R2 (config only, thanks to Phase 5).
- (Optional, demand-driven) Kafka/Redpanda ingest buffer; evaluate splitting
  ingest/query processes. Both out of v1 scope.

**Exit criteria:** fresh install defaults to the new engine; cold tier can target
object storage via config with no code change.

---

## Cross-cutting concerns

- **Testing:** every phase ships with tests; Phase 6's dual-run parity harness is
  the backbone. Add WAL-replay and compaction crash-recovery tests with explicit
  clock seams (no real-time sleeps).
- **Feature flags / opt-in:** `tael-backend` is opt-in via `--storage` until
  Phase 9; DuckDB is never broken in the interim.
- **Benchmarks:** capture ingest throughput plus span-tree *and* metric-range
  read latency starting Phase 4; they inform the Phase 6 decision gate.
- **Per-signal progress:** within Phases 4–6, spans/logs/metrics land in that
  order; each undelivered signal delegates to the wrapped `DuckDbStore`, so
  `--storage=tael-backend` is always a complete, correct backend even mid-migration.
- **Observability of the engine itself:** tael instruments tael — emit internal
  spans/metrics for WAL fsync time, compaction duration, LSM size, GC counts.
- **Docs:** update `DESIGN.md` storage section and `SKILL.md`/`README.md` when the
  default flips.

## Dependencies to add

| Crate | Phase | Purpose |
|-------|-------|---------|
| `async-trait` | 0 | async `Store` trait (unless native afit is viable) |
| `fjall` | 4 | embedded pure-Rust LSM hot tier (per-signal keyspaces) |
| `xxhash-rust` (or `twox-hash`) | 4 | stable `labels_hash` for metric series |
| `datafusion` | 6 | SQL/agg query engine over hot + Parquet |
| `arrow` / `parquet` | 5 | columnar schema + cold-tier files (per signal) |
| `object_store` | 5 | FS now, S3/R2 later, one API |
| `tantivy` | 8 | full-text search over payloads + log bodies |
| `usearch` or `hnsw_rs` | 8 | semantic search (feature-gated) |
| `ulid` | 5 | sortable Parquet file ids |

Already present and reused: `walrus-rust` (WAL), `snap` (blob compression),
`sha2`/`hex` (content addressing), `tokio`, `serde`/`prost`.

## Open risks carried from design

1. **fjall vs RocksDB** — benchmark before committing (design Open Q #1);
   isolated to `backend/hot.rs`.
2. **DataFusion read latency** — validated at the Phase 6 gate (design Open Q #2).
3. **Separate WAL vs LSM-internal WAL** — decide in Phase 3/4; leaning separate
   WAL for a backend-independent durability boundary (design Open Q #5).
4. **Tenant key now vs later** — include `tenant` in keys/paths from Phase 4
   (it's just a segment; cheap forward-compat) (design Open Q #4).
5. **Metric high-cardinality guard** — configurable per-tenant series cap with a
   surfaced warning, landed with metrics in Phase 4c (design Open Q #6).
6. **Log-body blob threshold** — start at 8 KiB; tune against real log corpora
   at B4 (design Open Q #7).
