# Scaling & HA: tael-server on the tael-backend engine

> Status: Part design, part as-built · Owner: colton@thousandbirds.ai · Last updated: 2026-05-26
>
> Companion to [`tael-backend-design.md`](./tael-backend-design.md) (the storage
> engine) and [`tael-backend-impl-plan.md`](./tael-backend-impl-plan.md). This
> doc covers how to run `tael-server` with the `tael-backend` engine across more
> than one node — how to scale ingest and query horizontally, and how to
> structure the deployment for high availability. It picks up where the design
> doc's **B5: Scale path (v2)** leaves off.

> **Implementation status.** The near-term horizontal + HA path is **built**
> (phased rollout §6, items 1–4): read fan-out (`FanoutStore` + `RemoteStore`),
> `trace_id` write routing, ops hardening (health/readiness + graceful drain),
> synchronous WAL replication (`required_acks`), and automatic failover via
> chitchat leader election + epoch fencing. **Remaining** (items 5–7): the async
> object-store cold/blob tier, the DataFusion analytics unification (retiring the
> DuckDB projection), and full ingest/query disaggregation. Per-section "Status
> (landed)" notes mark exactly what exists; everything else is design ahead of
> code.

## TL;DR

`tael-server` today is a **single-node, embedded, single-writer** process: WAL +
LSM hot tier + DuckDB projection + Tantivy index + blob store all live behind one
process holding exclusive file locks on one data dir (`tael-server/src/lib.rs`,
`storage/backend/mod.rs`). You cannot point two instances at the same data dir
and you cannot run two instances with the same WAL key — the WAL namespace is
process-global (`storage/backend/wal.rs`, `TaelBackend::with_wal_key`).

So horizontal scale is **not** "run N replicas of the same process." It is one of
two grains, in order of effort:

1. **Shard the telemetry stream** across N independent single-writer instances,
   each owning its own WAL/hot/cold, with a routing layer in front and a
   scatter-gather query layer on top. HA comes from per-shard replication.
2. **Disaggregate** into stateless ingest, single-writer-per-shard storage, and
   stateless query tiers that share a **replicated WAL** (walrus + a WAL-shipping
   layer — *not* Kafka; see below) and an object-store cold tier. This is the
   target; it's what B5 gestures at.

> **No Kafka/Redpanda.** The WAL is already walrus (`storage/backend/wal.rs`):
> durable fsync'd append, topic streams, and retained read offsets that
> `TaelBackend::replay` uses today. The only thing a broker would add over
> walrus is **cross-node replication** (surviving node *loss*, not just crash).
> We get that by building a small **WAL-shipping/replication layer on top of
> walrus** (§5.1), keeping the embedded log and owning a lightweight
> leader→standby stream — rather than taking a heavyweight broker dependency.
> walrus is single-node by design, so replication and failover are ours to
> build; the broker semantics we actually need are a small subset (replicate
> before ack, one owner per partition, a retained offset for replay).

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
| OTLP gRPC + REST | async, many connections | `lib.rs` two `tokio::spawn` listeners |
| `Store` access | `Arc<dyn Store>`, `Send + Sync`, **synchronous** methods | `storage/mod.rs:26` |
| WAL | one process per namespace key (process-global) | `wal.rs`, `mod.rs:51` |
| Hot tier (fjall) | exclusive DB lock per `<data_dir>/hot` | `hot.rs:43` |
| DuckDB projection | **single-writer** | design §"Why not just keep DuckDB" |
| Compactor / retention / blob GC | single in-process background task | `lib.rs:spawn_span_compactor` |
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
4. **Writes ack after a local fsync, not after replication** *(default; closed by
   §5.1)*. The base write path is WAL append (fsync) → apply to hot+projection →
   mark applied (`mod.rs:200`) — durability is "survives this node's crash," not
   "survives this node's loss." HA adds the second guarantee: with standbys
   configured (`TAEL_WAL_STANDBYS`) the write ack now waits for WAL replication
   (§5.1), so this constraint applies only to a node with no standbys.
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
- `get_trace` / `query_correlate`: route to the owning shard (hash the
  `trace_id`), with a fan-out fallback if it comes back empty — so a
  rebalancing window or hash skew can't drop a trace.
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
also needs a **remote `Store` client** (a thin HTTP client implementing `Store`
against another tael-server's REST API).

> **Status (landed):** both components exist. `RemoteStore`
> (`storage/remote.rs`) is a read-only `Store` over another node's REST API
> (blocking HTTP, so it satisfies the synchronous trait); `FanoutStore`
> (`storage/fanout.rs`) routes `get_trace`/`correlate`/comments and the write
> path to the owning shard by `hash(trace_id)`, fans out and re-limits the
> `query_*` reads, and re-aggregates `list_services`/`query_summary`/
> `query_anomalies`. `query_sql` is intentionally non-distributed (option (c)).
> A node runs as a stateless query tier when `TAEL_QUERY_SHARDS` is set to the
> comma-separated shard base URLs — it then serves reads via the `FanoutStore`
> and opens no local engine. Summary percentiles merge as a span-count-weighted
> approximation (exact cross-shard quantiles need a t-digest — future work), and
> anomaly merge is best-effort since `trace_id` sharding spreads a service's
> traffic across shards.

### HA within a shard

Sharding alone is scale, not availability — losing one shard loses 1/N of the
data. Add per-shard redundancy:

- **Replicate the WAL** to a standby instance of the same shard. A standby that
  tails the leader's shipped WAL rebuilds identical hot+cold state via the
  existing replay path (`TaelBackend::replay`, `wal.rs:drain`); on leader loss,
  chitchat elects the standby and epoch fencing locks out the old leader. **This
  is built** — see the §5.1 status note (WAL shipping + election + fencing).
- **Or share the cold tier + blobs** across the shard's instances on object
  storage, so only the hot tier (last `TAEL_HOT_TIER_HOURS`) must be rebuilt from
  WAL on failover. This is the cheaper recovery and the bridge to Strategy 2
  (the shared object-store tier itself is still item 5, not yet built).

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
   │  Replicated WAL  (walrus + WAL shipping)         │  leader fsync + ship to
   │  one walrus namespace per partition/shard        │  standbys; retained
   │  partitions = shards                              │  offset; this IS the WAL
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

- **The replicated WAL is the durability boundary, and it's still walrus.** The
  WAL already exists (`wal.rs`); making it *replicated* (leader fsync → ship to
  a standby before ack) is what turns a storage node's local state into a
  *rebuildable cache* — node loss replays the partition from the WAL's retained
  offset. This is the single biggest HA win and it closes the
  ack-before-replication gap (constraint #4). No external broker: the
  WAL-shipping layer (§5.1) provides the replicate-before-ack and retained-offset
  semantics on top of the embedded log.
- **Single-writer-per-partition is preserved, not fought.** Each partition is one
  walrus namespace with exactly one owner = exactly one fjall/DuckDB writer = the
  engine's invariant holds, while the *fleet* scales by adding partitions. (This
  is the same single-writer rule walrus already enforces per namespace today —
  the WAL key is process-global, constraint #2 — now used deliberately as the
  partition boundary.)
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

### 5.1 Durability: replicate the walrus WAL (no broker)

Today durability = local fsync (`mod.rs:200`), which survives a crash but not a
node loss, and the WAL is a process-local walrus namespace (`wal.rs`). HA closes
the node-loss gap **by replicating the walrus WAL itself**, not by introducing a
broker.

**What walrus already gives us** (`walrus-rust` 0.2): durable fsync'd append
(`append_for_topic` / `batch_append_for_topic`), topic streams, retained read
offsets that survive restart, and `StrictlyAtOnce` / `AtLeastOnce` read
consistency. `TaelBackend::replay` (`wal.rs:drain`) already rebuilds state from
it. What walrus does **not** provide (it is single-node by design): cross-node
replication, a network transport, or multi-consumer partitions. So the net-new
work is a thin **WAL-shipping layer** around walrus that supplies exactly those.

**The WAL-shipping layer** (the one net-new component for HA durability):

- *Leader side:* on append, frame the record (the existing
  `[version][tag][batch]` framing) and stream it to the shard's standby(s) over
  a simple transport (length-prefixed records over TCP/HTTP, or gRPC). Ack the
  write only after the standby acknowledges the framed record — this is the
  replicate-before-ack guarantee that closes constraint #4.
- *Standby side:* receive framed records, append them to its **own** walrus
  namespace, and apply via the existing replay path so its hot+cold state stays
  byte-identical to the leader's. A standby is just a follower running the same
  `apply_*` code on a shipped stream.
- *Failover:* on leader loss, a standby that has applied up to offset *N* is
  promoted; it already holds the state and simply starts accepting writes. The
  retained walrus offset bounds how much replay a cold start needs.
- *Bounded replay:* checkpoint the applied offset alongside the hot-tier flush
  (`Store::flush`) so recovery replays only the unflushed tail, not all history.

**Block-device alternative (interim):** put the WAL on storage-level–replicated
volumes and fail over (EBS multi-attach is *not* safe with the exclusive-lock
engines — use replication + failover, never a shared mount). Cheaper to stand up
than WAL shipping, but coarser (no per-record replicate-before-ack).

**Target (disaggregated):** the per-partition walrus namespace *is* the
replicated WAL. Local hot/cold state is a rebuildable view; recovery = replay
from the last checkpointed offset. This is the same mechanism as the sharded
near-term path, just with one partition-owner per walrus namespace and the
shipping layer providing the replication a broker would otherwise own.

> **Status (landed, end to end).** WAL shipping works over HTTP:
>
> - *Seam* (`storage/backend/wal.rs`): a `WalSink` trait is the replication
>   target; `WalLog` ships each appended record's framed bytes to its sinks
>   **before the write returns**, and is a no-op when none are configured.
>   `WalRecord::decode` is the shared codec the standby decodes with.
> - *Transport* (`storage/remote.rs`): `RemoteWalSink` POSTs framed records to a
>   standby's `POST /internal/wal/records` (blocking HTTP, like `RemoteStore`);
>   the standby applies them via `Store::apply_framed_wal` (append → apply →
>   consume, mirroring the leader so the standby is itself replayable).
> - *Sync policy:* `WalLog` enforces `required_acks` — a write fails only if
>   fewer than N standbys confirmed. Default is **all** (fully synchronous: a
>   write survives node loss because every standby has it before ack); `Some(0)`
>   is async best-effort (a down standby never blocks the leader; the record
>   stays locally durable for replay). Tune via `TAEL_WAL_REQUIRED_ACKS`.
> - *Config:* a node becomes a **leader** by setting `TAEL_WAL_STANDBYS` to its
>   standbys' base URLs; a standby is any tael-backend node (its
>   `/internal/wal/records` endpoint is always present).
> - *Tests:* an in-process loopback proves a standby rebuilds identical state
>   from the shipped WAL; a two-server test exercises the full HTTP path; a unit
>   test pins the `required_acks` availability/durability tradeoff.
>
> **Automatic election + fencing (landed, `cluster/`).** Failure detection and
> leader election are handled by [`chitchat`](https://crates.io/crates/chitchat)
> (gossip + phi-accrual), not a broker or external coordinator:
>
> - A replication group forms one chitchat cluster; the leader is the live
>   member with the smallest node id (`cluster::election::elect_leader`). When
>   the leader dies it drops out of the live set and the next node is elected
>   automatically — no quorum service.
> - **Epoch fencing** guards against a deposed leader: each reign carries a
>   strictly increasing epoch (a promoted node bumps past every epoch it has
>   seen, advertised via gossip), stamped on shipped records via the
>   `x-tael-wal-epoch` header. A standby's `EpochFencer` rejects any record below
>   the highest epoch it has accepted (HTTP 409), so a stale leader can't corrupt
>   replicas. The election/fencing logic is pure and unit-tested; chitchat is a
>   thin adapter (`ClusterCoordinator`).
> - *Config:* `TAEL_CLUSTER_LISTEN` (+ `TAEL_CLUSTER_SEEDS`, `TAEL_NODE_ID`,
>   `TAEL_CLUSTER_ID`) turns it on; `GET /internal/cluster` reports node id /
>   leadership / epoch. A standby remains a hot replica, so promotion needs no
>   internal state flip — ingest just follows the elected leader.
>
> **Known limit.** This is best-effort fencing over an *eventually-consistent*
> membership view — it closes the dangerous split-brain window but is not the
> linearizable guarantee a consensus log (Raft) gives; under a network partition
> each side may elect its own leader. That tradeoff is deliberate: chitchat keeps
> the system embedded and broker-free. A quorum/Raft path (e.g. `openraft`) is
> the upgrade if linearizable failover is ever required.

### 5.2 The singleton compactor / GC owner

`spawn_span_compactor` must run **exactly once per cold+blob namespace** (constraint
#3). Concretely:

- Sharded model: each shard owns its own cold+blobs, so its own in-process
  compactor is automatically the sole owner — fine as-is. This holds even within
  a replication group today: leader and standby each have their own data dir, so
  each compacts its own copy independently (no shared namespace, no race). The
  compactor is therefore **not** gated on the elected leader yet, and doesn't
  need to be.
- Disaggregated/shared-object-store model *(item 5, not yet built)*: once cold +
  blobs are a single shared bucket, compaction and `blobs.gc` must move to the
  per-partition storage owner, gated by the chitchat **leader election** we now
  have (§5.1). Never run blob GC from two processes against one bucket —
  `collect_live_blob_hashes` only sees one node's live rows and will delete
  another's blobs. If GC ever spans multiple writers' blobs, it must compute the
  live set as the **union across all owners** (or switch to refcounts).

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
- **Health endpoints:** liveness (`GET /healthz`, always 200, touches nothing)
  and readiness (`GET /readyz`, 200/503 from `Store::health()`) live on the REST
  router (`api/rest.rs`). `health()` is a `Store` trait method: a no-op for the
  embedded engine (constructed + locks held ⇒ ready), a `/healthz` ping for
  `RemoteStore`, and "≥1 reachable shard" for `FanoutStore` (so a query node
  degrades to partial results rather than dropping out of rotation).
- **Graceful shutdown:** `run()` now drains on SIGTERM/Ctrl-C — the REST listener
  uses `with_graceful_shutdown` and the gRPC listener `serve_with_shutdown`, both
  awaiting the same signal; after both drain, `Store::flush()` tightens the hot
  tier (`db.persist(SyncAll)`) so a restart/standby replays less WAL. (WAL fsync
  on the write path remains the durability boundary, so the flush is
  best-effort.) The old `tokio::select!` with no drain path is gone.

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

Writes are synchronous through fsync (`insert_spans`), which already applies
*implicit* backpressure — a slow disk slows the ack. **Not yet implemented:**
explicit shedding (OTLP gRPC `RESOURCE_EXHAUSTED` / remote-write 429) and a
bounded receive queue, so a sustained burst still buffers in the async runtime
rather than being cleanly rejected with a retryable error. walrus's
`batch_append_for_topic` + a tuned `FsyncSchedule` amortize fsync cost and the
local-NVMe WAL is the burst buffer; the explicit shed-with-429 path is the
remaining hardening (no broker needed to absorb spikes).

---

## 6. Phased rollout (extends design B5)

1. **Vertical + ops hardening** (no new topology): health/readiness probes,
   graceful drain, `TAEL_COLD_DIR` on shared/object-backed mount, tune
   `TAEL_HOT_TIER_HOURS`. Single node, but operable and recoverable.
   — *Landed:* `GET /healthz` + `GET /readyz` and SIGTERM-graceful drain/flush
   (§5.4). `TAEL_COLD_DIR`/`TAEL_HOT_TIER_HOURS` already existed.
2. **Remote `Store` client + `FanoutStore`**: the scatter-gather query layer and
   an HTTP/gRPC `Store` client. Unlocks read fan-out without changing the API.
   — *Landed:* `storage/remote.rs` + `storage/fanout.rs`, enabled via
   `TAEL_QUERY_SHARDS` (§3).
3. **Sharded writes**: OTel Collector routing on `trace_id`; N independent
   instances. This is the first true horizontal step. (`FanoutStore` already
   routes writes by `hash(trace_id)` to the owning shard, so a single ingest
   endpoint can shard in-process; the OTel Collector `routingconnector` is the
   production edge-routing alternative.)
4. **WAL shipping (walrus replication) + automatic failover**: leader→standby
   replicate-before-ack on top of the walrus WAL (§5.1), with chitchat-based
   election and epoch fencing on leader loss. Closes the node-loss durability gap
   without a broker. — *Landed:* `WalSink` + `RemoteWalSink` over
   `POST /internal/wal/records`, `required_acks` sync policy
   (`TAEL_WAL_STANDBYS` / `TAEL_WAL_REQUIRED_ACKS`); chitchat election + epoch
   fencing (`cluster/`, `TAEL_CLUSTER_*`). Quorum/Raft (linearizable) failover is
   the optional upgrade (Open Q #2).
5. **Async object-store cold + blobs** (design B5/Phase 9): shared system of
   record; failover only rebuilds the hot window.
6. **DataFusion unification** (design Phase 6): retire the DuckDB projection so the
   query tier is fully stateless and reads scale independently.
7. **Disaggregated tiers**: stateless ingest + query autoscale; one storage owner
   per partition; leader-elected compaction/GC.

## 7. Failure modes (target/disaggregated)

| Failure | Blast radius | Recovery |
|---|---|---|
| Query node dies | none (stateless) | LB removes it; retry elsewhere |
| Ingest node dies | none (writes acked only after WAL replication) | LB removes it; producers retry |
| Storage owner dies | that partition's hot reads stall | chitchat detects the death and elects the next node; the standby already tailed the shipped WAL (or cold-start replay from the retained walrus offset); cold/blobs unaffected (object store) |
| Deposed leader keeps writing | none — replicas fence it | epoch fencing: standbys reject the stale leader's records (409) |
| Object store AZ outage | cold reads degrade | object store multi-AZ; hot tier still serves recent |
| Compactor/GC double-run | **blob loss** if unguarded | leader election makes it a non-event; union-live-set if ever shared |
| Whole region loss | regional outage | cross-region WAL shipping (a standby in another region) + object-store replication; warm standby region |

## 8. Open questions

1. **Shard rebalancing.** Adding a shard re-hashes `trace_id` ownership. Do we
   accept a window where recent traces split across old/new owners (queries
   already fan out, so reads stay correct), or do we use consistent hashing +
   explicit hot-tier handoff? Lean consistent hashing; cold data stays addressable
   by partition path regardless.
2. **Linearizable failover, if ever needed.** Replication, election, and fencing
   are built on walrus + chitchat (§5.1): embedded, broker-free, but *best-effort*
   — election runs over an eventually-consistent gossip view, so a network
   partition can transiently produce two leaders (epoch fencing limits the blast
   radius but isn't a partition-proof guarantee). If linearizable failover
   becomes a requirement, the upgrade is a quorum consensus log (`openraft`): one
   Raft group per shard, the Raft log replacing the WAL-shipping path, with
   election + fencing from the algorithm. Deferred until a workload needs it —
   the gossip path is materially simpler to operate.
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
