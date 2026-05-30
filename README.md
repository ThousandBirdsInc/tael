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

One `tael` binary — server, CLI, TUI, and desktop GUI in one — with structured
data as the default interface.

<p align="center">
  <a href="https://asciinema.org/a/fJALiYb0pILGb18H">
    <img src="https://asciinema.org/a/fJALiYb0pILGb18H.svg" alt="tael asciinema demo" width="720" />
  </a>
</p>

## Installation

Supported on macOS (Intel + Apple Silicon) and Linux (x86_64 + aarch64).
Windows is not supported — a dependency in the WAL uses unix-only file I/O.

```bash
# Fastest — download a prebuilt `tael` binary (no compilation)
cargo binstall tael-cli
```

`cargo install tael-cli` also works, but compiles the desktop GUI from source,
so it can take several minutes and requires the native Tauri/WebKit build
dependencies for your platform. `cargo binstall` fetches a
prebuilt binary from the GitHub Release instead and finishes in seconds. Install
it once with `cargo install cargo-binstall` (or grab it from
[its releases](https://github.com/cargo-bins/cargo-binstall#installation)).

```bash
# Compiles from source — slower, but no extra tooling
cargo install tael-cli

# Optional legacy DuckDB storage backend
cargo install tael-cli --features duckdb
```

Or build from source:

```bash
cargo build --release
```

### Docker

A prebuilt, multi-arch (amd64 + arm64) image is published to GHCR on every
release, so there is no source build:

```bash
docker run --rm \
  -p 7701:7701 -p 4317:4317 \
  -v tael-data:/data \
  ghcr.io/thousandbirdsinc/tael:latest
```

That starts `tael serve` with OTLP gRPC on `:4317` and the REST API on `:7701`,
persisting telemetry to the `tael-data` volume. Point your app's OTLP exporter
at `http://localhost:4317` and query from the host with a locally installed
`tael`, or run the CLI inside the container:

```bash
docker exec <container> tael --format json services
```

To embed tael in your own image, use it as a base — the `tael` binary is on
`PATH` and `serve` is the default command:

```dockerfile
FROM ghcr.io/thousandbirdsinc/tael:latest
# your OTLP-emitting app alongside it, or override CMD to run a query
```

The image is server/CLI only; the desktop GUI (`tael gui`) is desktop-only and
is compiled out of the container build.

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

# Desktop GUI
tael gui
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

# Cross-signal queries
tael query logs --severity error --last 1h
tael query metrics --query 'sum by (service) (http_requests)'
tael query sql "SELECT service, COUNT(*) AS errors FROM spans WHERE status = 'error' GROUP BY service"
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

### Trace-Native Evals and Reliability Loop
Tael evals are designed around the full execution trace, not a narrow result
row. Eval runs, scores, comments, large artifacts, and live progress reuse the
same spans, metrics, comments, blobs, SQL, and TUI that production debugging
uses.

The roadmap extends this into a floor-raising loop for agent reliability:

```text
production trace -> failure classification -> issue/signal -> golden case -> fix -> compare -> monitor
```

Commands include `tael issue` for recurring failure patterns, `tael signal` for
long-running behavior monitoring, `tael eval case add --from-trace` for
promoting production failures into golden regression cases, `tael eval suite
inspect` for suite hygiene, and `tael experiment compare` for validating model,
prompt, tool, retrieval, or guardrail changes against production outcomes.

The initial implementation is intentionally comment-backed: issues, signal
definitions, eval case provenance, and self diagnostics are structured trace
comments, so they stay attached to the trace that motivated them.

```bash
# Classify a representative production failure
tael issue create --from-trace <trace-id> \
  --failure-mode tool_error --impact high \
  --summary "search tool timed out before answer synthesis"
tael issue list --format table
tael issue examples <issue-id>

# Promote the failure into a regression case and inspect suite hygiene
tael eval case add --from-trace <trace-id> --suite support-agent \
  --case-id search-timeout-001 --failure-mode tool_error \
  --source-issue-id <issue-id> --critical-path \
  --expected-behavior "Retries or degrades gracefully without hallucinating"
tael eval case link --case-id search-timeout-001 --issue-id <issue-id>
tael eval suite inspect support-agent --format table

# Run and score trace-native evals
tael eval run cases.jsonl --suite support-agent \
  --cmd './run_case.sh {case_id}' --code-version "$(git rev-parse --short HEAD)"
tael eval score <run-id> scores.jsonl
tael eval report <run-id> --format table
tael eval compare <run-id> <baseline-run-id> --format table

# Track long-running reliability signals and experiment variants
tael signal create --from-trace <trace-id> --name context_loss \
  --failure-mode context_loss --summary "agent lost required source context"
tael signal trend context_loss --format table
tael experiment compare checkout-prompt-v2 --signal context_loss --last 24h

# Record untrusted agent self diagnostics for later review
tael diagnose report --trace-id <trace-id> --category missing_context \
  --severity medium --confidence low --summary "could not find policy source"
tael diagnose list --format table
```

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
  eval            Collect, score, report, and compare trace-native evals
  issue           Classify production failures into recurring issues
  signal          Define and inspect long-running reliability signals
  experiment      Compare production experiment variants
  diagnose        Record and list untrusted agent self diagnostics
  skill install   Install the tael skill into Claude Code
  server status   Check server health

Global Options:
  --format <json|table>   Output format (default: json)
  --server <URL>          Server address (default: http://127.0.0.1:7701)
  --port-rest <N>         Shorthand for --server http://127.0.0.1:<N>;
                          for `serve`, sets the REST API listen port.
                          Conflicts with --server.
  --port-otel <N>         (serve only) OTLP gRPC ingest listen port on
                          127.0.0.1. Ignored by client commands.
```

### `tael serve`

Runs the server in the same binary. Flags fall back to the matching env var
(see [Configuration](#configuration)), then to the defaults.

| Flag | Description | Default |
|------|-------------|---------|
| `--otlp-grpc-addr` | OTLP gRPC listen address | `127.0.0.1:4317` |
| `--rest-api-addr` | REST API listen address | `127.0.0.1:7701` |
| `--data-dir` | Telemetry data directory | `~/.tael/data` |
| `--wal-dir` | Write-ahead log directory | `~/.tael/wal_files` |
| `--storage` | Storage backend. `duckdb` requires installing with `--features duckdb` | `tael-backend` |

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
| `--attribute` | Exact span-attribute match, repeatable and ANDed | `--attribute http.status_code=500` |
| `--text` | Full-text search over LLM prompt/completion payloads in `tael-backend` storage | `--text "rate limit"` |

### `tael query logs`

| Flag | Description | Example |
|------|-------------|---------|
| `--service` | Filter by service name | `--service api` |
| `--severity` | Filter by severity (`trace`, `debug`, `info`, `warn`, `error`, `fatal`) | `--severity error` |
| `--body-contains` | Substring search over log body text | `--body-contains timeout` |
| `--trace-id` | Exact trace ID match | `--trace-id a1b2c3...` |
| `--last` | Time window | `--last 1h` |
| `--limit` | Max results (default 100) | `--limit 50` |

### `tael query metrics`

| Flag | Description | Example |
|------|-------------|---------|
| `--service` | Filter by service name; ignored when `--query` is set | `--service api` |
| `--name` | Filter by metric name; ignored when `--query` is set | `--name http_requests` |
| `--type` | Filter by metric type (`gauge`, `sum`, `histogram`, `summary`) | `--type gauge` |
| `--query` | PromQL subset expression | `--query 'rate(http_requests[5m])'` |
| `--last` | Time window or PromQL selector lookback | `--last 5m` |
| `--limit` | Max results in filter mode (default 500) | `--limit 1000` |

PromQL support is intentionally small: bare selectors, `{label="value"}`,
`rate(metric[5m])`, and `sum|avg|min|max|count` with optional `by (...)`.
Binary operators, regex matchers, `histogram_quantile`, subqueries, offset, and
range queries are not supported.

### `tael query sql`

Runs read-only SQL over `spans`, `logs`, `metrics`, and `trace_comments`.
Only `SELECT`/`WITH` statements are accepted.

```bash
tael query sql "SELECT service, COUNT(*) AS n FROM spans GROUP BY service ORDER BY n DESC"
```

### `tael live`

| Flag | Description | Example |
|------|-------------|---------|
| `--service` | Filter live trace feed by service | `--service api` |
| `--status` | Filter live trace feed by status | `--status error` |
| `--interval` | Poll interval in seconds (default 2) | `--interval 1` |
| `--evals` | Open the eval progress view | `--evals` |
| `--eval-run` | Open a specific eval run in the eval progress view | `--eval-run run_20260528_120000` |

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

### `tael eval`

| Command | Description |
|---------|-------------|
| `eval run <cases.jsonl> --suite <suite> --cmd <cmd>` | Run a shell command once per JSONL case with `TAEL_EVAL_*` env vars and runner spans |
| `eval score <run-id> <scores.jsonl>` | Ingest JSONL score records as `tael_eval_score` metric points |
| `eval runs` | List recent eval runs |
| `eval status <run-id>` | Show one eval run summary |
| `eval cases <run-id>` | List cases in a run |
| `eval scores <run-id>` | List raw scores in a run |
| `eval report <run-id>` | Render status and cases together |
| `eval compare <run-id> <baseline-run-id>` | Compare score metrics against a baseline run |
| `eval case add --from-trace <trace> --suite <suite> --case-id <id>` | Promote a production trace into a golden case comment |
| `eval case link --case-id <id> --issue-id <issue>` | Link an eval case to a recurring issue |
| `eval suite inspect <suite>` | Inspect case provenance, expected behavior coverage, critical-path count, and duplicate failure modes |

`eval run` templates support `{case_id}`, `{case_index}`, `{run_id}`, and
`{suite_id}`. Child commands receive `TAEL_EVAL_SUITE_ID`,
`TAEL_EVAL_RUN_ID`, `TAEL_EVAL_CASE_ID`, `TAEL_EVAL_CASE_INDEX`,
`TAEL_EVAL_CASE_COUNT`, `TAEL_EVAL_TRACE_ID`, `TAEL_EVAL_SPAN_ID`, and
`OTEL_EXPORTER_OTLP_ENDPOINT`.

### Reliability Loop Commands

| Command | Description |
|---------|-------------|
| `issue create --from-trace <trace> --failure-mode <mode> --impact <level> --summary <text>` | Create a structured recurring-issue comment from a representative trace |
| `issue list` | List known recurring issues |
| `issue examples <issue-id>` | List comments and cases linked to an issue |
| `signal create --from-trace <trace> --name <name>` | Define a long-running signal from a trace |
| `signal trend <name>` | Count matching signal, failure-review, and self-diagnostic comments by day |
| `experiment compare <experiment-id>` | Compare variants tagged with `tael.experiment.id` and `tael.experiment.variant` span attributes |
| `diagnose report --trace-id <trace> --category <category> --severity <level> --summary <text>` | Record an untrusted agent self diagnostic as a trace comment |
| `diagnose list` | List self diagnostics |

The reliability-loop commands are deliberately comment-backed. They scan
structured JSON trace comments rather than requiring a separate issues or eval
database, which keeps provenance attached to the original trace.

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
│  │   (optional --features duckdb fallback)     │ │
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

The `tael` binary is published as `tael-cli`, which embeds `tael-server` and the
desktop GUI as libraries — so `cargo install tael-cli` is the whole stack.

Use `tael_server::run(config)` for a user-facing server process. In-process
integrations that must preserve one-shot JSON output or TUI control of the
terminal should use `tael_server::run_embedded(config)` or
`run_with_options(config, ServerRunOptions::quiet())`; quiet mode skips Tael's
startup banner and default tracing subscriber setup.

```
├── tael-server/     # Library: OTLP ingestion, tiered storage, REST/gRPC API
│   └── src/
│       ├── lib.rs        # tael_server::run / run_embedded
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
├── tael-gui/        # Tauri desktop GUI launched by `tael gui`
│   ├── src/         # TypeScript frontend
│   └── src-tauri/   # Rust Tauri shell and packaged frontend assets
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
| Storage (fallback) | DuckDB | Optional embedded columnar DB, `--features duckdb` + `--storage duckdb` |
| CLI | clap | Standard Rust CLI framework |
| GUI | Tauri | Desktop app embedded in the installed `tael` binary |
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
| `TAEL_DATA_DIR` | `~/.tael/data` | Telemetry data directory |
| `TAEL_WAL_DIR` | `~/.tael/wal_files` | Write-ahead log directory |
| `TAEL_STORAGE` | `tael-backend` | Storage backend. `duckdb` requires a build with `--features duckdb` |
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
- [x] **M3.5**: comment-backed floor-raising reliability loop: issues, signals, trace-to-golden-case promotion, suite hygiene, production experiment comparison, and self-diagnostic conventions — see [`docs/tael-evals-design.md`](docs/tael-evals-design.md)
- [x] **tael-backend**: purpose-built tiered storage engine (WAL + LSM hot tier + Parquet cold tier + content-addressed blobs + full-text search), now the default — see [`docs/tael-backend-design.md`](docs/tael-backend-design.md)
- [ ] **M4**: object-store cold tier + horizontal scale / HA ([`docs/tael-server-scaling-ha.md`](docs/tael-server-scaling-ha.md)), MCP server, auth

## License

MIT
