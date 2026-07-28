# Running tael for a team

> The operations guide for multi-node tael: which topologies are supported,
> how to configure each one, and the rules that keep them safe. Companion to
> [`tael-server-scaling-ha.md`](tael-server-scaling-ha.md) (the design and its
> rationale); this doc is the "what do I actually run" side.

Every topology below is the same single binary (`tael serve`) in a different
role, selected entirely by environment variables. Start at the top and move
down only when a measurement says you must — each step down adds operational
surface.

## The rules that hold everywhere

1. **One process per data dir.** The embedded engines (fjall, Tantivy, the
   WAL) take exclusive locks. Never point two processes at one
   `TAEL_DATA_DIR` or share one over NFS/EBS-multi-attach.
2. **One writer per shard.** A shard's WAL namespace has exactly one owner.
   HA comes from replication + failover, never from two concurrent writers.
3. **One blob-GC owner per shared blob store.** On a shared (object-store)
   blob bucket, exactly one node may garbage-collect, and it must union every
   writer's live set first (`TAEL_BLOB_GC_PEERS`). Node-local blob dirs need
   no coordination.
4. **Same keystore on every node.** Auth is file-based
   (`<data_dir>/keys.json`). Nodes that forward or fan out for each other
   must all resolve the same keys — distribute the file (it stores salted
   digests, not the keys themselves).
5. **Firewall `/internal/*`.** `POST /internal/wal/records`,
   `GET /internal/blobs/live`, and `GET /internal/cluster` are for peers, not
   clients. Keep them reachable only inside the cluster network.

## Topology 0 — one node (start here)

```
tael serve
```

One process: ingest (OTLP gRPC :4317, OTLP HTTP :4318, Datadog :8126,
remote-write), storage, and the query API (:7701). Vertical scaling goes a
long way — fast NVMe for `TAEL_DATA_DIR`, `TAEL_COLD_DIR` on a cheaper mount,
`TAEL_HOT_TIER_HOURS` tuned down if reads slow. Exhaust this before adding
nodes.

## Topology 1 — leader + standby (HA, no scale change)

One writer, one (or more) hot standbys, synchronous WAL replication, gossip
election. A write is acked only after every standby holds it, so losing the
leader loses nothing acked.

```bash
# standby (start first)
TAEL_DATA_DIR=/var/tael-b TAEL_WAL_DIR=/var/tael-b/wal \
TAEL_REST_API_ADDR=0.0.0.0:7701 \
TAEL_CLUSTER_LISTEN=0.0.0.0:9890 TAEL_CLUSTER_SEEDS=leader:9890 \
TAEL_NODE_ID=b-standby \
tael serve

# leader
TAEL_DATA_DIR=/var/tael-a TAEL_WAL_DIR=/var/tael-a/wal \
TAEL_REST_API_ADDR=0.0.0.0:7701 \
TAEL_WAL_STANDBYS=http://standby:7701 \
TAEL_CLUSTER_LISTEN=0.0.0.0:9890 TAEL_CLUSTER_SEEDS=standby:9890 \
TAEL_NODE_ID=a-leader \
tael serve
```

- **Election** is "smallest live node id" over chitchat gossip; give the
  preferred leader the smaller id. When the leader dies, the standby is
  promoted automatically and (already holding all acked state) just starts
  taking traffic — repoint ingest at it via your LB/DNS.
- **Fencing:** each reign carries an increasing epoch stamped on shipped
  records; a deposed leader's records are rejected with 409. This is
  best-effort over gossip, not Raft — a network partition can transiently
  produce two leaders. See design Open Q #2 before betting a compliance
  story on it.
- `TAEL_WAL_REQUIRED_ACKS` tunes the durability/availability trade: default
  = all standbys (a down standby blocks writes), `0` = async best-effort
  (a down standby loses nothing already applied, but unshipped acked writes
  die with the leader's disk).
- **Drill it.** `scripts/failover-drill.sh` runs the whole story — two real
  processes, real traffic, SIGKILL, election, zero acked-write loss — in
  about a minute. Run it against every release you deploy.

## Topology 2 — shard the stream (write scale)

N independent single-writer nodes, each a complete tael, with the telemetry
stream partitioned by `hash(trace_id)` so every trace lands whole on one
shard. Two ways to route:

- **tael's ingest tier** (below) — no external components.
- An OTel Collector with `loadbalancingexporter` (`routing_key: traceID`) in
  front of the shards.

Reads come back together through a **query tier**:

```bash
# stateless query node — no local engine, scatter-gathers the shards
TAEL_QUERY_SHARDS=http://shard-0:7701,http://shard-1:7701 \
tael serve
```

`query_*` re-merge newest-first; `summarize`/`services`/`anomalies`
re-aggregate from per-shard partials (cross-shard percentiles are span-count
weighted approximations). `query sql` is deliberately not distributed — run
it against one shard when you need it. Add HA per shard with Topology 1
(each shard gets its own standby + cluster group).

## Topology 3 — add an ingest tier (edge scale, in-binary routing)

Stateless ingest-only nodes terminate OTLP on both transports, split each
export batch by the shard key (`hash(trace_id)` for spans and logs,
`hash(name)` for metrics — the same hash the query tier uses), and re-post
per-shard OTLP requests. No local engine, no disk growth, autoscale freely.

```bash
TAEL_NODE_ROLE=ingest \
TAEL_INGEST_SHARDS=http://shard-0:7701,http://shard-1:7701 \
tael serve
```

- Producers point at the ingest nodes (any L4/L7 LB in front — trace
  affinity is handled by the split, not the balancer).
- The client's `Authorization` header is forwarded verbatim, so shards
  authenticate the original principal and tenant stamping stays correct
  (rule 4: same keystore everywhere). `TAEL_INGEST_FORWARD_API_KEY` supplies
  a fallback writer key if you terminate client auth at the edge instead.
- Delivery is at-least-once: a failed shard fails the batch with a retryable
  status and the producer resends; shards apply resends idempotently
  (content-derived keys), so retries never duplicate.
- **Scope:** ingest nodes forward OTLP only. Point dd-trace
  (`DD_TRACE_AGENT_URL`) and Prometheus remote-write directly at a storage
  shard; the ingest node's query routes answer with an explanation rather
  than data. LLM payload blobbing and text indexing happen on the owning
  shard, not the edge.

## Multi-tenancy

Two independent levels (see `tael-server/src/tenancy.rs`):

- `TAEL_MULTI_TENANT=1` — *authorization*: API keys carry a tenant, writes
  are stamped server-side (client-supplied `tael.tenant` is overridden),
  reads are scoped, SQL becomes admin-only.
- `TAEL_TENANT_ISOLATION=1` — *isolation* (implies the above): each tenant
  gets its own complete engine under `<data_dir>/tenants/<tenant>/` — own
  WAL, hot tier, cold tier, text index, comments. The tenant is the shard
  key of the storage layout. WAL replication composes (per-tenant engines
  ship to the same standbys). The payload blob store stays shared by design
  (content-addressed dedup). Enabling isolation does not migrate
  pre-isolation data; the server warns if it finds any.

## Object storage (cold tier + blobs)

`TAEL_COLD_STORE=s3|gcs` + `TAEL_COLD_BUCKET`, `TAEL_BLOB_STORE=s3|gcs` +
`TAEL_BLOB_BUCKET` (build with `--features cloud`). Cold Parquet and blobs
become the shared, durable system of record; failover then only rebuilds the
hot window. With a shared blob bucket, designate the GC owner
(`TAEL_BLOB_GC_ROLE=coordinator` on exactly one node, or let the elected
cluster leader own it) and set `TAEL_BLOB_GC_PEERS` to every other writer so
live sets are unioned before any sweep (rule 3).

## Operations checklist

| Concern | Answer |
|---|---|
| Liveness / readiness | `GET /healthz` / `GET /readyz` on every node (query tier: ≥1 shard reachable; ingest tier: ≥1 shard reachable) |
| Graceful deploys | SIGTERM drains listeners, then flushes so the restart/standby replays less WAL |
| Backpressure | `TAEL_INGEST_MAX_IN_FLIGHT` (shed with retryable statuses), `TAEL_METRIC_SERIES_LIMIT` |
| Retention | per-signal TOML config (`tael config init`), enforced by each storage node's own compactor |
| Who is leader? | `GET /internal/cluster` on any cluster member |
| Ingest health | `tael ingest status` per node |
| Engine health | `tael.engine.*` self-metrics (compaction, GC, maintenance timing) |
| Failover confidence | `scripts/failover-drill.sh` (multi-process, real network); the same scenario runs in-process in CI |

## What is deliberately not supported

- Two writers on one data dir or one WAL namespace (rule 1/2).
- Distributed `query sql` (run per-shard, or use the structured commands).
- dd-trace / remote-write termination on ingest-only nodes.
- Linearizable failover under network partitions — the gossip path is
  best-effort by design; a Raft upgrade is the documented escape hatch if a
  workload ever requires it.
