# Tael: AI-Agent-Native Observability Platform

## Problem

Existing observability platforms (Datadog, Honeycomb, Grafana) are built for humans staring at dashboards. AI agents — like Claude Code, Devin, or custom autonomous agents — need to monitor, debug, and react to production systems programmatically. They don't use browsers. They need structured, queryable, CLI-first access to traces, metrics, and logs.

There is no observability platform designed for machine consumption as a first-class interface.

## Goals

1. **Ingest OpenTelemetry traces/logs and OpenMetrics/Prometheus metrics** via standard protocols — no custom SDKs required.
2. **Provide a CLI as the primary interface** that AI agents (and power-user humans) use to query, monitor, and alert on telemetry data.
3. **Return structured output** (JSON, tables) that agents can parse and reason over without scraping HTML or interpreting screenshots.
4. **Support natural-language and structured queries** so agents can ask "what's slow?" or run precise PromQL/trace filters.
5. **Optimize for agent workflows**: correlation, root-cause suggestions, anomaly detection, and watch/subscribe patterns.

## Non-Goals

- Building a full GUI dashboard (out of scope for v1; a minimal web UI for humans is a future consideration).
- Replacing Prometheus or Jaeger — we sit on top of standard protocols, not beside them.
- Multi-tenancy or enterprise RBAC in v1.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│                    Data Sources                         │
│  (any app instrumented with OTel SDK or Prometheus)     │
└──────────┬──────────────────────┬───────────────────────┘
           │ OTLP (gRPC/HTTP)    │ Prometheus remote-write
           ▼                     ▼
┌─────────────────────────────────────────────────────────┐
│                  Ingestion Layer                        │
│                                                         │
│  ┌──────────────┐  ┌───────────────┐  ┌──────────────┐ │
│  │ OTLP Receiver│  │ Prom Remote   │  │ Log Receiver │ │
│  │ (traces+logs)│  │ Write Receiver│  │ (OTLP logs)  │ │
│  └──────┬───────┘  └──────┬────────┘  └──────┬───────┘ │
│         └─────────┬───────┘───────────────────┘         │
│                   ▼                                     │
│         ┌─────────────────┐                             │
│         │  Pipeline        │                            │
│         │  (normalize,     │                            │
│         │   enrich,        │                            │
│         │   route)         │                            │
│         └────────┬────────┘                             │
└──────────────────┼──────────────────────────────────────┘
                   ▼
┌─────────────────────────────────────────────────────────┐
│                  Storage Layer                          │
│                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ Trace Store  │  │ Metric Store │  │ Log Store    │  │
│  │ (ClickHouse  │  │ (ClickHouse  │  │ (ClickHouse  │  │
│  │  or DuckDB)  │  │  or DuckDB)  │  │  or DuckDB)  │  │
│  └──────────────┘  └──────────────┘  └──────────────┘  │
└──────────────────┬──────────────────────────────────────┘
                   │
                   ▼
┌─────────────────────────────────────────────────────────┐
│                  Query Engine                           │
│                                                         │
│  - PromQL-compatible metric queries                     │
│  - Trace search (by service, span, duration, status)    │
│  - Log filtering (structured field queries)             │
│  - Cross-signal correlation (trace → metrics → logs)    │
│  - Anomaly detection (baseline comparisons)             │
└──────────────────┬──────────────────────────────────────┘
                   │
          ┌────────┴────────┐
          ▼                 ▼
┌──────────────┐   ┌────────────────┐
│   CLI        │   │  API Server    │
│  (primary)   │   │  (gRPC + REST) │
└──────────────┘   └────────────────┘
```

## Components

### 1. Ingestion Layer

Accepts telemetry via standard protocols. No proprietary agents.

| Signal  | Protocol                        | Port (default) |
|---------|---------------------------------|----------------|
| Traces  | OTLP gRPC / OTLP HTTP          | 4317 / 4318    |
| Metrics | OTLP gRPC / Prometheus remote-write | 4317 / 9090 |
| Logs    | OTLP gRPC / OTLP HTTP          | 4317 / 4318    |

The ingestion layer normalizes everything into a unified internal model before storage. We use the OpenTelemetry Collector as a reference but implement a purpose-built receiver to keep the binary small and focused.

### 2. Storage Layer

**v1 (single-node):** DuckDB as an embedded columnar database. Zero external dependencies, single-file storage, and analytical query performance far beyond SQLite for time-series and aggregation workloads. DuckDB's columnar engine gives us ClickHouse-like query patterns (fast GROUP BY, window functions, range scans) locally without running a separate server.

**v2 (scale):** ClickHouse for distributed columnar storage. The migration path is smooth since both are columnar and share similar SQL dialects — queries written for DuckDB translate to ClickHouse with minimal changes.

Key schema concepts:
- **Traces**: stored as spans with parent references, indexed on service, operation, duration, status, and attributes.
- **Metrics**: time-series with labels, stored in a columnar layout optimized for range queries.
- **Logs**: structured records with indexed fields and full-text search on body.

### 3. Query Engine

The query engine supports two modes:

**Structured queries** — a filter DSL that maps cleanly to CLI flags:
```
tael query traces --service=api-gateway --min-duration=500ms --status=error --last=1h
```

**PromQL-compatible metric queries:**
```
tael query metrics 'rate(http_requests_total{status="500"}[5m])'
```

**Cross-signal correlation** — the killer feature. Given a trace ID, pull every span, every log tagged with that `trace_id`, and all metrics from the touched services inside the trace's time window:
```
tael correlate --trace <trace-id>
```
Future work: metric-driven correlation (`--metric http_latency_p99 --threshold '>2s'`) to walk the other direction.

### 4. CLI (`tael`)

The CLI is the primary interface. It must be excellent for both AI agents and human power users.

#### Design Principles
- Every command returns structured JSON by default (`--format=json`). Human-readable tables via `--format=table`.
- Consistent flag patterns across all subcommands.
- Streaming support for watch/tail operations.
- Exit codes that encode error categories (not just 0/1).
- Built-in `--explain` flag that adds plain-English context to results (useful for agents reasoning about output).

#### Command Surface

```
tael ingest status                    # health of ingestion pipelines
tael query traces [filters]           # search/filter traces
tael query metrics [promql]           # query metrics
tael query logs [filters]             # search/filter logs
tael get trace <trace-id>             # full trace waterfall as structured data
tael get metric <name>                # describe a metric (type, labels, recent values)
tael correlate                        # cross-signal correlation
tael watch <query>                    # stream matching results in real-time
tael diff <query> --baseline=<range>  # compare current vs baseline period
tael anomalies [--service=X]          # surface anomalies detected over recent window
tael services                         # list known services and their health
tael topology                         # service dependency map from trace data
tael summarize --last=1h              # agent-friendly summary of system health
```

#### Agent-Optimized Features

- **`tael summarize`**: returns a structured health summary an agent can use to decide what to investigate further. Includes: top errors, latency regressions, anomalous metrics, and recent deploys correlated with changes.
- **`tael anomalies`**: surfaces statistically significant deviations without requiring the agent to define thresholds.
- **`tael correlate`**: eliminates manual cross-signal pivoting — the agent says "this metric spiked, what's related?" and gets traces + logs back.
- **`tael watch`**: polls the summary endpoint on an interval and prints signed deltas per tick (span count, error count, error rate, p95, log errors, metric volume). A future `--exit-on=<condition>` flag will let an agent subscribe to a query and exit once a threshold is crossed ("watch this deploy and tell me if error rate exceeds 1%").
- **`tael diff`**: compare a time range against a baseline. Agents use this to answer "is this deploy worse than the last one?"

### 5. API Server

gRPC and REST endpoints that mirror the CLI surface 1:1. The CLI is a thin client over this API. This means any agent that prefers HTTP can use the API directly.

## Data Model

### Unified Event Model

All signals share a common envelope:

```
{
  "timestamp": "2026-04-09T12:00:00Z",
  "service": "api-gateway",
  "environment": "production",
  "signal": "trace|metric|log",
  "attributes": { ... },
  "resource": { ... },
  "payload": { ... }  // signal-specific
}
```

This allows cross-signal queries without joins.

### Trace Payload
```
{
  "trace_id": "abc123",
  "span_id": "def456",
  "parent_span_id": "ghi789",
  "operation": "HTTP GET /users",
  "duration_ms": 142,
  "status": "ok|error",
  "events": [ ... ],
  "links": [ ... ]
}
```

### Metric Payload
```
{
  "name": "http_requests_total",
  "type": "counter|gauge|histogram|summary",
  "value": 42,
  "labels": { "method": "GET", "status": "200" }
}
```

### Log Payload
```
{
  "severity": "ERROR",
  "body": "connection refused to downstream",
  "trace_id": "abc123",
  "span_id": "def456",
  "fields": { "retry_count": 3 }
}
```

## Technology Choices

| Component       | Choice             | Rationale                                              |
|-----------------|--------------------|--------------------------------------------------------|
| Language        | Rust               | Fast, single binary, memory-safe, no GC pauses         |
| Storage (v1)    | DuckDB (via duckdb-rs) | Embedded columnar DB, analytical perf locally       |
| Storage (v2)    | ClickHouse         | Columnar, fast aggregations, proven at scale           |
| CLI framework   | clap               | Standard Rust CLI library, excellent completions       |
| API transport   | tonic + axum       | tonic for gRPC, axum for REST, both async on tokio     |
| Serialization   | serde + prost      | serde for JSON/YAML, prost for protobuf decoding       |
| Config          | YAML               | Familiar, easy for agents to read/write                |
| OTel parsing    | opentelemetry-rust | Official Rust SDK for OTLP decoding                    |
| Async runtime   | tokio              | Industry-standard async runtime for Rust               |

## Deployment Modes

### Single Binary (v1 target)
```
tael server start --storage=sqlite --data-dir=./data
```
One process handles ingestion, storage, query, and API. Good for local dev, single-team use, or an agent monitoring its own infra.

### Distributed (v2)
Separate ingestion, storage, and query services. ClickHouse cluster for storage. Horizontal scaling of ingestion and query nodes.

## MCP / Tool-Use Integration (Future)

The CLI is the immediate interface, but the natural evolution is an **MCP server** that exposes observability tools directly to agents:

```json
{
  "tools": [
    { "name": "query_traces", "description": "Search distributed traces", ... },
    { "name": "get_anomalies", "description": "Surface anomalous metrics", ... },
    { "name": "correlate", "description": "Cross-signal correlation", ... }
  ]
}
```

This lets agents like Claude Code call observability tools without shelling out.

## Milestones

### M1: Foundation
- [x] Project scaffolding (Rust workspace, CI, linting)
- [x] OTLP gRPC receiver for traces
- [x] DuckDB trace storage with basic schema
- [x] `tael query traces` with service/duration/status filters
- [x] `tael get trace <id>` with structured JSON output

### M2: Metrics + Logs
- [x] OTLP metrics receiver
- [x] Prometheus remote-write receiver
- [x] DuckDB metric storage
- [x] OTLP log receiver + storage
- [x] `tael query metrics` with PromQL subset
- [x] `tael query logs` with field filters

### M3: Agent-Native Features
- [x] `tael summarize` — system health digest
- [x] `tael anomalies` — baseline-vs-current regression detection
- [x] `tael correlate` — cross-signal correlation by trace ID
- [x] `tael watch` — polling summary deltas
- [ ] `tael diff` — baseline comparison
- [ ] `tael topology` — service dependency graph

### M4: Scale + Polish
- [ ] ClickHouse storage backend
- [ ] MCP server integration
- [ ] Retention policies and downsampling
- [ ] Auth (API keys)
- [ ] Packaging (Homebrew, Docker)

## Agent Auth Model

### Design Principles
- Auth should be zero-friction for single-agent/local use and scale to multi-agent environments without rearchitecting.
- Agents are first-class principals — not users impersonating humans.

### Defaults

**Single-node (v1):** No auth required by default. The server binds to `127.0.0.1` only. If you can reach the socket, you're in. This matches the local-dev mental model — no API key ceremony to get started.

Enable auth explicitly with `tael server start --auth=required`.

**Multi-agent:** API key per agent identity. Keys are created via the CLI:

```
tael auth create-key --name="claude-code-prod" --role=reader
tael auth create-key --name="deploy-bot" --role=reader
tael auth create-key --name="admin" --role=admin
```

### Roles

| Role     | Capabilities                                                    |
|----------|-----------------------------------------------------------------|
| `reader` | Query traces, metrics, logs. Read anomalies, topology, summaries. |
| `writer` | Everything in `reader` + push telemetry via OTLP/remote-write.  |
| `admin`  | Everything in `writer` + manage keys, retention, server config.  |

Most agents are `reader` — they consume observability data. The `writer` role exists for agents that also instrument and report their own telemetry. `admin` is for operators.

### Key Format
Keys are prefixed for easy identification: `tael_r_<random>` (reader), `tael_w_<random>` (writer), `tael_a_<random>` (admin). Passed via `--api-key` flag or `TAEL_API_KEY` env var.

### Scoping (v2)
Future: optional service-level scoping so an agent can only see telemetry from specific services. Not needed for v1 — most deployments are single-team.

## Retention Policy

### Defaults

| Signal   | Raw Retention | Downsampled Retention | Rationale                                    |
|----------|---------------|-----------------------|----------------------------------------------|
| Traces   | 7 days        | —                     | Large, high-cardinality; 7d covers most investigations |
| Metrics  | 30 days (raw) | 1 year (5m rollups)   | Agents need recent precision + long-term trends |
| Logs     | 14 days       | —                     | Middle ground; logs are verbose but searchable |

### How It Works

- Retention is enforced by a background cleanup job that runs hourly.
- Configurable per signal via `tael server start --retention-traces=7d --retention-metrics=30d --retention-logs=14d`.
- Also configurable in the YAML config file:

```yaml
retention:
  traces: 7d
  metrics:
    raw: 30d
    downsampled: 365d
    downsample_interval: 5m
  logs: 14d
```

### Metric Downsampling

Raw metric data points are rolled up into 5-minute aggregates (min, max, avg, sum, count) after the raw retention window. This preserves trend visibility for capacity planning and long-term comparisons while keeping storage bounded.

### Storage Estimates (rough)

Assuming a mid-size deployment (~50 services, moderate traffic):
- **Traces**: ~2-5 GB/day → ~15-35 GB at 7d retention
- **Metrics**: ~500 MB/day raw → ~15 GB at 30d + ~5 GB/year downsampled
- **Logs**: ~1-3 GB/day → ~15-40 GB at 14d retention

Total: **~50-90 GB** for a single-node DuckDB deployment. Well within local disk for most machines.

## Open Questions

1. ~~**Naming**: resolved — `tael` (**t**race **a**gent **e**vent **l**og). Short, unique, no conflicts.~~
2. **Storage default**: DuckDB gives us columnar performance locally. Need to validate concurrent write throughput under high-ingest scenarios — DuckDB is single-writer, so we may need a write-ahead buffer or batching layer.
3. ~~**Agent auth model**: resolved — see Auth section below.~~
4. ~~**Retention**: resolved — see Retention section below.~~
5. ~~**Natural language query layer**: resolved — leave it to the calling agent. Tael returns structured data; the agent is already an LLM that can formulate queries and interpret results. Embedding a query translator adds complexity and a model dependency we don't need.~~
