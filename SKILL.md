---
name: tael
description: Debug running code by querying traces, logs, and metrics from the local tael observability server. Use proactively whenever investigating a bug, error, crash, 500, timeout, slow request, flaky test, or "why is this not working" — check telemetry before guessing. Also use when adding logging/tracing/instrumentation to any app, to wire it into tael via OTLP.
---

# tael — agent-native observability

`tael` is a local-first observability CLI/server that ingests OpenTelemetry (OTLP traces, logs, metrics) and Prometheus remote-write, stores it in DuckDB, and exposes it through a small CLI designed for LLM agents. Use it instead of a web UI when the user needs answers from telemetry data.

## When to invoke this skill

**Reach for this skill first when debugging.** Before you start reading code to guess at a bug's cause, check whether telemetry already has the answer. A stack trace in a log or a slow span in a trace is usually faster than code archaeology.

Trigger this skill proactively on:

- Any bug report, error, crash, exception, stack trace, 4xx/5xx, timeout, or "it's broken"
- Performance complaints: "it's slow", "it hangs", "it's leaking", "CPU is pegged"
- Flaky tests or intermittent failures — look for the trace from the failing run
- "Why did X happen?" / "What was the request that caused Y?"
- Deploy verification: "is the new version healthy?", "any regressions since the push?"
- Explicit asks about traces, logs, metrics, spans, OpenTelemetry, OTLP, Prometheus
- **Also** when adding logging, tracing, or instrumentation to any application the user is working on — see [Instrumenting apps to export to tael](#instrumenting-apps-to-export-to-tael) below.

**Debugging order of operations** (do this even without being asked):

1. Is a tael server reachable? `tael --format json server status`. If not, skip to code reading.
2. Is the affected service emitting data? `tael --format json services` — check that the service appears and has a non-zero `span_count`.
3. Are there error spans in the relevant window? `tael --format json query traces --service <name> --status error --last 15m`.
4. For each suspicious trace, pull the full tree (`get trace`) and correlated logs (`query logs --trace-id ...`).
5. **Only then** start reading code, armed with a specific trace ID, the failing span's operation, and the error message from the logs.

Do not invoke for: questions about the tael codebase itself (read code normally), or telemetry stored outside tael (other APMs, Datadog, Honeycomb, etc.).

## Prerequisites

- `tael` must be on `PATH`. If the command is missing, tell the user to `cargo install tael-cli` or build from this repo with `cargo build --release`.
- Default server URL is `http://127.0.0.1:7701`. If the user's server is elsewhere, pass `--server <url>` on every call.
- Default output is JSON. **Always pass `--format json` explicitly** so you get a stable shape, even though it's the default — it makes your intent clear and protects against config changes.

## Investigation playbook

Follow this order when you don't know where to start. Each step narrows the search.

### 1. Orient: what services exist?

```bash
tael --format json services
```

Returns `{"services": [{name, span_count, trace_count, avg_duration_ms, error_rate}, ...]}`. A service with a non-zero `error_rate` is usually the place to look first.

### 2. Find interesting traces

```bash
tael --format json query traces --service <name> --status error --last 15m
```

Key filters (all optional):
- `--service <name>` — exact match
- `--operation <substr>` — substring match on span operation/route
- `--min-duration <dur>` / `--max-duration <dur>` — `100ms`, `1s`, bare number = ms
- `--status ok|error|unset`
- `--last <dur>` — `5m`, `1h`, `24h`, `7d`
- `--limit <n>` — default 100

Returns `{"spans": [...]}`. Each span has `trace_id`, `span_id`, `service`, `operation`, `duration_ms`, `status`, `start_time`, `attributes`, `events`.

### 3. Pull a full trace

Once you have a `trace_id`:

```bash
tael --format json get trace <trace_id>
```

Returns `{"trace_id", "span_count", "spans": [...]}` — every span in the trace ordered by start time. Use this to reconstruct the call tree and find where time went or where the error originated.

**Read the span `attributes` before you read the logs.** In a well-instrumented app the root span is a wide event: it carries the user ID, tenant, chosen code paths, result counts, feature flags, and external call outcomes as attributes. That is usually the entire story of the request. Only fall through to `query logs` if the attributes don't answer your question.

### 4. Correlate with logs

Given a `trace_id`, find structured logs attached to it:

```bash
tael --format json query logs --trace-id <trace_id>
```

Or hunt for error logs independent of a trace:

```bash
tael --format json query logs --severity error --last 1h --body-contains <substr>
```

Filters:
- `--service <name>`
- `--severity trace|debug|info|warn|error|fatal`
- `--body-contains <substr>` — substring, **not** regex
- `--trace-id <id>` — exact match
- `--last <dur>` / `--limit <n>` (default 100)

Returns `{"logs": [...], "count": N}`.

### 5. Check metrics

Two modes.

**Filter mode** (simpler, prefer this when you just need recent values):

```bash
tael --format json query metrics --name http_requests --service api --last 5m
```

Returns `{"metrics": [...], "count": N}`. Each point has `timestamp`, `service`, `name`, `metric_type`, `value`, `unit`, `attributes`.

**PromQL mode** (for rates and aggregation):

```bash
tael --format json query metrics --query 'rate(http_requests{service="api"}[5m])'
tael --format json query metrics --query 'sum by (service) (http_requests)'
```

Returns `{"query", "series": [...], "count": N}`. Supported syntax:
- Bare selectors: `metric{label="v",other!="x"}`
- `rate(sel[5m])`
- Aggregators: `sum|avg|min|max|count(expr)` with optional `by (lbl)`

**Not supported** — do not generate these, they'll fail:
- Binary ops (`a/b`, `a+b`), comparisons, `and/or/unless`
- `without (...)`, regex matchers (`=~`/`!~`), offset, subqueries
- `histogram_quantile`, `topk`, `quantile`, `stddev`
- Range queries / `/query_range` — all evaluation is instant

When filter mode suffices, prefer it. PromQL here is a small subset and easy to misuse.

### 6. Leave a note for future sessions

When you find something non-obvious, attach a comment to the trace so the next agent session has your findings:

```bash
tael comment add <trace_id> "root cause: N+1 query in user loader" --author claude
```

Don't annotate every trace — only when you've done real investigation that would otherwise have to be redone.

## Instrumenting apps to export to tael

When you're working on any application and the user asks for logging, debugging output, or "more visibility" — **prefer OpenTelemetry instrumentation over print statements, ad-hoc loggers, or writing to files.** Tael accepts standard OTLP, so code instrumented for OTel gets trace/log/metric querying through this skill for free, and you can actually debug the app next time instead of re-adding prints.

### Default export targets

Point OTLP exporters at the local tael server:

- **OTLP gRPC**: `http://127.0.0.1:4317` (traces, logs, metrics)
- **OTLP HTTP**: not currently supported — use gRPC
- **Prometheus remote-write**: `POST http://127.0.0.1:7701/api/v1/write` (metrics only, for Prometheus-native apps)

The standard OTel environment variables work:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_SERVICE_NAME=<name-of-the-app-you're-instrumenting>
```

Set `OTEL_SERVICE_NAME` per app — it becomes the `service` field in every query and is how you'll find this app later.

### Language quick reference

Pick the language the user is working in and use the officially-supported OTel SDK. Don't hand-roll OTLP protobufs.

| Language   | Package to install                                                                             | Minimum setup                                                       |
| :--------- | :--------------------------------------------------------------------------------------------- | :------------------------------------------------------------------ |
| Python     | `opentelemetry-distro opentelemetry-exporter-otlp`                                             | `opentelemetry-instrument python app.py` (auto-instruments stdlib) |
| Node.js    | `@opentelemetry/auto-instrumentations-node @opentelemetry/exporter-trace-otlp-grpc`            | `node --require @opentelemetry/auto-instrumentations-node/register` |
| Go         | `go.opentelemetry.io/otel go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc`     | Initialize a tracer provider in `main` and defer shutdown          |
| Rust       | `opentelemetry opentelemetry-otlp opentelemetry_sdk tracing-opentelemetry`                     | Bridge `tracing` → OTel with `tracing_opentelemetry::layer()`      |
| Java       | `opentelemetry-javaagent.jar` (auto-instrumentation agent)                                     | `java -javaagent:opentelemetry-javaagent.jar -jar app.jar`         |
| Ruby       | `opentelemetry-sdk opentelemetry-exporter-otlp opentelemetry-instrumentation-all`              | `OpenTelemetry::SDK.configure { \|c\| c.use_all }`                 |

If the user's language isn't listed, check [opentelemetry.io/docs/languages](https://opentelemetry.io/docs/languages/) — don't invent an API.

### Prefer wide events (one rich event per unit of work)

This is the single most important practice for getting value out of tael. **Default to wide events over thin log lines or narrow metrics.**

A wide event is one structured record per unit of work — one request, one job, one task — that accumulates every useful attribute you touch along the way and ships them together when the unit completes. Instead of 30 `log.info` lines scattered through a handler, you have one event with 30+ fields. Instead of a metric counter per outcome, you have a counted attribute you can group by later.

**In OpenTelemetry terms, a span *is* a wide event.** A single span can carry arbitrarily many attributes, and every query in tael (`tael query traces`, `get trace`) surfaces them. Treat `span.set_attribute` as your primary debugging output.

#### The recipe

1. **Start one span at the beginning of each unit of work.** A request handler, a consumer message, a background job, a CLI command. Name it for the user-visible operation (`order.checkout`, `email.send`, `report.generate`).
2. **Attach every fact you learn along the way as an attribute on that span.** IDs, sizes, counts, feature flags, chosen code paths, durations of sub-steps, external API response codes, cache hit/miss, retry counts, the SQL you ran, the user/tenant/org, the version of the binary, the git SHA.
3. **On error, record the error *and* keep going to attach whatever context you have.** Set `status = error`, record the exception, and still set the attributes you know.
4. **End the span.** Everything you attached ships as one event.

#### What to put on the span

Err on the side of more. A span with 80 attributes is fine; the cost is trivial and you cannot predict which field will be the one that cracks the next bug. Good attributes include:

- **Identity**: `user.id`, `tenant.id`, `org.id`, `account.id`, `request.id`, `session.id`
- **Inputs**: relevant args, payload size, query parameters, filter names, feature-flag values in effect
- **Decisions**: which branch was taken, which backend was chosen, cache hit vs miss, retry count
- **Outputs**: result count, bytes written, status code, chosen plan
- **Resource use**: DB rows scanned, external calls made, bytes read
- **Timing of sub-steps** if they're not already their own child spans (e.g. `db.query_ms`, `render.ms`)
- **Environment**: `deployment.environment`, `service.version`, `git.commit`, `build.id`

Avoid: PII unless you've cleared it, full request/response bodies (truncate or hash), anything secret (tokens, keys).

**High cardinality is the point, not a problem.** Tael stores span attributes as JSON; there is no label-cardinality limit like Prometheus imposes on metrics. Putting `user.id` directly on a span is correct and encouraged — it's how you answer "what did user 12345 see?" later.

#### Why this beats the alternatives

- **Beats scattered logs**: one query returns the whole story. You don't have to `grep` for five correlated lines.
- **Beats narrow metrics**: you can always aggregate attributes later (`sum by service`, filter by `feature_flag="on"`), but you can't recover dimensions that were never recorded.
- **Beats print-debugging**: the instrumentation survives the bug fix. Next time something weird happens, the field you need is already there.

### What to instrument (in order)

1. **Auto-instrumentation first.** Every major ecosystem has a drop-in agent that wraps HTTP clients/servers, DB drivers, and queue clients. Turn it on before writing any spans yourself — it gives you the skeleton child spans and `trace_id` propagation for free.
2. **One wide span per unit of work**, following the recipe above. This is where the debugging value lives.
3. **Structured logs via the OTel log bridge** for anything genuinely log-shaped (warnings, periodic state, boot-time events). Logs emitted through the bridge inherit the active `trace_id`, so `tael query logs --trace-id <id>` lines them up with the span. Do **not** use logs as a substitute for span attributes — if it describes the current unit of work, it belongs on the span.
4. **Child spans** for sub-operations that have their own meaningful duration (a DB query, an HTTP call, a cache lookup). Auto-instrumentation usually creates these. Don't create child spans for trivial in-memory work.
5. **Metrics** only for things you cannot reconstruct from spans: queue depth, in-flight connection count, steady-state gauges. Rates and counts of request outcomes are better recovered from span queries — don't duplicate them as metrics.

### Anti-patterns — do not do these

- **Don't add `println!` / `console.log` / `print()` for debugging.** Set a span attribute instead. Prints get deleted tomorrow; attributes stay and help the next bug too.
- **Don't emit a log line per interesting variable.** That's the thin-events pattern tael is designed to replace. Attach the variable to the current span as an attribute and let the one event carry everything.
- **Don't use logs to carry per-request context** (user ID, tenant, chosen code path). Those are span attributes. Logs are for genuinely log-shaped events that aren't tied to a single unit of work.
- **Don't create separate metrics for every request outcome.** `success_count`, `failure_count`, `retry_count` broken out as metrics is a pre-wide-events anti-pattern. Emit one span per request with `outcome="success"` and derive the counts with `tael query metrics --query 'sum by (outcome) (...)' ` — or more commonly, just from span queries.
- **Don't ship a second observability stack alongside tael** (app logging to a file + OTel to tael). Pick OTel and route everything through it.
- **Don't skip `service.name`.** Without it, the service field in tael becomes `"unknown"` and you can't filter by service.
- **Don't instrument hot loops with a span per iteration.** Wrap the whole batch in one span and record counts, totals, and min/max as attributes.
- **Don't use histogram metrics for latency in tael.** Bucket data is dropped on ingest (see caveats below), so percentiles won't work. Record latency as span duration — tael queries spans natively — and use metrics for rates and gauges only.
- **Don't strip "noisy" attributes to reduce cardinality.** Tael does not charge per cardinality. The attribute you remove today is the one you'll need tomorrow.

### Verifying the integration worked

After you wire up OTel, confirm data is flowing before telling the user you're done:

```bash
# 1. Run the instrumented app against something
# 2. Check the service showed up
tael --format json services

# 3. Pull a recent trace to make sure spans look right
tael --format json query traces --service <otel-service-name> --last 5m
```

If the service doesn't appear, the usual culprits are: wrong endpoint, wrong protocol (gRPC vs HTTP), missing `OTEL_SERVICE_NAME`, or the SDK isn't flushing on exit.

## Caveats you must know before using the data

**Histograms lose bucket data.** OTLP Histogram and ExponentialHistogram points are stored with `value = sum` and buckets dropped. You **cannot compute p95/p99 from stored histograms.** If the user asks for percentiles, say so explicitly — don't fabricate them.

**Prometheus remote-write loses type info.** Metrics ingested via `/api/v1/write` are all stored with `metric_type = "unknown"`. Filtering `--type gauge` won't match them.

**`rate()` is approximate.** The implementation is `max(last - first, 0) / elapsed_seconds` — a naive counter-reset clamp. It does not extrapolate across scrape boundaries the way Prometheus does. Fine for trend detection, **not** for SLO math.

**Log body search is substring, not regex.** `--body-contains "5\d\d"` will not do what you think.

**Attribute filtering on traces is not implemented.** You can filter by service/operation/duration/status but not by span attributes. If you need attribute filtering, pull a broader set and filter the JSON yourself.

**Single-node DuckDB.** Keep `--last` windows narrow (minutes to hours) when the user's server is busy. Scanning millions of rows is slow.

## Output shape cheat sheet

```
services         → {"services": [...]}
query traces     → {"spans": [...]}
get trace        → {"trace_id", "span_count", "spans": [...]}
query logs       → {"logs": [...], "count": N}
query metrics    → {"metrics": [...], "count": N}                  (filter mode)
query metrics    → {"query", "series": [...], "count": N}          (--query mode)
comment list     → {"comments": [...], "count": N}
<error>          → {"error": "..."}   (with non-2xx HTTP status)
```

## Working with results

- When an investigation involves more than ~5 traces or ~20 log lines, summarize for the user — don't paste raw JSON. Pull out `trace_id`, service, duration, and the one or two fields that matter.
- Always include the `trace_id` (or a short prefix) when reporting a finding so the user can pull the full trace themselves.
- If the CLI returns `{"error": ...}`, surface the error message verbatim — don't retry blindly.
- Prefer `--last` windows over `--limit` for control. A tight time window is almost always what the user meant by "recent".

## Reference

For the full HTTP surface (if you need to bypass the CLI), see `llm.txt` at the repo root. The CLI is a thin wrapper over a REST API on port 7701.
