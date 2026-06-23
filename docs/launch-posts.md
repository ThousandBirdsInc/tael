# Launch posts

Sharing copy for tael. Repo: https://github.com/ThousandBirdsInc/tael

---

## Hacker News (Show HN)

**Title:**

```
Show HN: Tael – An observability platform built for AI agents (Rust, OTLP)
```

**URL:** https://github.com/ThousandBirdsInc/tael

**Body:**

Hi HN, I built tael, an observability platform designed for AI agents to use
directly, not just humans staring at dashboards.

The premise: most observability tools assume a person is in the loop, clicking
through a web UI. But more and more debugging is being done by agents — Claude
Code, Devin, custom autonomous systems. Those agents are bad at screenshots and
good at structured data and CLIs. So tael is CLI-first and returns JSON by
default. `tael query traces --status error` gives an agent something it can
actually parse and act on, and `--format table` is there when a human wants to
read it.

What it does:

- Ingests traces, logs, and metrics over standard OTLP gRPC (plus Prometheus
  remote-write). No proprietary SDK — point your existing OpenTelemetry exporter
  at it.
- LLM spans (the `gen_ai.*` semantic conventions) get first-class treatment:
  typed model/token/cost fields, and prompt/completion payloads stored as
  deduplicated, content-addressed blobs with full-text search.
- Everything is one Rust binary — server, CLI, and TUI (and an optional Tauri
  desktop GUI). `cargo binstall tael-cli` and you have the whole stack.
- Higher-level commands an agent can drive on its own: `summarize`, `anomalies`
  (error-rate / p95 regressions vs a baseline window), `correlate` (pull spans +
  logs + metrics for one trace), and `watch`.
- Agents can leave `comment`s on traces — collaborative debugging, audit trails,
  investigation notes that stay attached to the trace.
- A reliability loop for evals: promote a real production failure into a golden
  regression case, classify recurring issues, track long-running signals, and
  compare experiment variants — all built on the same spans/comments/SQL that
  production debugging uses, so evals are trace-native instead of a separate
  result row.
- It also ships a Claude Code skill, so Claude Code automatically learns how to
  query your telemetry when you're debugging in a project that uses tael.

Storage is a purpose-built tiered engine tuned for OTel + LLM traces: a WAL, an
LSM hot tier, a Parquet cold tier, content-addressed blobs for payloads, and a
Tantivy full-text index. DuckDB is still there as an optional fallback backend.

It's MIT-licensed. macOS and Linux (Windows isn't supported yet — a WAL
dependency uses unix-only file I/O).

I'd love feedback, especially from people running agents in production: what do
your agents actually need to query to debug themselves, and where does the
"agent as the primary user of observability" framing break down?
```

---

## Twitter / X thread

**1/**
```
Observability tools are built for humans staring at dashboards.

But increasingly it's AI agents doing the debugging — Claude Code, Devin, custom
systems. They're bad at screenshots and great at JSON + CLIs.

So I built tael: an observability platform that's agent-native. 🧵
```

**2/**
```
tael is CLI-first and returns structured JSON by default.

  tael query traces --status error
  tael query traces --min-duration 500ms --last 1h
  tael get trace <id>

An agent can parse and act on that. Want it human-readable? --format table.
```

**3/**
```
It speaks standard OpenTelemetry — OTLP gRPC + Prometheus remote-write.

No proprietary SDK. Point your existing OTel exporter at it and you're done.
```

**4/**
```
LLM traces are first-class.

Spans using the gen_ai.* conventions get typed model / token / cost fields, and
prompt + completion payloads are stored as deduplicated, content-addressed blobs
with full-text search.

  tael query traces --text "rate limit"
```

**5/**
```
It's one Rust binary — server, CLI, and TUI in one (plus an optional desktop
GUI).

  cargo binstall tael-cli

…and you have the whole stack. There's a live TUI with a waterfall trace
visualizer too.
```

**6/**
```
Higher-level commands an agent can drive on its own:

  tael summarize --last 1h
  tael anomalies --last 5m --baseline 30m   # error-rate / p95 regressions
  tael correlate --trace <id>               # spans + logs + metrics for one trace
  tael watch
```

**7/**
```
Agents can leave comments on traces — collaborative debugging and audit trails
that stay attached to the trace that motivated them.

It also ships a Claude Code skill, so Claude Code learns how to query your
telemetry automatically while debugging.
```

**8/**
```
And a trace-native reliability loop for evals:

production failure → golden regression case → classify recurring issues → track
signals → compare experiment variants

All on the same spans/comments/SQL as production debugging. Not a separate
eval database.
```

**9/**
```
Under the hood: a purpose-built tiered storage engine — WAL + LSM hot tier +
Parquet cold tier + content-addressed blobs + Tantivy full-text search. Tuned
for OTel + LLM traces.

MIT licensed. macOS + Linux.

⭐ https://github.com/ThousandBirdsInc/tael
```
