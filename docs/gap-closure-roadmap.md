# Gap-Closure Roadmap: Competing with Eval-First SaaS Platforms

Status: partially implemented — see **Implementation status** below
Extends: [DESIGN.md](../DESIGN.md) milestones M4+, [tael-backend-design.md](tael-backend-design.md), [tael-evals-design.md](tael-evals-design.md)

## Context

A functional comparison against Braintrust (the most polished of the eval-first
SaaS platforms) surfaced a set of real gaps alongside tael's real advantages.
tael's advantages are structural: full-signal telemetry (traces + logs +
metrics, not just LLM spans), vendor-neutral ingest (OTLP / Prometheus /
Datadog, no SDK wrapper), local-first single-binary deployment, no provider
keys held, and an agent-native reliability loop no SaaS platform has.

The gaps fall into four groups:

1. **Trust & fidelity** — no auth, no OTLP HTTP, histogram buckets dropped,
   retention hard-coded to env vars, no benchmarks for the default backend.
2. **Agent interface completion** — no MCP server, no `watch --exit-on`, search
   covers only LLM payloads, `diff`/`topology`/`ingest status` unbuilt.
3. **The eval loop** — no managed datasets, no online scoring of production
   traffic, no alerting, no review workflow, no trace clustering.
4. **Scale & distribution** — HA/sharding half-built, no multi-tenancy
   enforcement, no S3 cold tier, no Homebrew.

This document plans closure of all four groups **without compromising the
thesis**. Every feature below is designed CLI-first with JSON output, runs in
the same single binary, and treats an AI agent as the primary operator.

## Implementation status

**All phases implemented.** Everything below ships with tests and was verified
end to end against a running server.

| Item | What landed |
|---|---|
| A1 auth | API keys with reader/writer/admin, salted-digest keystore, one middleware over REST/OTLP-HTTP/remote-write/dd-trace plus a gRPC interceptor, live keystore reload, fail-closed on non-loopback binds |
| A2 OTLP/HTTP | `:4318` listener and the same routes on the REST listener, gzip, 415 naming the supported content type for OTLP/JSON |
| A3 histograms | Bucket layout retained (explicit + exponential, converted at ingest), aggregation temporality stored, `histogram_quantile(phi, selector)` with `by (...)`; also fixed the PromQL lexer rejecting dotted OTel metric names |
| A4 retention | Per-signal TOML policy with flag > env > file > default, per-signal cold-partition drops (which also fixed metric rollups never expiring), `tael config show/init` |
| A5 benchmarks | `tael-backend` ingest/query/aggregation/compaction benches; BENCHMARKS.md leads with them and marks the DuckDB numbers as non-comparable |
| B1 MCP | `tael mcp serve` over stdio, 19 tools mapped onto existing endpoints, SKILL.md and llm.txt as resources |
| B2 exit codes | Category codes 0–6, `watch --exit-on` with absolute and baseline-relative thresholds, `query traces --explain` |
| B3 search | Substring (`k~=v`) and regex (`k=~v`) attribute matchers; the text index now covers log bodies and span attribute values, not just LLM payloads |
| B4 M3 commands | `topology`, `diff`, `get metric` |
| B5 SQL | DataFusion over the default engine behind `--features sql`, same tables and column names as the DuckDB backend plus flattened LLM token/cost columns; in-memory, bounded, read-only |
| C1 case suites | `eval suite push/pull/snapshot/list/diff`, content-addressed cases, content-derived immutable snapshot ids, byte-stable canonical JSONL round trip |
| C2 online scoring | `score rule create/list/delete`, deterministic per-trace sampling, per-rule scored memory, scorer contract identical to `eval run`, progress and last-error reporting |
| C3 alerting | Rules with `for` semantics, span-derived series needing no instrumentation, webhook/exec/SSE sinks, transition-only delivery; PromQL gained top-level scalar comparison |
| C4 review | `review request/list/submit`, comment-backed, append-only with derived state, option validation, eval-case linkage |
| C5 clustering | `tael embed/similar/cluster`, user-supplied embedder, deterministic k-means, cohesion reported so a weak grouping can be called weak |
| D1 tenancy | Reads scoped and writes stamped by the caller's tenant; SQL admin-only under tenancy since it cannot be row-scoped. **Authorization, not isolation** — storage is not partitioned by tenant, and the docs say so |
| D2 object storage | S3 alongside GCS for the cold tier and blob store; the blob-GC single-owner guard now covers any shared store |
| D3 packaging | `install.sh` fetching prebuilt binaries, Homebrew formula in-repo |
| D4 CI + contract | CI runs fmt, clippy at `-D warnings`, tests, and a build of each optional feature. A test walks the clap tree and fails when a command is missing from SKILL.md and llm.txt — it immediately found eleven that had shipped undocumented |

### Deviations from the plan, and why

**Retention defaults were not shortened.** DESIGN.md proposes 7d/14d/30d, but
everything was previously retained for a year; adopting the shorter windows as
defaults would delete a year of history the first time an existing server
restarted after upgrade. The recommended windows ship as a file
`tael config init` writes.

**SQL is opt-in, not default.** Closing the gap was the goal, but DataFusion
roughly doubles the binary (448 MB to 983 MB in debug) and tael's pitch is one
small binary. Two features now provide SQL where one did before, and the
default build's error names both plus the structured alternatives.

**Tenancy is authorization, not isolation.** Physical isolation needs a
storage key-schema change. What shipped is enforced at the query layer and
documented as such rather than sold as more than it is.

### Findings that were not in the plan

The benchmarks turned up three things worth their own work: unbatched ingest
is 367 spans/s because the WAL fsync barrier dominates below ~1000 records per
call; `query_traces` has no secondary indexes, so cost scales inversely with
filter selectivity; and `query_summary` takes 65ms per 10k spans, which puts a
million-span hot tier into multi-second territory for the command SKILL.md
tells agents to run first.

Two defects surfaced along the way: the PromQL lexer rejected dotted
OpenTelemetry metric names, making the subset unusable for semconv metrics;
and the 5-minute metric rollups were written and expired but never readable,
which made downsampling pure cost until `query metrics --rollups` landed.

## Thesis guardrails (won't build)

These stay non-goals no matter how prominent they are in competitor products.
Each has a thesis-consistent answer instead:

| Competitor feature | Why we won't build it | Thesis-consistent answer |
|---|---|---|
| Web dashboard / Monitor UI | Humans-second; dashboards are pre-agent UX | `summarize` / `watch` / alert feed JSON; TUI + desktop GUI for human spot-checks |
| Playground (prompt × model grid UI) | Iteration is the calling agent's job | `eval run` + `experiment compare`; the agent *is* the playground |
| Prompt management platform (versioned CRUD, environments, server-side `invoke()`) | Prompts belong in git with the code that uses them | Spans already carry `prompt_sha256`; add git-metadata span conventions (§B4) so any prompt version is joinable to its traces |
| AI gateway / provider proxy | Holding customer provider keys is the exact trust surface we advertise not having | None. Ingest is passive; tael never sits on the request path |
| Hosted SaaS / browser eval service | Local-first is the moat | Docker + (future) object-store cold tier covers team deployments |
| Natural-language query layer | Resolved won't-do; the calling agent translates intent | llm.txt + SKILL.md keep the query contract cheap to learn |
| In-product AI agent (Loop equivalent) | tael's "Loop" is the agent using tael | MCP server (§B1) makes any external agent the resident analyst |
| Per-token streaming capture | ~100× volume for marginal value | Keep `ttft_ms` + `inter_token_ms` summary stats |

## Phase A — Trust & fidelity (table stakes)

Everything here is a prerequisite for anyone comparing tael seriously against
a paid platform. No new product surface — just making the existing one honest
and safe.

### A1. Auth: implement the designed API-key model
The design already exists in DESIGN.md (Agent Auth Model); it is unbuilt.
- `tael auth create-key --role reader|writer|admin` → `tael_r_*` / `tael_w_*` /
  `tael_a_*` keys; `TAEL_API_KEY` on the client; `--auth=required` on serve
  (default stays `off` when bound to loopback, forced `required` on non-loopback
  binds so Docker deployments fail closed rather than open).
- Bearer auth on REST + gRPC metadata on OTLP; Datadog intake keys via the
  existing `DD-API-KEY` header.
- Acceptance: non-loopback serve without keys refuses to start; every REST
  route and ingest path enforces role; keys revocable via `tael auth revoke`.

### A2. OTLP HTTP (:4318)
Highest-frequency onboarding papercut — many SDK default configs speak
http/protobuf. Mount `/v1/traces`, `/v1/logs`, `/v1/metrics` (protobuf +
gzip; JSON optional later) on a 4318 listener sharing the gRPC ingest path.
- Acceptance: stock `opentelemetry-python`/`-js` with default OTLP HTTP
  exporter ingests with zero config beyond the endpoint URL.

### A3. Histogram fidelity → real p95/p99
Today Histogram/ExponentialHistogram collapse to `value = sum` and quantiles
are impossible — the single worst data-fidelity gap.
- Store bucket bounds + counts (and exp-histogram scale/buckets) in a
  `histogram` column family / Parquet struct column; keep the flat `sum` row
  for backward compatibility.
- Add `histogram_quantile(φ, metric[range])` to the PromQL subset, and
  `p50/p95/p99` fields to `summarize` sourced from real buckets where present.
- Update SKILL.md/llm.txt caveats (delete "p95/p99 cannot be computed").
- Acceptance: OTLP histogram round-trips to a correct `histogram_quantile`
  answer within bucket-resolution error.

### A4. Retention & downsampling as config (lands "Phase 7")
Replace the env-var stopgaps with the designed per-signal policy.
- Single TOML file (`~/.tael/config.toml`, `--config`), env vars still win:
  per-signal retention (`traces`, `llm_payload_blobs`, `logs`,
  `metrics_raw`, `metrics_5m`), hot-tier hours, compaction interval.
- Wire the already-designed 5m metric downsampling into compaction; drop raw
  metrics on their clock while `metrics_5m` lives on its own.
- Acceptance: `tael serve --config` honors distinct per-signal windows;
  defaults documented in README replace the aspirational DESIGN.md numbers.

### A5. Benchmarks for the default backend
The only published storage numbers are legacy DuckDB (~900 spans/s inserts) —
actively harmful marketing. Add Criterion + end-to-end benches for
`tael-backend`: OTLP ingest throughput (spans/s, logs/s, metric points/s),
hot query latency, cold Parquet scan, `--text` search, compaction cost.
Publish in BENCHMARKS.md and cite in README.
- Acceptance: BENCHMARKS.md leads with tael-backend numbers; DuckDB moved to
  a "legacy backend" appendix.

## Phase B — Agent interface completion

The thesis says agents are the primary users; these items finish that
interface.

### B1. MCP server
`tael mcp serve` (stdio) + `--http` (streamable HTTP on the REST listener).
Tools map 1:1 onto the existing CLI/REST surface — same names, same JSON
shapes already documented in llm.txt, so nothing is invented: `query_traces`,
`query_logs`, `query_metrics`, `query_sql`, `get_trace`, `services`,
`summarize`, `anomalies`, `correlate`, `comment_add`, `eval_*`, `issue_*`,
`signal_*`, `experiment_compare`, `diagnose_*`. SKILL.md and llm.txt ship as
MCP resources so a connected agent self-onboards.
- Acceptance: Claude Code with only the MCP server configured completes the
  SKILL.md 7-step debugging playbook without shelling out.

### B2. `watch --exit-on`, exit codes, `--explain`
- `tael watch --exit-on 'error_rate>0.05' --exit-on 'p95_ms>2x'` — exits with
  a distinct code and a final JSON verdict when a condition trips; this is the
  blocking primitive agents need for "wait until it breaks / recovers."
- Category-encoded exit codes across the CLI (0 ok, 2 no-results, 3 bad
  query, 4 server unreachable, 5 auth) — currently everything exits 0.
  `server status` keeps stdout contract but adopts codes.
- `--explain` on query commands: echo the effective filter, indexes used,
  tiers touched, rows scanned — the agent's substitute for a query planner UI.

### B3. Search & filter expansion
- Extend the Tantivy index to log bodies and span attribute values (designed,
  unbuilt); `--text` then works across signals.
- Substring/regex matchers on span attributes and log fields:
  `--attribute k~=substr`, `--attribute k=~/regex/`; same for PromQL label
  matchers (`=~`/`!~`).
- Structured log-field querying: index log attributes; `query logs --field k=v`.
- Acceptance: SKILL.md caveats section shrinks accordingly.

### B4. Finish the M3 command surface + git conventions
- `tael diff --last 1h --baseline 24h [--service]` — the general
  current-vs-baseline comparator (`anomalies` is its opinionated cousin).
- `tael topology` — service dependency graph from span parent/child edges,
  JSON adjacency + `--format table`.
- `tael ingest status` — per-protocol ingest counters, last-seen, drop counts.
- `tael get metric <name>` — series metadata + recent points.
- Git/prompt span conventions: document `tael.git.commit`, `tael.git.branch`,
  `tael.prompt.name` attributes; `experiment compare` and `eval report` learn
  `--group-by` on them. This is the whole thesis-consistent answer to prompt
  versioning: prompts live in git, traces join to them by hash + commit.

## Phase C — Close the eval loop

Braintrust's core pitch is production trace → dataset → experiment → deploy.
tael has the trace and the experiment; this phase closes dataset, online
scoring, alerting, review, and clustering — each in agent-native shape,
reusing the comment/metric/blob conventions from
[tael-evals-design.md](tael-evals-design.md) rather than new subsystems.

### C1. Managed case suites (dataset parity)
Keep JSONL as the interchange format; make the server the system of record.
- `tael eval suite push <suite> cases.jsonl` / `pull` — cases stored as blobs
  + comment-backed provenance records (already the convention for
  `case add --from-trace`); dedup free via content addressing.
- `tael eval suite snapshot <suite>` → immutable snapshot id; `eval run`
  accepts `--suite <name>@<snapshot>` so experiments pin exact data.
- `tael eval suite diff <a> <b>` — added/removed/changed cases.
- Git-friendly by design: `pull` emits canonical sorted JSONL so suites can
  also live in-repo and round-trip.
- Acceptance: an agent can promote a prod trace to a case, snapshot the
  suite, run two experiments against the same snapshot, and `eval compare`
  them — no local files required.

### C2. Online scoring (sampled production evaluation)
The one Braintrust feature with no tael analog and high thesis fit.
- `tael score rule create --name faithfulness --sample 0.05
  --match 'service=agent-api attribute:gen_ai.system=anthropic'
  --cmd './score.sh'` — server samples matching completed traces, invokes the
  scorer command with `TAEL_EVAL_*` env (same contract as `eval run`), writes
  results as `tael_eval_score` metric points tagged `rule=<name>`.
- Scorers are arbitrary commands (code judge, LLM judge, whatever) — tael
  schedules and records; it never calls model providers itself (guardrail).
- `tael score rules` / `rule status` for inventory and lag/error visibility.
- Trend via existing PromQL (`avg(tael_eval_score{rule="faithfulness"})`) and
  `signal trend`; alertable via C3.
- Acceptance: a judge script scores 5% of production traces continuously with
  no ingest-path latency impact, and a score regression is visible in
  `summarize` within one interval.

### C3. Alerting engine
No dashboards — alerts are JSON events an agent (or webhook) consumes.
- `tael alert create --name high-errors --query
  'rate(tael_spans_errors[5m]) > 0.05' --for 5m` — server-side evaluation on
  the existing PromQL subset plus derived series (error rate, p95 via A3,
  eval scores via C2, signal counts).
- Sinks: `--sink webhook=<url>`, `--sink exec=<cmd>`, and always the internal
  feed: `tael alerts [--follow]` (SSE-backed, like `traces/live`) +
  `GET /api/v1/alerts`. `--follow` is the long-poll primitive for a
  babysitting agent; `watch --exit-on alert:<name>` ties into B2.
- Alert state transitions recorded as comment-convention events so firing
  history is queryable via SQL like everything else.
- Acceptance: error-rate spike fires within one eval interval, delivers to a
  webhook, and appears in `tael alerts --follow`.

### C4. Review workflow (humans assist agents)
Braintrust queues humans as the primary reviewers; tael inverts it — the
agent triages, humans adjudicate the residue. Comment-backed, no new tables.
- `tael review request --trace <id> --question "was this refusal correct?"
  --options yes,no` — structured comment, state `open`.
- `tael review list --open` / `review submit <id> --answer no --note ...` —
  answers write back as comments; when linked to an eval case, the verdict
  lands in the case's `expected-behavior`.
- TUI/GUI get a Review tab listing open requests with the trace waterfall one
  keypress away — the human's whole job is answering queued questions.
- Acceptance: agent files review requests for low-confidence
  self-diagnostics; human clears the queue in the TUI; agent consumes
  verdicts via `review list --state answered` and updates cases.

### C5. Trace clustering (Topics analog)
Ship the already-designed semantic index and let the agent do the narration.
- HNSW embedding index over LLM prompt/completion blobs (design exists;
  off by default). Embeddings computed by a user-supplied command
  (`--embed-cmd`) — same no-provider-keys guardrail as C2.
- `tael similar <trace-id>` — nearest-neighbor traces; the missing primitive
  for "has this failure happened before?"
- `tael cluster --last 24h --service X` — k-means/HDBSCAN over the index,
  returns cluster members + exemplar traces as JSON. Naming/summarizing
  clusters is the calling agent's job (SKILL.md gains a clustering playbook:
  cluster → summarize exemplars → file issues → promote cases).
- Acceptance: agent runs the playbook end-to-end and files one issue per
  discovered failure cluster with linked example traces.

## Phase D — Scale & distribution

### D1. Multi-tenancy enforcement
Storage is already tenant-partitioned; enforce it. API keys (A1) gain a
tenant claim; every read/write path filters by the key's tenant; `tael_a_*`
keys may set `--tenant`. Single-tenant default stays zero-config.

### D2. Cold tier on S3 + HA hardening
- `TAEL_COLD_STORE=s3` via the existing `object_store` integration (GCS is
  done; S3 is the bigger market).
- Promote the half-built pieces to documented, tested config: query shards,
  WAL standby acks, gossip election — one "running tael for a team" doc with
  the supported topologies and the explicit single-writer-per-shard rule.
- End-to-end failover test in CI (kill leader, standby resumes, no ack'd loss).

### D3. Packaging & platform
- Homebrew tap (M4 checkbox) + `install.sh`; keep `cargo binstall` primary.
- Windows: decide explicitly. Recommendation: **document as won't-fix for
  serve** (WAL is unix-only), but make the *client* CLI compile on Windows so
  agents on Windows hosts can query a remote/Docker server. Revisit only on
  demand signal.

### D4. Docs & contract freshness
- Fix known staleness: llm.txt still says "single-node DuckDB"; README OTLP
  port table implies 4318 pre-A2; DESIGN.md retention defaults vs. reality.
- Add a CI check that the clap command tree and llm.txt command list agree
  (generate the reference from `--help` output), so the agent contract can't
  drift again.

## Sequencing & dependencies

```
A1 auth ──────────────┬─► D1 multi-tenancy
A2 otlp-http          │
A3 histograms ────────┼─► C3 alerting (p95 alerts)
A4 retention config   │
A5 benchmarks         │
B1 mcp ◄─ (llm.txt)   │
B2 exit-on/codes ◄────┼── C3 (watch --exit-on alert:)
B3 search             │
B4 diff/topology/git  │
C1 suites ─► C2 online scoring ─► C3 ─► C4 review
C5 clustering (independent; needs embed index)
D2 s3/ha, D3 packaging, D4 docs (parallel, ongoing)
```

Suggested order of attack: **A2 → A1 → A3 → B2 → A5** (a week-scale burst
that removes every "yes but" in a head-to-head), then **B1 (MCP)** as the
single highest-leverage adoption feature, then C1→C2→C3 as one arc (the
eval-loop story), with A4, B3, B4, C4, C5, and D running behind as capacity
allows.

## What this buys, restated against the comparison

After Phases A–C, the honest comparison table changes from "Braintrust is
more complete" to a genuine fork in philosophy:

- **They have**: web playground, prompt CRUD, provider gateway, hosted scale,
  human-first review, six SDKs.
- **We have**: everything in their observe/evaluate loop (datasets, online
  scoring, alerts, review, clustering) in agent-native CLI/MCP shape, plus
  full-signal correlation, vendor-neutral ingest, local-first deployment with
  auth, honest quantiles, and zero provider-key custody.

The remaining deltas are all things the thesis says we don't want.
