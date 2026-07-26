---
name: tael
description: Debug running code by querying traces, logs, and metrics from the local tael observability server. Use proactively whenever investigating a bug, error, crash, 500, timeout, slow request, flaky test, or "why is this not working" — check telemetry before guessing. Also use when adding logging/tracing/instrumentation to any app, to wire it into tael via OTLP.
---

# tael — agent-native observability

`tael` is a local-first observability tool — server and client in one `tael` binary — that ingests OpenTelemetry (OTLP traces, logs, metrics) and Prometheus remote-write, stores it in a purpose-built tiered engine (hot LSM tier + Parquet cold tier; a DuckDB backend is available via `--storage duckdb`), and exposes it through a small CLI designed for LLM agents. Use it instead of a web UI when the user needs answers from telemetry data.

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
2. What's unhealthy right now? `tael --format json summarize --last 1h` and `tael --format json anomalies --last 5m --baseline 30m` — use these to pick a service and a failure mode before drilling in.
3. Is the affected service emitting data? `tael --format json services` — check that the service appears and has a non-zero `span_count`.
4. Are there error spans in the relevant window? `tael --format json query traces --service <name> --status error --last 15m`.
5. For each suspicious trace, pull spans + logs + metrics in one shot: `tael --format json correlate --trace <trace_id>`.
6. **Only then** start reading code, armed with a specific trace ID, the failing span's operation, and the error message from the logs.
7. If the failure is recurring or should become a regression case, use the reliability loop commands in this skill (`issue`, `signal`, `eval case`, `experiment`, `diagnose`) so the finding stays attached to the originating trace.

Do not invoke for: questions about the tael codebase itself (read code normally), or telemetry stored outside tael (other APMs, Datadog, Honeycomb, etc.).

## Prerequisites

- `tael` must be on `PATH`. If the command is missing, tell the user to `cargo install tael-cli` (one binary, server included) or build from this repo with `cargo build --release`.
- The server runs from the same binary: `tael serve` starts OTLP ingest (gRPC :4317) and the REST API (:7701). If `server status` fails, the user likely hasn't started it.
- Default server URL is `http://127.0.0.1:7701`. If the user's server is elsewhere, pass `--server <url>` on every call (or `--port-rest <N>` when it's still on localhost, just a different port).
- Default output is JSON. **Always pass `--format json` explicitly** so you get a stable shape, even though it's the default — it makes your intent clear and protects against config changes.

## Investigation playbook

Follow this order when you don't know where to start. Each step narrows the search.

### 0. Snapshot: what's unhealthy right now?

Before drilling in, grab a system-wide digest. Two commands cover this:

```bash
# Aggregated health over a window: spans/errors/p50-p99, top services,
# top error operations, log severity breakdown, metric volume.
tael --format json summarize --last 1h

# Services whose error rate or p95 regressed vs a baseline window.
# Default baseline is 6× the current window.
tael --format json anomalies --last 5m --baseline 30m
```

`summarize` returns `{window_seconds, traces, top_services, top_error_operations, logs, metrics}` — read `top_error_operations` first, then `top_services` sorted by span count. `anomalies` returns `{anomalies: [{service, kind, severity, current, baseline, delta, description}]}` where `kind` is `error_rate` or `latency_p95` and `severity` is `info|warning|critical`. If `anomalies` is empty at reasonable thresholds, jump straight to step 1.

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
- `--attribute key=value` — exact span-attribute match; repeatable and ANDed (e.g. `--attribute http.method=GET --attribute http.status_code=500`)
- `--text <query>` — full-text search over LLM prompt/completion payloads (e.g. `--text "rate limit"`); tael-backend storage only
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

Given a `trace_id`, the one-shot path is `tael correlate` — it returns the trace, every log tagged with that `trace_id`, and metrics from the touched services inside the trace's time window, in a single response:

```bash
tael --format json correlate --trace <trace_id>
```

Returns `{trace_id, span_count, services, start_time, end_time, duration_ms, error_count, logs: [...], metrics: [...]}`. Prefer this over three separate queries when you already have a trace ID in hand.

If you only need the logs (e.g. for keyword grep), the narrow form still works:

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
- `histogram_quantile(0.95, metric{...})`, optionally `... by (lbl)`

Note `histogram_quantile` takes the **metric selector directly**, not a fan of
`le`-labelled bucket series as in Prometheus — tael keeps each point's whole
bucket layout on the point. Dotted OTel metric names work as written.

**Not supported** — do not generate these, they'll fail:
- Binary ops (`a/b`, `a+b`), comparisons, `and/or/unless`
- `without (...)`, regex matchers (`=~`/`!~`), offset, subqueries
- `topk`, `bottomk`, `quantile`, `stddev`
- Range queries / `/query_range` — all evaluation is instant

When filter mode suffices, prefer it. PromQL here is a small subset and easy to misuse.

### 5b. Map the system, or compare two windows

```bash
tael --format json topology --last 1h
tael --format json diff --last 10m --baseline 6h --service api
```

`topology` reconstructs the service graph from span parent/child edges — use it
when you don't know what calls what, or to find which downstream dependency an
error rate is coming from. `diff` reports every summary metric's current,
baseline, delta, and ratio with no threshold applied; `anomalies` is the same
comparison with an opinion attached. Reach for `diff` when investigating a
*specific* change ("did the deploy at 14:00 do this"), and `anomalies` when
asking "is anything wrong".

Before querying an unfamiliar metric, describe it:

```bash
tael --format json get metric http.server.duration --last 24h
```

That reports its type, unit, label keys, and whether the points retained
histogram buckets — i.e. whether `histogram_quantile` will work on it.

### 5c. Block until something happens, or alert on it

```bash
# Wait for a condition, then exit 6. This is how to watch a deploy.
tael watch --last 1m --interval 10 --exit-on 'error_rate>0.05' --exit-on 'p95_ms>2x'

# Or make it standing, delivered to a webhook or command.
tael alert create --name high-errors \
  --query 'tael:span_error_rate{service="api"} > 0.05' --for 5m \
  --sink exec='./page.sh'
tael alerts --follow
```

Prefer `watch --exit-on` for a bounded wait inside one task, and an alert rule
for a standing condition. Thresholds can be absolute (`error_rate>0.05`) or
relative to the first sample (`p95_ms>2x`) — use the relative form when you're
starting mid-incident and don't know what healthy looks like.

These span-derived series need no metric instrumentation, each labelled by
`service` with a fleet-wide aggregate under `service="tael"`:
`tael:span_error_rate`, `tael:span_p95_ms`, `tael:span_p99_ms`,
`tael:span_count`, `tael:span_error_count`.

### 5d. Score production traffic continuously

```bash
tael score rule create --name faithfulness --sample 0.05 \
  --match service=agent-api --cmd ./judge.sh
tael score rule list
```

Samples matching traces and runs a scorer against each, writing
`tael_eval_score` points tagged `source=online`. The scorer contract matches
`tael eval run`, so the same script grades golden cases offline and production
traffic online. Check `failures` and `last_error` in `score rule list` — a
broken judge shows up there, not as missing data.

### 6. Watch an ongoing change

When the user is mid-deploy, mid-migration, or otherwise wants to know whether something is getting worse over the next few minutes, use `watch`. It polls `summarize` on an interval and prints signed deltas (span count, error count, error rate, p95, log errors, metric volume) per tick:

```bash
tael --format table watch --last 1m --interval 10
tael --format table watch --last 30s --interval 5 --service api-gateway
```

Use JSON output if you want to consume ticks programmatically. There's no built-in `--exit-on` yet — to turn a watch into an alert, read ticks until a threshold is crossed, then exit.

### 7. Leave a note for future sessions

When you find something non-obvious, attach a comment to the trace so the next agent session has your findings:

```bash
tael comment add <trace_id> "root cause: N+1 query in user loader" --author claude
```

Don't annotate every trace — only when you've done real investigation that would otherwise have to be redone.

## Reliability loop: issues, signals, golden cases, experiments

Use these commands after you have a concrete trace ID and a defensible failure story. They are comment-backed: each command writes or reads structured JSON in `trace_comments`, so provenance stays tied to the trace. Do not create issues or cases from speculation.

### Classify a recurring issue
When a trace represents a user-visible or recurring failure:

```bash
tael --format json issue create \
  --from-trace <trace_id> \
  --failure-mode <short_mode> \
  --impact low|medium|high|critical \
  --summary "<one sentence>" \
  --last-successful-step "<optional>" \
  --first-failure "<optional>"
```

Then inspect the inventory:
```bash
tael --format json issue list
tael --format json issue examples <issue_id>
```

Use stable, reusable `failure_mode` names such as `tool_error`, `context_loss`, `retrieval_miss`, `policy_violation`, `timeout`, or the domain's existing taxonomy. `issue create` returns the created comment; the generated `issue_id` is inside the JSON body.

### Promote a production trace into a golden eval case
When the failure should be protected by regression testing:

```bash
tael --format json eval case add \
  --from-trace <trace_id> \
  --suite <suite_id> \
  --case-id <stable_case_id> \
  --failure-mode <short_mode> \
  --source-issue-id <issue_id> \
  --critical-path \
  --expected-behavior "<durable expected behavior>"
```

If you created the case first and later identify the issue:

```bash
tael --format json eval case link --case-id <stable_case_id> --issue-id <issue_id>
```

Audit case quality before trusting a suite:
```bash
tael --format json eval suite inspect <suite_id>
```

Read `missing_expected_behavior`, `provenance_free`, and `duplicate_failure_modes` first. A case without source-trace provenance or durable expected behavior is weak evidence.

### Define and trend long-running signals
Use signals for patterns that should be watched across many traces:

```bash
tael --format json signal create \
  --from-trace <trace_id> \
  --name <signal_name> \
  --failure-mode <short_mode> \
  --summary "<what this signal means>" \
  --query "<optional human-readable classifier/query>"

tael --format json signal trend <signal_name>
```

`signal trend` counts matching signal definitions, failure reviews, and self diagnostics by day. It is useful for directionality, not precise incident metrics.

### Compare production experiment variants
If spans carry `tael.experiment.id` and `tael.experiment.variant`, compare variants directly:

```bash
tael --format json experiment compare <experiment_id> --last 24h
tael --format json experiment compare <experiment_id> --signal <failure_mode_or_signal> --last 24h
```

This reports trace count, span count, error count/rate, average span duration, and optional signal count/rate per variant. Treat it as an operational comparison over observed traces, not a randomized-experiment statistics package.

### Record untrusted agent self diagnostics

Use this sparingly when the agent itself caused or noticed a limitation in a trace:

```bash
tael --format json diagnose report \
  --trace-id <trace_id> \
  --span-id <optional_span_id> \
  --category missing_context|capability_gap|broken_tool|<other> \
  --severity low|medium|high|critical \
  --confidence low|medium|high \
  --summary "<one sentence>"

tael --format json diagnose list
```

Diagnostics are untrusted hints. Label confidence conservatively and avoid presenting them as verified root cause without corroborating telemetry.

## Trace-native evals

Eval runs reuse tael telemetry. Runner spans carry eval attributes, scores are stored as `tael_eval_score` metrics, large rationales can be blob-backed, and progress is visible in the same TUI as production traces.

### Run cases

```bash
tael eval run <cases.jsonl> \
  --suite <suite_id> \
  --cmd '<command using {case_id} {case_index} {run_id} {suite_id}>' \
  --code-version <version> \
  --run-id <optional_run_id>
```

Each JSONL case should include `case_id` or `id`. The child command receives `TAEL_EVAL_SUITE_ID`, `TAEL_EVAL_RUN_ID`, `TAEL_EVAL_CASE_ID`, `TAEL_EVAL_CASE_INDEX`, `TAEL_EVAL_CASE_COUNT`, `TAEL_EVAL_TRACE_ID`, `TAEL_EVAL_SPAN_ID`, optional `TAEL_EVAL_CODE_VERSION`, and `OTEL_EXPORTER_OTLP_ENDPOINT`.

### Score and inspect runs

```bash
tael --format json eval score <run_id> <scores.jsonl>
tael --format json eval runs
tael --format json eval status <run_id>
tael --format json eval cases <run_id>
tael --format json eval scores <run_id>
tael --format json eval report <run_id>
tael --format json eval compare <run_id> <baseline_run_id>
```

Score JSONL lines should include `case_id`, `metric`, and `value`; optional fields include `suite_id`, `trace_id`, `span_id`, `scorer`, `label`, `rationale`, `rationale_sha256`, and `source`. If a line contains `rationale` and no `rationale_sha256`, the CLI uploads the rationale as a blob and stores its SHA-256.

For human live progress, use:

```bash
tael live --evals
tael live --eval-run <run_id>
```

As an agent, prefer the JSON eval commands over the TUI unless the user explicitly asks for an interactive view.

### SQL escape hatch (advanced, and opt-in at build time)

**Check this before reaching for it.** `tael query sql` needs a server built
with either `--features sql` (DataFusion over the default storage engine) or
`--features duckdb` (the legacy backend). A plain `cargo install tael-cli`
has neither, and the error names both.

DataFusion roughly doubles the binary, which is why it is opt-in rather than
default. Install it with `cargo install tael-cli --features sql` when the
structured commands genuinely can't express the cut you need.

Where it is available, it runs read-only SQL over the telemetry tables
(`spans`, `logs`, `metrics`, `trace_comments`), with identical column names on
both backends so a query is portable between them. Span rows additionally
carry flattened `llm_provider`, `llm_model`, `input_tokens`, `output_tokens`,
`total_tokens`, and `cost_usd` columns, so token and cost aggregations don't
need JSON surgery:

```bash
tael --format json query sql "SELECT service, COUNT(*) AS n FROM spans WHERE status = 'error' GROUP BY service ORDER BY n DESC"
```

Returns `{"rows": [...], "count": N}`. Only `SELECT`/`WITH` are allowed — mutations are rejected.

Without a SQL build, cover the same ground with `summarize` (aggregates by
service and operation), `diff` (window comparison), `topology` (cross-service
call counts and error rates), and the PromQL subset with `sum by (...)`.
Between them these answer most of what SQL gets reached for.

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
- **Prefer span duration over histogram metrics for latency.** tael retains histogram buckets, so `histogram_quantile` works — but a span carries the attributes that explain *why* a request was slow, and a histogram bucket doesn't. Use histograms for aggregate distributions, spans for anything you'll need to investigate.
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

**Histogram quantiles are bucket-resolution estimates.** OTLP Histogram and ExponentialHistogram points retain their bucket layout, so `histogram_quantile(0.95, metric)` works. The answer interpolates within the containing bucket — a histogram doesn't keep individual observations — so it is an estimate, not an exact percentile. A quantile landing in the open-ended top bucket reports the producer's `max`. Points ingested before bucket retention, and anything from Prometheus remote-write, have no bucket layout and are skipped rather than reported as zero.

**Prometheus remote-write loses type info.** Metrics ingested via `/api/v1/write` are all stored with `metric_type = "unknown"`. Filtering `--type gauge` won't match them.

**`rate()` is approximate.** The implementation is `max(last - first, 0) / elapsed_seconds` — a naive counter-reset clamp. It does not extrapolate across scrape boundaries the way Prometheus does. Fine for trend detection, **not** for SLO math.

**Log body search is substring, not regex.** `--body-contains "5\d\d"` will not do what you think.

**Attribute filtering has three operators.** `--attribute k=v` matches the whole value exactly, `--attribute 'k~=v'` matches a substring, and `--attribute 'k=~pattern'` matches a regex. All are repeatable and ANDed. Reach for the substring form whenever you don't already know the exact value — a URL with an ID in it, a model name with a date suffix. An invalid regex is rejected up front (exit 3) rather than silently matching nothing. Quote the spec so the shell doesn't eat the operator.

**`--text` search needs the tael-backend storage.** Full-text search is served by the default engine's index; under `--storage duckdb` it returns nothing. Three kinds of text are indexed, all resolving to trace IDs so one query reaches every signal: LLM prompt/completion payloads, log bodies (only for logs carrying a trace ID), and span attribute values as `key=value` text. That last one is why `--text PaymentDeclined` finds a trace whose `error.type` attribute holds it. Attribute text is truncated at 4 KB per span, so an attribute carrying a whole request body is only partly searchable.

**Single-node engine.** tael runs as one node: reads scan the in-memory/LSM hot tier for recent data and Parquet for older data. Keep `--last` windows narrow (minutes to hours) when the server is busy — wide scans over millions of rows are slow.

## Output shape cheat sheet

```
services         → {"services": [...]}
query traces     → {"spans": [...]}
get trace        → {"trace_id", "span_count", "spans": [...]}
query logs       → {"logs": [...], "count": N}
query metrics    → {"metrics": [...], "count": N}                  (filter mode)
query metrics    → {"query", "series": [...], "count": N}          (--query mode)
query sql        → {"rows": [...], "count": N}   (needs a --features sql or --features duckdb build)
comment list     → {"comments": [...], "count": N}
summarize        → {"window_seconds", "traces", "top_services", "top_error_operations", "logs", "metrics"}
anomalies        → {"current_seconds", "baseline_seconds", "anomalies": [...]}
correlate        → {"trace_id", "span_count", "services", "start_time", "end_time", "duration_ms", "error_count", "logs": [...], "metrics": [...]}
watch            → {"timestamp", "window_seconds", "traces", "logs", "metrics"}   (one JSON object per tick)
eval runs        → {"runs": [...], "count": N}
eval status      → {"run": {...}}
eval cases       → {"run_id", "cases": [...], "count": N}
eval scores      → {"run_id", "scores": [...], "count": N} or {"scores": [...], "count": N} after ingest
eval report      → {"run": {...}, "cases": [...]}
eval compare     → {"current_run_id", "baseline_run_id", "cases": [...]}
eval suite inspect → {"suite", "case_count", "critical_path_count", "provenance_free", "missing_expected_behavior", "duplicate_failure_modes", "cases"}
issue list       → {"issues": [...], "count": N}
issue examples   → {"issue_id", "examples": [...], "count": N}
signal trend     → {"signal", "definitions", "matches", "buckets", "count": N}
experiment compare → {"experiment_id", "variants": [...], "count": N}
diagnose list    → {"diagnostics": [...], "count": N}
topology         → {"services": [...], "edges": [...], "spans_examined", "spans_with_parent_outside_window"}
diff             → {"current_window_seconds", "baseline_window_seconds", "<metric>": {"current","baseline","delta","ratio"}, "totals", "note"}
get metric       → {"metric", "type", "unit", "point_count", "series_count", "services", "label_keys", "value_min", "value_max", "histogram_quantile_available", "recent_points"}
alert list       → {"alerts": [...], "count": N}
alerts           → {"events": [...], "count": N}
score rule list  → {"rules": [...], "count": N}
config show      → {"config_file", "config_file_exists", "storage", "retention_days"}
auth list        → {"keystore", "count", "keys": [...]}
<error>          → {"error": "..."}   (with non-2xx HTTP status)
```

### Exit codes

Branch on the exit code instead of parsing output:

```
0  success                       3  malformed query or argument
1  unclassified failure          4  server unreachable
2  matched nothing               5  auth failure
                                 6  a --exit-on condition tripped
```

Code 2 is not an error — the command printed a well-formed empty response.
`tael query traces --status error` exiting 2 means "no errors", which is a
useful thing to be able to test directly.

## Working with results

- When an investigation involves more than ~5 traces or ~20 log lines, summarize for the user — don't paste raw JSON. Pull out `trace_id`, service, duration, and the one or two fields that matter.
- Always include the `trace_id` (or a short prefix) when reporting a finding so the user can pull the full trace themselves.
- If the CLI returns `{"error": ...}`, surface the error message verbatim — don't retry blindly.
- Prefer `--last` windows over `--limit` for control. A tight time window is almost always what the user meant by "recent".

## Reference

For the full HTTP surface (if you need to bypass the CLI), see `llm.txt` at the repo root. The CLI is a thin wrapper over a REST API on port 7701.
