<p align="center">
  <img src=".github/tael-banner.svg" alt="tael" width="720" />
</p>

<p align="center">
  <b>AI-agent-native observability platform</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/tael-cli"><img src="https://img.shields.io/crates/v/tael-cli?style=flat-square&color=2dd4bf&logo=rust&logoColor=white" alt="crates.io" /></a>
  <a href="https://crates.io/crates/tael-cli"><img src="https://img.shields.io/crates/d/tael-cli?style=flat-square&color=2dd4bf" alt="downloads" /></a>
  <a href="https://crates.io/crates/tael-cli"><img src="https://img.shields.io/crates/l/tael-cli?style=flat-square&color=2dd4bf" alt="license" /></a>
  <a href="https://opentelemetry.io/"><img src="https://img.shields.io/badge/OTLP-native-2dd4bf?style=flat-square&logo=opentelemetry&logoColor=white" alt="OTLP native" /></a>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> •
  <a href="#features">Features</a> •
  <a href="#cli-reference">CLI Reference</a> •
  <a href="#architecture">Architecture</a> •
  <a href="DESIGN.md">Design Doc</a>
</p>

---

**tael** is an observability platform built for AI agents. It ingests [OpenTelemetry](https://opentelemetry.io/) traces, logs, and metrics via standard OTLP (and Prometheus remote-write), stores them in a purpose-built tiered engine tuned for OTel + LLM traces, and exposes a CLI-first interface that returns structured JSON — designed for agents like Claude Code, Devin, or custom autonomous systems to query, monitor, and annotate production telemetry programmatically.

No dashboards. No browsers. Just a single `tael` binary — server and client in one — and structured data.

[![asciicast](https://asciinema.org/a/svewi9ncgeH52UFP.svg)](https://asciinema.org/a/svewi9ncgeH52UFP)

## Installation

Supported on macOS (Intel + Apple Silicon) and Linux (x86_64 + aarch64).
Windows is not supported — a dependency in the WAL uses unix-only file I/O.

```bash
# Fastest — download a prebuilt `tael` binary (no compilation)
cargo binstall tael-cli
```

`cargo install tael-cli` also works, but compiles the bundled DuckDB engine from
source (a large C++ build), so it can take several minutes. `cargo binstall`
fetches a prebuilt binary from the GitHub Release instead and finishes in
seconds. Install it once with `cargo install cargo-binstall` (or grab it from
[its releases](https://github.com/cargo-bins/cargo-binstall#installation)).

```bash
# Compiles from source — slower, but no extra tooling
cargo install tael-cli
```

Or build from source:

```bash
cargo build --release
```

## Quickstart

```bash
# Start the server (OTLP on :4317, REST API on :7701)
tael serve

# In another terminal — send sample traces
cargo run --bin tael-test

# Query
tael services --format table
tael query traces --status error --format table
tael query traces --min-duration 500ms --last 1h
tael get trace <trace-id> --format json

# Interactive TUI
tael live
```

## Features

### OTLP Ingestion
Accepts traces, logs, and metrics from any OpenTelemetry-instrumented application via standard OTLP gRPC (port 4317), plus Prometheus remote-write over HTTP (`POST /api/v1/write`). No proprietary SDKs or agents required. LLM spans (`gen_ai.*` semantic conventions) get typed model/token/cost fields, with prompt/completion payloads stored as deduplicated blobs.

### CLI-First Querying
Every command returns structured JSON by default. Human-readable tables via `--format table`.

```bash
# Find errors across all services
tael query traces --status error --format json

# Find slow requests
tael query traces --min-duration 500ms --service api-gateway

# Full trace with span hierarchy, attributes, and events
tael get trace <trace-id>

# Service health overview
tael services
```

### Trace Comments
Agents can annotate traces with comments — useful for collaborative debugging, audit trails, or recording investigation notes.

```bash
# Add a comment to a trace
tael comment add <trace-id> "Root cause: expired DB connection pool" --author oncall-bot

# Attach a comment to a specific span
tael comment add <trace-id> "This query needs an index" --span-id <span-id>

# View comments
tael comment list <trace-id>
```

### Health Summary, Anomalies, Correlation, Watch
Agent-friendly analysis commands built on top of the core query layer.

```bash
# Aggregated health digest over a window (traces, top services, top error
# ops, log severity breakdown, metric volume)
tael summarize --last 1h
tael summarize --last 15m --service api-gateway --format table

# Services whose error rate or p95 regressed vs a baseline window
tael anomalies --last 5m --baseline 30m
tael anomalies --last 10m --baseline 2h --service cart

# Pull spans, logs, and time-window metrics for a single trace
tael correlate --trace <trace-id>

# Poll the summary endpoint on an interval and print signed deltas per tick
tael watch --last 1m --interval 10
```

`anomalies` flags a service when its current-window error rate rises ≥5%
absolute over baseline, or p95 latency regresses ≥1.5× (severity bumps at
10%/25% error delta and 2×/3× latency ratio). `correlate` takes a trace ID
and returns the spans, any logs tagged with that `trace_id`, and metrics
from the touched services within the trace's time range.

### Claude Code Skill
`tael` ships with a [Claude Code skill](./SKILL.md) so Claude Code picks up telemetry-querying instructions automatically when you're debugging inside a project that uses tael. Install it once:

```bash
# Personal install (~/.claude/skills/tael/SKILL.md) — available in every project
tael skill install

# Project-scoped install (.claude/skills/tael/SKILL.md) — committed to this repo
tael skill install --project

# Overwrite an existing install
tael skill install --force

# Just show where it would be written
tael skill where
```

Restart any running Claude Code session after the first install so it picks up the new skill directory. Subsequent `--force` re-installs take effect within the session.

### Interactive TUI
`tael live` launches a terminal UI with live-updating trace feed, service health, and a waterfall trace visualizer.

```
┌─ tael ─────────────────────────────────────────────────────┐
│  1:Traces    2:Services    Trace                           │
├────────────────────────────────────────────────────────────-┤
│ Trace a1b2c3… │ 340ms │ 3 spans                           │
│                  0ms        170ms       340ms              │
│ api-gateway    ████████████████████████████████   340ms    │
│   cart-service ██                                  15ms    │
│   payment-svc  ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓   310ms    │
├────────────────────────────────────────────────────────────-┤
│ span_id: ab4e…  service: payment-svc  status: error       │
│ attrs: payment.provider=stripe  error.type=PaymentDeclined │
│ event(exception): Card declined: insufficient funds        │
├────────────────────────────────────────────────────────────-┤
│ Comments (2)                                               │
│ 06:53:19 oncall-bot: Payment declined — Stripe 402         │
│ 06:53:20 debug-agent: Card expired. Not a system issue.    │
└────────────────────────────────────────────────────────────-┘
 q:quit  esc:back  j/k:navigate  c:comment
```

**Controls:**

| Key | Action |
|-----|--------|
| `1` / `2` | Switch between Traces and Services tabs |
| `j` / `k` | Navigate up/down |
| `Enter` | Open trace waterfall visualizer |
| `Esc` / `Backspace` | Go back |
| `c` | Add comment (in trace view) |
| `Space` | Pause/resume live updates |
| `q` | Quit |

## CLI Reference

```
tael [OPTIONS] <COMMAND>

Commands:
  serve           Run the server (OTLP ingest + storage + REST API)
  query traces    Search and filter traces (--text for LLM payload search)
  query logs      Search and filter logs
  query metrics   Query metrics (incl. PromQL subset)
  query sql       Read-only SQL over the telemetry tables
  get trace       Get a full trace by ID
  services        List known services and their health
  comment add     Add a comment to a trace
  comment list    List comments on a trace
  live            Interactive TUI trace feed
  summarize       Aggregated health summary over a window
  anomalies       Surface services that regressed vs a baseline window
  correlate       Pull spans + logs + metrics for a trace ID
  watch           Poll the summary endpoint and print deltas per tick
  skill install   Install the tael skill into Claude Code
  server status   Check server health

Global Options:
  --format <json|table>   Output format (default: json)
  --server <URL>          Server address (default: http://127.0.0.1:7701)
```

### `tael serve`

Runs the server in the same binary. Flags fall back to the matching env var
(see [Configuration](#configuration)), then to the defaults.

| Flag | Description | Default |
|------|-------------|---------|
| `--otlp-grpc-addr` | OTLP gRPC listen address | `127.0.0.1:4317` |
| `--rest-api-addr` | REST API listen address | `127.0.0.1:7701` |
| `--data-dir` | Telemetry data directory | `./data` |
| `--storage` | Storage backend: `tael-backend` (default) or `duckdb` | `tael-backend` |

### `tael query traces`

| Flag | Description | Example |
|------|-------------|---------|
| `--service` | Filter by service name | `--service api-gateway` |
| `--operation` | Filter by operation (substring) | `--operation checkout` |
| `--status` | Filter by status | `--status error` |
| `--min-duration` | Minimum span duration | `--min-duration 500ms` |
| `--max-duration` | Maximum span duration | `--max-duration 1s` |
| `--last` | Time window | `--last 1h` |
| `--limit` | Max results (default 100) | `--limit 50` |

### `tael summarize`

| Flag | Description | Example |
|------|-------------|---------|
| `--last` | Time window (default 1h) | `--last 15m` |
| `--service` | Filter to a single service | `--service cart` |

### `tael anomalies`

| Flag | Description | Example |
|------|-------------|---------|
| `--last` | Current window (default 1h) | `--last 5m` |
| `--baseline` | Baseline window (default 6× current) | `--baseline 1h` |
| `--service` | Filter to a single service | `--service api` |

### `tael correlate`

| Flag | Description | Example |
|------|-------------|---------|
| `--trace` | Trace ID to pull across signals | `--trace a1b2c3…` |

### `tael watch`

| Flag | Description | Example |
|------|-------------|---------|
| `--last` | Summary window (default 1m) | `--last 30s` |
| `--service` | Filter to a single service | `--service api` |
| `--interval` | Poll interval in seconds (default 10) | `--interval 5` |

## Architecture

Server and client are the same `tael` binary — `tael serve` runs the
ingest/storage/API side; the other subcommands are the client.

```
┌──────────────────────────────┐
│         Data Sources         │
│  (OTel-instrumented apps)    │
└──────────┬───────────────────┘
           │ OTLP gRPC :4317 · Prometheus remote-write (HTTP)
           ▼
┌──────────────────────────────────────────────┐
│   tael serve                                   │
│                                                │
│  ┌──────────────────────────────────────────┐ │
│  │  OTLP receivers: traces · logs · metrics  │ │
│  │  (tonic gRPC + axum)                       │ │
│  └──────────────────┬───────────────────────┘ │
│                     ▼                          │
│  ┌──────────────────────────────────────────┐ │
│  │  tael-backend (default) — Store trait      │ │
│  │   WAL → LSM hot tier → Parquet cold tier   │ │
│  │   content-addressed blobs · Tantivy search │ │
│  │   (or --storage duckdb: embedded fallback) │ │
│  └──────────────────┬───────────────────────┘ │
│                     ▼                          │
│  ┌──────────────────────────────────────────┐ │
│  │  REST API (axum)  :7701                    │ │
│  └──────────────────────────────────────────┘ │
└──────────────────────┬─────────────────────────┘
                       │ HTTP
                       ▼
┌──────────────────────────────────────────────┐
│   tael <query|get|comment|live|summarize|…>    │
└──────────────────────────────────────────────┘
```

See [`docs/tael-backend-design.md`](docs/tael-backend-design.md) for the storage
engine and [`docs/tael-server-scaling-ha.md`](docs/tael-server-scaling-ha.md) for
the horizontal-scale / HA path.

## Project Structure

The `tael` binary is published as `tael-cli`, which embeds `tael-server` as a
library — so `cargo install tael-cli` is the whole stack.

```
├── tael-server/     # Library: OTLP ingestion, tiered storage, REST/gRPC API
│   └── src/
│       ├── lib.rs        # tael_server::run(ServerConfig)
│       ├── config.rs
│       ├── ingest/       # OTLP traces/logs/metrics + Prometheus remote-write
│       ├── storage/      # Store trait, models, query layer
│       │   ├── backend/  # tael-backend: wal, hot (LSM), cold (Parquet)
│       │   ├── blobs.rs   #   content-addressed payload store
│       │   ├── search.rs  #   Tantivy full-text index
│       │   └── duckdb_store.rs  # legacy --storage=duckdb backend
│       └── api/          # REST endpoints (axum)
├── tael-cli/        # The `tael` binary: serve + query/get/comment/live TUI
│   └── src/
│       ├── main.rs      # clap dispatch; `serve` → tael_server::run
│       ├── client.rs    # HTTP client to the server REST API
│       ├── tui.rs       # Interactive TUI (ratatui)
│       ├── output.rs    # JSON + table formatters
│       └── commands/    # Subcommand handlers
├── tael-test/       # Sample OTLP emitter for testing
├── docs/            # Storage-engine design, impl plan, scaling/HA
├── DESIGN.md        # Full design document
└── mise.toml        # Rust 1.87 toolchain
```

## Tech Stack

| Component | Choice | Why |
|-----------|--------|-----|
| Language | Rust | Fast, single binary, memory-safe |
| Storage | tael-backend | Tiered engine: WAL (walrus) + LSM hot tier (fjall) + Parquet cold tier (arrow/parquet) + content-addressed blobs + Tantivy search |
| Storage (fallback) | DuckDB | Embedded columnar DB, `--storage duckdb` |
| CLI | clap | Standard Rust CLI framework |
| API | axum | Async REST on tokio |
| gRPC | tonic | OTLP ingestion |
| TUI | ratatui | Terminal UI with waterfall visualization |
| OTel | opentelemetry-proto | Standard OTLP protobuf decoding |

## Configuration

The server (`tael serve`) is configured via flags or environment variables
(flags win):

| Variable | Default | Description |
|----------|---------|-------------|
| `TAEL_OTLP_GRPC_ADDR` | `127.0.0.1:4317` | OTLP gRPC listen address |
| `TAEL_REST_API_ADDR` | `127.0.0.1:7701` | REST API listen address |
| `TAEL_DATA_DIR` | `./data` | Telemetry data directory |
| `TAEL_STORAGE` | `tael-backend` | Storage backend (`tael-backend` or `duckdb`) |
| `TAEL_COLD_DIR` | `<data_dir>/cold` | Override the Parquet cold-tier location (e.g. an object-store mount) |
| `TAEL_HOT_TIER_HOURS` | `24` | Hot-tier window before data rolls to the cold tier |
| `TAEL_COMPACT_INTERVAL_SECS` | `3600` | Compaction / retention / blob-GC interval |
| `TAEL_TRACE_RETENTION_DAYS` | `365` | Span metadata retention in the cold tier |
| `RUST_LOG` | `info` | Log level |

## Development

```bash
# Prerequisites: Rust 1.87+ (or use mise)
mise install

# Build
cargo build

# Run server (alias for `cargo run --bin tael -- serve`)
./run-server.sh

# Send test data
cargo run --bin tael-test

# Run CLI
cargo run --bin tael -- query traces --format table
```

## Roadmap

See [DESIGN.md](DESIGN.md) for the full design document and milestone plan.

- [x] **M1**: OTLP trace ingestion, embedded storage, CLI queries, trace comments, TUI
- [x] **M2**: Metrics + logs ingestion, PromQL subset
- [x] **M3**: `tael summarize`, `tael anomalies`, `tael correlate`, `tael watch`
- [x] **tael-backend**: purpose-built tiered storage engine (WAL + LSM hot tier + Parquet cold tier + content-addressed blobs + full-text search), now the default — see [`docs/tael-backend-design.md`](docs/tael-backend-design.md)
- [ ] **M4**: object-store cold tier + horizontal scale / HA ([`docs/tael-server-scaling-ha.md`](docs/tael-server-scaling-ha.md)), MCP server, auth

## License

MIT
