<p align="center">
  <h1 align="center">tael</h1>
  <p align="center">AI-agent-native observability platform</p>
  <p align="center">
    <a href="#quickstart">Quickstart</a> •
    <a href="#features">Features</a> •
    <a href="#cli-reference">CLI Reference</a> •
    <a href="#architecture">Architecture</a> •
    <a href="DESIGN.md">Design Doc</a>
  </p>
</p>

---

**tael** is an observability platform built for AI agents. It ingests [OpenTelemetry](https://opentelemetry.io/) traces via standard OTLP gRPC, stores them in [DuckDB](https://duckdb.org/), and exposes a CLI-first interface that returns structured JSON — designed for agents like Claude Code, Devin, or custom autonomous systems to query, monitor, and annotate production telemetry programmatically.

No dashboards. No browsers. Just a single binary and structured data.

[![asciicast](https://asciinema.org/a/svewi9ncgeH52UFP.svg)](https://asciinema.org/a/svewi9ncgeH52UFP)

## Quickstart

```bash
# Build
cargo build --release

# Start the server (OTLP on :4317, REST API on :7701)
./target/release/tael-server

# In another terminal — send sample traces
./target/release/tael-test

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
Accepts traces from any OpenTelemetry-instrumented application via standard OTLP gRPC (port 4317). No proprietary SDKs or agents required.

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
  query traces    Search and filter traces
  get trace       Get a full trace by ID
  services        List known services and their health
  comment add     Add a comment to a trace
  comment list    List comments on a trace
  live            Interactive TUI trace feed
  server status   Check server health

Global Options:
  --format <json|table>   Output format (default: json)
  --server <URL>          Server address (default: http://127.0.0.1:7701)
```

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

## Architecture

```
┌──────────────────────────────┐
│         Data Sources         │
│  (OTel-instrumented apps)    │
└──────────┬───────────────────┘
           │ OTLP gRPC (:4317)
           ▼
┌──────────────────────────────┐
│      tael-server             │
│                              │
│  ┌────────────────────────┐  │
│  │  OTLP Trace Receiver   │  │
│  │  (tonic gRPC)          │  │
│  └──────────┬─────────────┘  │
│             ▼                │
│  ┌────────────────────────┐  │
│  │  DuckDB Storage        │  │
│  │  (embedded, columnar)  │  │
│  └──────────┬─────────────┘  │
│             ▼                │
│  ┌────────────────────────┐  │
│  │  REST API (axum)       │  │
│  │  :7701                 │  │
│  └────────────────────────┘  │
└──────────────────────────────┘
           │
           ▼
┌──────────────────────────────┐
│      tael (CLI)              │
│  query, get, comment, live   │
└──────────────────────────────┘
```

## Project Structure

```
├── tael-server/     # Backend: OTLP ingestion, DuckDB storage, REST API
│   └── src/
│       ├── main.rs
│       ├── config.rs
│       ├── ingest/     # OTLP gRPC trace receiver
│       ├── storage/    # DuckDB store, models, queries
│       └── api/        # REST endpoints (axum)
├── tael-cli/        # CLI: query, get, comment, live TUI
│   └── src/
│       ├── main.rs
│       ├── client.rs   # HTTP client to server REST API
│       ├── tui.rs      # Interactive TUI (ratatui)
│       ├── output.rs   # JSON + table formatters
│       └── commands/   # Subcommand handlers
├── tael-test/       # Sample OTLP trace emitter for testing
├── DESIGN.md        # Full design document
└── mise.toml        # Rust 1.87 toolchain
```

## Tech Stack

| Component | Choice | Why |
|-----------|--------|-----|
| Language | Rust | Fast, single binary, memory-safe |
| Storage | DuckDB | Embedded columnar DB, analytical query performance |
| CLI | clap | Standard Rust CLI framework |
| API | axum | Async REST on tokio |
| gRPC | tonic | OTLP trace ingestion |
| TUI | ratatui | Terminal UI with waterfall visualization |
| OTel | opentelemetry-proto | Standard OTLP protobuf decoding |

## Configuration

The server is configured via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `TAEL_OTLP_GRPC_ADDR` | `127.0.0.1:4317` | OTLP gRPC listen address |
| `TAEL_REST_API_ADDR` | `127.0.0.1:7701` | REST API listen address |
| `TAEL_DATA_DIR` | `./data` | DuckDB data directory |
| `RUST_LOG` | `info` | Log level |

## Development

```bash
# Prerequisites: Rust 1.87+ (or use mise)
mise install

# Build
cargo build

# Run server
./run-server.sh

# Send test data
cargo run --bin tael-test

# Run CLI
cargo run --bin tael -- query traces --format table
```

## Roadmap

See [DESIGN.md](DESIGN.md) for the full design document and milestone plan.

- [x] **M1**: OTLP trace ingestion, DuckDB storage, CLI queries, trace comments, TUI
- [ ] **M2**: Metrics + logs ingestion, PromQL subset
- [ ] **M3**: `tael summarize`, `tael anomalies`, `tael correlate`, `tael watch`
- [ ] **M4**: ClickHouse backend, MCP server, auth, packaging

## License

MIT
