# Scaling & HA: tael-server on the tael-backend engine

> Status: Draft · Owner: colton@thousandbirds.ai · Last updated: 2026-05-25
>
> Companion to [`tael-backend-design.md`](./tael-backend-design.md) (the storage
> engine) and [`tael-backend-impl-plan.md`](./tael-backend-impl-plan.md). This
> doc covers how to run `tael-server` with the `tael-backend` engine across more
> than one node — how to scale ingest and query horizontally, and how to
> structure the deployment for high availability. It picks up where the design
> doc's **B5: Scale path (v2)** leaves off.

## TL;DR

`tael-server` today is a **single-node, embedded, single-writer** process: WAL +
LSM hot tier + DuckDB projection + Tantivy index + blob store all live behind one
process holding exclusive file locks on one data dir (`tael-server/src/main.rs`,
`storage/backend/mod.rs`). You cannot point two instances at the same data dir
and you cannot run two instances with the same WAL key — the WAL namespace is
process-global (`storage/backend/wal.rs`, `TaelBackend::with_wal_key`).

So horizontal scale is **not** "run N replicas of the same process." It is one of
two grains, in order of effort:

1. **Shard the telemetry stream** across N independent single-writer instances,
   each owning its own WAL/hot/cold, with a routing layer in front and a
   scatter-gather query layer on top. HA comes from per-shard replication.
2. **Disaggregate** into stateless ingest, single-writer-per-shard storage, and
   stateless query tiers that share a durable ingest log (Kafka/Redpanda) and an
   object-store cold tier. This is the target; it's what B5 gestures at.

The architecture is already shaped for both: the `Store` trait is a clean swap
boundary (`storage/mod.rs:26`), the cold tier is relocatable to object storage
(`TAEL_COLD_DIR`, `storage/backend/cold.rs:64`), and the blob store is
content-addressed and therefore trivially shareable (`storage/blobs.rs`). The
work is in the seams around those, not a rewrite.

---

## 1. Where we are today: the single-node topology

```
                    OTLP gRPC :4317        REST API :7701
                          │                     │
            ┌─────────────▼─────────────────────▼──────────────┐
            │                 tael-server (1 process)            │
            │                                                    │
            │   ingest receivers ──┐        ┌── REST/CLI reads   │
            │   (tonic, axum)      │        │                    │
            │                      ▼        ▼                    │
            │              Arc<dyn Store> = TaelBackend          │
            │   ┌──────────────────────────────────────────┐    │
            │   │ WAL (walrus)  process-global key           │    │
            │   │ Hot tier (fjall LSM)   <data_dir>/hot      │    │  exclusive
            │   │ DuckDB projection      <data_dir>/*.duckdb │    │  file locks
            │   │ Tantivy search index   <data_dir>/...      │    │  on one
            │   │ Blob store             <data_dir>/blobs    │    │  data_dir
            │   │ Cold tier (Parquet)    <data_dir>/cold     │    │
            │   └──────────────────────────────────────────┘    │
            │   background compactor task (singleton, in-proc)   │
            └────────────────────────────────────────────────────┘
```

Concurrency model, as built:

| Component | Concurrency | Source |
|---|---|---|
| OTLP gRPC + REST | async, many connections | `main.rs` two `tokio::spawn` listeners |
| `Store` access | `Arc<dyn Store>`, `Send + Sync`, **synchronous** methods | `storage/mod.rs:26` |
| WAL | one process per namespace key (process-global) | `wal.rs`, `mod.rs:51` |
| Hot tier (fjall) | exclusive DB lock per `<data_dir>/hot` | `hot.rs:43` |
| DuckDB projection | **single-writer** | design §"Why not just keep DuckDB" |
| Compactor / retention / blob GC | single in-process background task | `main.rs:spawn_span_compactor` |
| Blob store | content-addressed, idempotent `put`, mark-and-sweep `gc` | `blobs.rs` |

### The hard constraints (why you can't just add replicas)

These are the load-bearing facts. Every scaling decision below is downstream of
them.

1. **Embedded engines take exclusive file locks.** `fjall`
   (`Database::builder(path).open()`), the DuckDB projection, and the Tantivy
   index each lock their files. Two processes on one data dir corrupt or refuse
   to open. So a data dir has exactly one writer *and one reader process* at a
   time — there is no "open read-only from a second process" path today.
2. **The WAL key is process-global.** `Walrus::new_for_key(key)` namespaces a WAL
   globally within the process/host; the code comments this explicitly
   (`mod.rs:51`). Two instances with the same key on a host collide.
3. **The maintenance loop is a singleton.** `spawn_span_compactor` runs
   compaction, partition-drop retention, and blob GC in one task. If two
   processes ran it against shared cold/blob storage, they would race on
   `drop_partitions_before` and `blobs.gc` — GC computes "live hashes" from *its
   own* view of live rows (`collect_live_blob_hashes`), so a second writer's
   blobs look like orphans and get deleted. Exactly one compactor may own a given
   cold/blob namespace.
4. **Writes ack after a local fsync, not after replication.** The write path is
   WAL append (fsync) → apply to hot+projection → mark applied (`mod.rs:200`).
   Durability today is "survives this node's crash," not "survives this node's
   loss." HA has to add the second guarantee.
5. **Core reads are full scans of the node's own data.** `query_traces` /
   `query_logs` / `query_metrics` reverse-iterate the whole hot keyspace and, to
   fill a limit, pull `cold.all_spans()` and filter in memory (`mod.rs:222-332`,
   `cold.rs:all_spans`). Read latency therefore scales with **per-node** data
   volume — which is itself an argument for sharding rather than one fat node.

### What's already shaped for scale-out

Not everything fights us. These seams are deliberate (see design B5):

- **`Store` is the swap boundary.** Everything above it — REST, gRPC ingest, CLI,
  PromQL — depends only on `Arc<dyn Store>` (`storage/mod.rs`). A routing/fan-out
  `Store` implementation, or a remote-client `Store`, slots in without touching
  the API layer.
- **The cold tier is relocatable.** `TAEL_COLD_DIR` already redirects Parquet to a
  separate mount, and the `date=…/hour=…` path layout *is* a valid object-store
  key prefix (`cold.rs:8,64`). Native async S3/R2 via `object_store` is the v2
  follow-on, but a FUSE mount (s3fs/gcsfuse) makes cold storage shared *today*.
- **Blobs are content-addressed.** `sha256(content)` keys mean any node computes
  the same path for the same payload, `put` is idempotent, and dedup is free
  (`blobs.rs`). A shared object-store blob bucket needs no coordination on the
  write path — only GC needs a single owner.
- **Ingest receivers are stateless.** The tonic/axum receivers hold no state but
  the `Arc<dyn Store>`; they can be lifted into their own tier.

---

## 2. Strategy 0 — Vertical first (do this before sharding)

A single node goes a long way and skips all the distributed-systems cost. Before
scaling out, exhaust:

- **Bigger box + fast local NVMe** for `<data_dir>/hot` and the DuckDB projection
  (the latency-sensitive, fsync-heavy paths).
- **Offload cold to a separate/cheaper mount** via `TAEL_COLD_DIR` so the hot
  NVMe isn't competing with aged Parquet.
- **Tune the hot-tier window down.** `TAEL_HOT_TIER_HOURS` (default 24) bounds how
  much data the full-scan reads (`hot.rs:query_*`) and the in-memory cold filter
  traverse. A smaller hot window = smaller scans = lower read latency and RAM, at
  the cost of pushing more reads into the cold path. Pair with
  `TAEL_COMPACT_INTERVAL_SECS`.
- **Keep payloads out of the row scans** — already true (blobbed), but verify
  oversized log bodies are actually blobbing.

Vertical scaling's ceiling is the single-writer DuckDB projection under bursty
ingest (the original motivation for tael-backend) and the O(per-node-volume) read
scans. When you hit either, shard.

---

## 3. Strategy 1 — Shard the stream (near-term horizontal + HA)

The realistic first horizontal step that needs **no engine rewrite**: run N
independent `tael-server` instances, each a complete single-writer backend over
its **own** data dir and WAL key, and partition the telemetry stream across them.

```
                       OTLP / remote-write producers
                                   │
                    ┌──────────────▼───────────────┐
                    │   Routing layer                │  hash(trace_id) → shard
                    │   (OTel Collector              │  keeps a whole trace on
                    │    routingconnector, or LB     │  one shard so get_trace /
                    │    w/ consistent hashing)      │  correlate stay local
                    └───┬─────────────┬─────────────┘
                        │             │
              ┌─────────▼───┐   ┌─────▼───────┐        (N shards)
              │ shard 0      │   │ shard 1      │   ...
              │ tael-server  │   │ tael-server  │
              │ own WAL/hot/ │   │ own WAL/hot/ │
              │ cold/blobs   │   │ cold/blobs   │
              └─────────┬────┘   └────┬─────────┘
                        │             │
                    ┌───▼─────────────▼───┐
                    │  Query fan-out layer  │  scatter to all shards,
                    │  (scatter-gather)     │  gather + merge + re-limit
                    └───────────────────────┘
```

### Choosing the shard key

**Shard by `trace_id`** (hash → shard). This is the only key that keeps the two
locality-sensitive operations correct without cross-shard joins:

- `get_trace` is a `trace_id` prefix scan (`hot.rs:get_trace`) — all spans of a
  trace must land on one shard.
- `tael correlate <trace_id>` joins spans and logs by `trace_id`
  (`query_correlate`) — same requirement.

A trace's spans arrive from multiple services/processes, so the **routing layer
must hash on `trace_id`**, not on source. The OTel Collector's `routingconnector`
or a span-aware load balancer (`loadbalancingexporter` with `routing_key:
traceID`) does exactly this and is the standard pattern. Logs carry `trace_id`
too and route the same way; metrics (no trace) shard by `(name, labels_hash)` or
`service`.

Tenant, when it lands (design Open Q #4), becomes the natural top-level shard key
— `hash(tenant, trace_id)` — and gives clean per-tenant isolation.

### The query fan-out layer

Reads scatter to all shards and merge. Most `Store` methods compose cleanly under
fan-out because they already return "newest-first, then re-limit":

- `query_traces` / `query_logs` / `query_metrics`: query each shard with the same
  limit, concatenate, re-sort by time desc, truncate to `limit`. The per-shard
  ordering contract (`mod.rs:222-332`) makes this a k-way merge.
- `get_trace` / `query_correlate`: route to the single owning shard (hash the
  `trace_id`) — no fan-out needed.
- `list_services`, `query_summary`, `query_anomalies`: fan out and **aggregate**
  (sum counts, recompute error_rate / avg from component sums — `list_services`
  already exposes count+total shapes that re-aggregate, see
  `hot.rs:list_services`). This is the part that needs real merge code, because
  averages and rates don't concatenate.
- `query_sql`: hardest — arbitrary SQL over the DuckDB projection doesn't
  distribute. Options: (a) restrict the fan-out SQL surface to pushdown-able
  shapes, (b) run it per-shard and union rows (correct for filters/projections,
  wrong for cross-shard GROUP BY/aggregates), or (c) leave `query_sql` as a
  single-node power-tool and document it as non-distributed. Recommend (c) for
  the first shard release.

Implement the fan-out as a `Store` impl (`FanoutStore`) that holds N remote-client
`Store`s — the trait boundary means the REST/gRPC/CLI layers don't change. This
also needs a **remote `Store` client** (a thin HTTP/gRPC client implementing
`Store` against another tael-server's REST API), which the project does not have
yet — it's the main net-new component for this strategy.

### HA within a shard

Sharding alone is scale, not availability — losing one shard loses 1/N of the
data. Add per-shard redundancy:

- **Replicate the WAL** to a standby instance of the same shard. Because writes
  ack after WAL fsync (`mod.rs:200`), a standby that tails the leader's WAL can
  rebuild identical hot+cold state via the existing replay path
  (`TaelBackend::replay`, `wal.rs:drain`). On leader loss, promote the standby.
  This needs WAL shipping (today the WAL is a local walrus namespace — see §5.1).
- **Or share the cold tier + blobs** across the shard's instances on object
  storage, so only the hot tier (last `TAEL_HOT_TIER_HOURS`) must be rebuilt from
  WAL on failover. This is the cheaper recovery and the bridge to Strategy 2.

---

## 4. Strategy 2 — Disaggregate (the target architecture)

The sharded model still couples ingest, storage, compaction, and query inside one
process per shard. The target — aligned with B5's "separate ingest/query
processes" and "object-store cold tier" — splits them so each tier scales on its
own axis and a node loss is never data loss.

```
        producers
            │
   ┌────────▼─────────┐     stateless, autoscale on ingest QPS
   │  Ingest tier      │     decode OTLP/remote-write → normalize → enrich
   │  (N replicas)     │     → append to durable log (no local state)
   └────────┬─────────┘
            │ partitioned by hash(tenant, trace_id)
   ┌────────▼───────────────────────────────────────┐
   │  Durable ingest log  (Kafka / Redpanda)          │  replicated, retained
   │  partitions = shards                              │  this IS the WAL now
   └────────┬───────────────────────────────────────┘
            │ one consumer-owner per partition (single writer per shard)
   ┌────────▼─────────┐     stateful but recoverable: hot tier is a
   │  Storage/compact  │     materialized view of the log; rebuildable
   │  tier (1 owner    │     by replaying the partition. Compactor lives
   │  per partition)   │     here, one per partition → no GC race.
   └────────┬─────────┘
            │ writes Parquet + blobs
   ┌────────▼─────────────────────────────┐
   │  Object store (S3/R2): cold + blobs    │  durable, shared, the system
   │  date=…/hour=… Parquet; sha256 blobs   │  of record for aged data
   └────────┬─────────────────────────────┘
            │ read-only
   ┌────────▼─────────┐     stateless, autoscale on query QPS; reads cold
   │  Query tier       │     from object store, hot from the owning storage
   │  (N replicas)     │     node (or its read replica). Scatter-gather.
   └───────────────────┘
```

Why this maps onto the existing engine cleanly:

- **The durable ingest log replaces the local WAL as the durability boundary.**
  The design already contemplates "Optional Kafka/Redpanda ingest buffer" (B5).
  Once the log is the source of truth, a storage node's local state is a
  *rebuildable cache* — node loss replays the partition from the log's retained
  offset. This is the single biggest HA win and it removes the
  ack-before-replication gap (constraint #4).
- **Single-writer-per-partition is preserved, not fought.** Each Kafka partition
  has exactly one consumer-owner = exactly one fjall/DuckDB writer = the engine's
  invariant holds, while the *fleet* scales by adding partitions.
- **The cold tier and blobs become the shared system of record** on object
  storage. The path layout is already object-store-shaped (`cold.rs:8`) and blobs
  are already content-addressed (`blobs.rs`). The remaining code work is the
  async `object_store` read path the design flags as v2 (the sync local-FS scan in
  `cold.rs:for_each_row` must go async).
- **The query tier is stateless and read-only**, so it autoscales freely and a
  query node can die mid-request with no data impact. Hot reads come from the
  owning storage node (a small remote read of the last N hours); cold reads come
  straight from object storage and are cacheable.

The one structural change the engine needs for true read scale-out is breaking
the "one process per data dir, no second reader" constraint (#1) for the **cold**
path — which object storage gives for free (many readers, no lock) — and routing
**hot** reads to the owning node. The DuckDB projection (currently doing
analytics) is the awkward piece: see §5.5.

---

## 5. HA building blocks (cross-cutting)

These apply to both strategies; they're the checklist for "structure it for HA."

### 5.1 Durability: from local WAL to replicated log

Today durability = local fsync (`mod.rs:200`), which survives a crash but not a
node loss, and the WAL is a process-local walrus namespace (`wal.rs`). For HA:

- **Near term (sharded):** ship the WAL to a standby (WAL streaming) or put the
  WAL on a replicated block device (e.g. EBS multi-attach is *not* safe with the
  exclusive-lock engines — use storage-level replication + failover, not shared
  mount). Promote standby on failure; it replays via `TaelBackend::replay`.
- **Target (disaggregated):** the Kafka/Redpanda partition *is* the replicated
  WAL. Local state is a view; recovery = replay from last checkpointed offset.
  Checkpoint the offset alongside the hot-tier flush so replay is bounded.

### 5.2 The singleton compactor / GC owner

`spawn_span_compactor` must run **exactly once per cold+blob namespace** (constraint
#3). Concretely:

- Sharded model: each shard owns its own cold+blobs, so its own in-process
  compactor is automatically the sole owner — fine as-is.
- Disaggregated/shared-object-store model: compaction and `blobs.gc` move to the
  per-partition storage owner, gated by **leader election** (the partition
  consumer-owner is the leader). Never run blob GC from two processes against one
  bucket — `collect_live_blob_hashes` only sees one node's live rows and will
  delete another's blobs. If GC ever spans multiple writers' blobs, it must
  compute the live set as the **union across all owners** (or switch to refcounts).

### 5.3 Object storage for cold + blobs

The durable, shared, replicated system of record. Action items:

- Land the native async `object_store` cold backend (design B5 / Phase 9). The
  blocker is the sync read path (`cold.rs:for_each_row`, `all_spans`); it must go
  async and ideally gain predicate/partition pushdown (the design's DataFusion
  Phase 6) so cold reads don't pull whole partitions into memory.
- Point blobs at the same bucket. `put` is already idempotent and safe under
  concurrent writers (temp-file-then-rename, `blobs.rs:50`); on S3 use
  put-if-absent semantics or just tolerate idempotent overwrites.
- Rely on the object store's own multi-AZ replication for cold durability.

### 5.4 Load balancing, health, and graceful lifecycle

- **Ingest** (stateless, or sharded by trace_id) sits behind a span-aware LB
  (`loadbalancingexporter`/`routingconnector`) so a whole trace lands on one
  shard. For the stateless ingest tier, any L4/L7 LB works.
- **Query** (stateless) sits behind a normal LB with health checks.
- **Health endpoints:** add readiness (WAL open, hot tier mounted, cold reachable)
  and liveness probes to the REST router (`api/rest.rs`) — not present today.
- **Graceful shutdown:** on SIGTERM, stop accepting new OTLP, drain in-flight
  writes through `mark_applied`, flush the hot tier (`db.persist`), and release
  locks before exit so a standby can take over cleanly. The current `main.rs`
  `tokio::select!` has no drain path — add one.

### 5.5 The DuckDB projection problem

`TaelBackend` still double-writes to an inner DuckDB projection that backs
`query_summary` / `query_anomalies` / `query_correlate` / `query_sql` / PromQL
(`mod.rs:39,334-365`). It is single-writer and node-local, so under
disaggregation it does **not** distribute:

- Short term: fan-out aggregation in the query tier handles summary/anomalies
  (recompute from per-shard partials); leave `query_sql` non-distributed (§3).
- Long term: this is exactly what the design's **DataFusion unification (Phase 6)**
  removes — analytics run over the hot⊎cold tables directly, no DuckDB
  projection, so the query tier reads object-store Parquet and per-node hot tiers
  through one engine. Retiring the projection is a prerequisite for clean read
  scale-out.

### 5.6 Backpressure & flow control

Writes are synchronous through fsync (`insert_spans`). Under a burst, the ingest
path must apply backpressure (OTLP gRPC can signal `RESOURCE_EXHAUSTED` /
remote-write 429) rather than unboundedly buffer. The durable log in Strategy 2
absorbs bursts natively (the design's stated motivation for the optional
Kafka/Redpanda buffer); in the sharded model, bound the receive queue and shed
with retryable errors.

---

## 6. Phased rollout (extends design B5)

1. **Vertical + ops hardening** (no new topology): health/readiness probes,
   graceful drain, `TAEL_COLD_DIR` on shared/object-backed mount, tune
   `TAEL_HOT_TIER_HOURS`. Single node, but operable and recoverable.
2. **Remote `Store` client + `FanoutStore`**: the scatter-gather query layer and
   an HTTP/gRPC `Store` client. Unlocks read fan-out without changing the API.
3. **Sharded writes**: OTel Collector routing on `trace_id`; N independent
   instances. Per-shard WAL shipping for HA. This is the first true horizontal
   step.
4. **Async object-store cold + blobs** (design B5/Phase 9): shared system of
   record; failover only rebuilds the hot window.
5. **Durable ingest log** (Kafka/Redpanda, B5): the log becomes the WAL; storage
   nodes become rebuildable views; ack-after-replication.
6. **DataFusion unification** (design Phase 6): retire the DuckDB projection so the
   query tier is fully stateless and reads scale independently.
7. **Disaggregated tiers**: stateless ingest + query autoscale; one storage owner
   per partition; leader-elected compaction/GC.

## 7. Failure modes (target/disaggregated)

| Failure | Blast radius | Recovery |
|---|---|---|
| Query node dies | none (stateless) | LB removes it; retry elsewhere |
| Ingest node dies | none (writes are in the durable log) | LB removes it; producers retry |
| Storage owner dies | that partition's hot reads stall | new owner replays partition from log's last offset; cold/blobs unaffected (object store) |
| Object store AZ outage | cold reads degrade | object store multi-AZ; hot tier still serves recent |
| Compactor/GC double-run | **blob loss** if unguarded | leader election makes it a non-event; union-live-set if ever shared |
| Whole region loss | regional outage | cross-region log mirror + object-store replication; warm standby region |

## 8. Open questions

1. **Shard rebalancing.** Adding a shard re-hashes `trace_id` ownership. Do we
   accept a window where recent traces split across old/new owners (queries
   already fan out, so reads stay correct), or do we use consistent hashing +
   explicit hot-tier handoff? Lean consistent hashing; cold data stays addressable
   by partition path regardless.
2. **WAL shipping vs. log-as-WAL.** Is it worth building WAL streaming for the
   sharded interim (Strategy 1 HA), or do we jump straight to Kafka-as-WAL
   (Strategy 2) and accept single-writer-no-standby until then? Depends on how
   long Strategy 1 is the production topology.
3. **`query_sql` semantics under fan-out.** Keep it a single-node power tool, or
   invest in a distributed SQL surface? Tied to the DuckDB→DataFusion retirement
   (Phase 6).
4. **Hot-read routing in the query tier.** Does the stateless query tier read the
   hot window over the network from the owning storage node, or do storage nodes
   also serve a read replica? Network read is simpler; replica is faster. Decide
   against measured hot-read latency.
5. **Tenant as the primary shard key.** When multi-tenancy lands (design Open
   Q #4), does `hash(tenant)` alone shard well, or do large tenants need
   `hash(tenant, trace_id)` sub-sharding? Almost certainly the latter for whales.
