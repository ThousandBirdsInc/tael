# Tael Evals: Trace-Native Evaluation for Agents

> Status: Draft | Owner: colton@thousandbirds.ai | Last updated: 2026-05-28
>
> Companion to [`../DESIGN.md`](../DESIGN.md). This doc describes how Tael can
> collect, score, query, and visualize agent evals while reusing existing spans,
> metrics, comments, blobs, SQL, and the `tael live` TUI.

## Problem

Agent eval systems usually keep a narrow result row: input, output, pass/fail,
maybe a judge score. Tael already captures the richer execution artifact: model
calls, tool calls, logs, errors, timing, token usage, cost, retries, filesystem
actions, and span-level causality.

We want Tael to support eval workflows without creating a separate eval product
beside observability. An eval case should be inspectable as a normal trace, and
eval reporting should be a queryable layer over the same telemetry Tael already
stores.

## Goals

1. Reuse existing storage primitives wherever possible: spans, metrics, logs,
   trace comments, blob storage, read-only SQL, and the live TUI.
2. Treat the trace as the source of truth for each evaluated execution.
3. Allow scores and annotations at both trace and span granularity.
4. Show eval run progress in `tael live` while a run is still executing.
5. Keep the MVP useful without adding dedicated eval tables.
6. Leave a clean migration path to purpose-built eval tables if conventions over
   existing primitives become painful.

## Non-Goals

- A standalone hosted eval service.
- A browser dashboard for evals.
- Replacing test frameworks or LLM judge frameworks.
- A mandatory Tael SDK. Evaluated code can use normal OpenTelemetry.
- Strong dataset/version governance in the MVP.

## Core Design

An eval execution is a normal trace with eval attributes. Scores are normal
metric points. Judge notes and human labels are normal trace comments. Large
inputs, expected outputs, actual outputs, and rationales use the existing blob
store when they are too large for attributes or comments.

The first implementation is therefore a set of conventions plus convenience
commands and endpoints:

```
eval run summary -> case list -> selected case -> trace waterfall -> span details
```

The data stays in the existing telemetry model.

## Floor-Raising Loop

Tael should optimize for floor raising, not benchmark maxxing. The core workflow
is:

```
production trace -> failure classification -> issue/signal -> golden case -> fix -> compare -> monitor
```

That loop is deliberately trace-native. A production failure, an offline eval
case, and a long-running reliability signal all point back to the same execution
artifact: the trace. The product should make it cheap to move between those
states without creating a separate eval dashboard or hosted test runner.

### Failure Review Questions

Every review flow should help an agent or human answer the same small set of
questions:

- What was the last successful step?
- What was the first real failure?
- Did retrieval miss, context disappear, a tool fail, or the final answer
  overstate what the trace supports?
- Is this a one-off stumble, a recurring issue, or a long-horizon signal worth
  monitoring?
- Should this become a golden regression case?

These are product primitives, not only docs. Reports, comments, issue views, and
case-promotion commands should preserve these fields so future sessions do not
repeat the same investigation.

## Production Failure Taxonomy

The MVP should add structured conventions over trace comments before adding new
tables. A trace comment with `kind=failure_review` records a classified
production stumble:

```json
{
  "kind": "failure_review",
  "status": "stumble",
  "failure_mode": "tool_error",
  "impact": "high",
  "last_successful_step": "retrieved invoice metadata",
  "first_failure": "createRefund returned permission_denied",
  "summary": "Agent retried the same forbidden refund tool call instead of escalating.",
  "representative": true
}
```

Suggested lifecycle:

| State | Meaning |
|-------|---------|
| `stumble` | Raw reviewed failure or near miss. Useful firehose, not yet deduped. |
| `issue` | Recurring or important pattern the team intends to discuss or fix. |
| `signal` | Long-running behavior to monitor by query, classifier, or score. |
| `experiment` | A production comparison validating whether a fix helped. |
| `closed` | Pattern is fixed, accepted, or no longer relevant. |

Initial CLI surface:

```bash
tael issue create --from-trace <trace-id> \
  --failure-mode tool_error \
  --impact high \
  --summary "refund tool permission failure loops"

tael issue examples <issue-id>
tael issue promote-signal <issue-id> --name ignored_tool_error
tael signal trend ignored_tool_error --last 7d
```

These can be backed by comments and SQL over spans/logs/metrics first. A later
`issues` table is warranted only when comment querying becomes too awkward.

## Promote Trace to Golden Case

Production traces should become targeted regression cases with one command. The
command should capture enough information to replay or reconstruct the scenario,
but the original trace remains the provenance record.

```bash
tael eval case add --from-trace <trace-id> --suite golden \
  --case-id refund_permission_denied_loop \
  --failure-mode tool_error

tael eval case link --case-id refund_permission_denied_loop --issue <issue-id>
```

Case metadata should include:

| Field | Meaning |
|-------|---------|
| `source_trace_id` | Production or eval trace that motivated the case. |
| `source_issue_id` | Optional issue/signal this case protects. |
| `failure_mode` | Representative failure class. |
| `critical_path` | Whether failure blocks a core workflow. |
| `expected_behavior` | Minimal durable expectation, not a brittle transcript. |
| `created_from` | `production`, `manual`, `synthetic`, or `migration`. |
| `last_failed_at` | Last time this case caught a regression. |

The product should make pruning normal:

```bash
tael eval suite inspect golden
tael eval case prune --suite golden --stale 90d --dry-run
```

An eval suite with fewer, representative cases is preferable to hundreds of
low-signal edge cases.

## Production Experiments

Offline evals protect critical paths; production experiments tell whether a
change helped real users. Tael should support variant metadata on spans and
scores:

| Attribute | Meaning |
|-----------|---------|
| `tael.experiment.id` | Stable experiment identifier. |
| `tael.experiment.variant` | Variant name, such as `control` or `treatment`. |
| `tael.experiment.change_type` | `model`, `prompt`, `tool`, `retrieval`, `guardrail`, or `code`. |
| `tael.experiment.change_id` | Prompt version, model name, feature flag, commit, or config hash. |

Initial query surface:

```bash
tael experiment compare refund-agent-202605 \
  --signal ignored_tool_error \
  --metric task_completion \
  --last 24h

tael signal compare ignored_tool_error --by experiment.variant --last 24h
```

The comparison should report both eval-style outcomes and production symptoms:
task completion, issue/signal rate, refusal rate, tool-error rate, p95 latency,
token cost, and user-retry or correction patterns when available.

## Self Diagnostics

Agents may report their own suspected problems, but those reports are noisy.
Treat them as untrusted stumbles until they are clustered, reviewed, or tied to
observable behavior.

Recommended hidden-tool payload:

```json
{
  "kind": "self_diagnostic",
  "category": "missing_context",
  "severity": "medium",
  "confidence": "low",
  "summary": "Could not find enterprise refund policy in retrieved context."
}
```

Self diagnostics should not automatically create issues or fail evals. They are
useful inputs for discovery and classifier tuning.

## Suite Hygiene

Tael should report whether an eval suite is still earning its keep:

```bash
tael eval suite inspect golden
```

Useful hygiene checks:

- Cases that have not failed in 90 days.
- Flaky cases whose outcome changes without code or prompt changes.
- Duplicate cases protecting the same issue and failure mode.
- Cases with no `source_trace_id` or production provenance.
- Slowest and most expensive cases.
- Critical paths with no golden case.

The recommendation should be conservative: prune low-signal cases, keep
representative failures, and keep suite runtime short enough that failures stay
actionable.

## Eval Identity Conventions

Every evaluated case should emit at least one root span carrying these
attributes:

| Attribute | Required | Meaning |
|-----------|----------|---------|
| `tael.eval.suite_id` | yes | Stable suite/dataset name. |
| `tael.eval.run_id` | yes | Unique run identifier. |
| `tael.eval.case_id` | yes | Stable case identifier within the suite. |
| `tael.eval.code_version` | recommended | Git SHA, image digest, build ID, or other source version. |
| `tael.eval.case_index` | optional | 0-based or 1-based display order. |
| `tael.eval.case_count` | optional | Total cases in the run, if known. |
| `tael.eval.role` | optional | `runner`, `agent`, `judge`, `tool`, `test`, or `scorer`. |

Child spans inherit trace identity through `trace_id`, so the root span is
enough for run/case grouping. Spans that deserve specific eval treatment can
also carry `tael.eval.role` or other eval attributes.

## Scores as Metrics

Scores are stored in the existing `metrics` table using a reserved metric name:

```
name: tael_eval_score
service: tael-eval
metric_type: gauge
value: numeric score
```

Required metric attributes:

| Attribute | Meaning |
|-----------|---------|
| `suite_id` | Suite/dataset name. |
| `run_id` | Eval run ID. |
| `case_id` | Eval case ID. |
| `metric` | Score name, such as `correctness`, `pass`, `quality`, `cost_usd`. |
| `scorer` | Scorer identity, such as `unit-tests`, `llm-judge`, `human`. |
| `trace_id` | Trace for the evaluated case. |

Optional metric attributes:

| Attribute | Meaning |
|-----------|---------|
| `span_id` | Span the score applies to. Omit for whole-case scores. |
| `label` | Human-readable label such as `pass`, `fail`, or `flaky`. |
| `threshold` | Threshold used by the scorer. |
| `rationale_sha256` | Blob hash for a long scorer rationale. |
| `source` | `script`, `test`, `llm`, `human`, or `derived_sql`. |

Example:

```json
{
  "timestamp": "2026-05-28T17:10:00Z",
  "service": "tael-eval",
  "name": "tael_eval_score",
  "metric_type": "gauge",
  "value": 0.82,
  "attributes": {
    "suite_id": "coding-regression",
    "run_id": "run_20260528_1710",
    "case_id": "case_017",
    "metric": "correctness",
    "scorer": "unit-tests",
    "trace_id": "abc123",
    "label": "pass"
  }
}
```

This lets existing metric queries, SQL, summaries, and future downsampling keep
working without a new score store.

## Annotations as Comments

The MVP reuses `trace_comments` for notes, judge explanations, and lightweight
labels. Span-scoped comments already exist, so a scorer can attach feedback to a
specific failing step:

```bash
tael comment add <trace-id> \
  '{"kind":"eval_judge","case_id":"case_017","failure_mode":"missing_edit","summary":"Patch compiles but does not update the failing assertion."}' \
  --author eval:judge \
  --span-id <span-id>
```

Conventions:

- `author=eval:runner` for lifecycle notes.
- `author=eval:scorer` for deterministic scoring output.
- `author=eval:judge` for LLM or human judge rationales.
- `author=eval:human` for manual labels.
- JSON bodies are allowed but not required.

A later phase can add a structured `span_annotations` table if querying comment
bodies becomes limiting. The MVP should avoid that schema until it is clearly
needed.

## Inputs, Outputs, and Rationale Blobs

Small inputs and expected outputs can live in eval runner metadata, span
attributes, or comments. Large values should use existing content-addressed
blobs and reference hashes from span attributes or metric attributes:

| Attribute | Meaning |
|-----------|---------|
| `tael.eval.input_sha256` | Input/case payload blob hash. |
| `tael.eval.expected_sha256` | Expected output blob hash. |
| `tael.eval.actual_sha256` | Actual output blob hash. |
| `rationale_sha256` | Judge/scorer rationale blob hash on score metrics. |

This follows the same shape Tael already uses for LLM prompt/completion payloads
and oversized log bodies.

## CLI Surface

The eval CLI is mostly automation around existing ingestion and query surfaces.

```
tael eval run <cases.jsonl> --suite <name> --cmd <template> [--code-version <id>]
tael eval score <run-id> <scores.jsonl>
tael eval runs
tael eval status <run-id>
tael eval cases <run-id>
tael eval report <run-id>
tael eval compare <new-run-id> <baseline-run-id>
```

### `tael eval run`

Runs a command once per case and injects eval identity through environment
variables:

| Env var | Meaning |
|---------|---------|
| `TAEL_EVAL_SUITE_ID` | Suite name. |
| `TAEL_EVAL_RUN_ID` | Run ID. |
| `TAEL_EVAL_CASE_ID` | Case ID. |
| `TAEL_EVAL_CASE_INDEX` | Case display order. |
| `TAEL_EVAL_CASE_COUNT` | Total case count, if known. |
| `TAEL_EVAL_CODE_VERSION` | Git SHA/build ID. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Tael OTLP endpoint. |

The runner should also emit a root `tael-eval-runner` span per case so progress
is visible even if the evaluated program has sparse instrumentation.

### `tael eval score`

Accepts JSONL score records and writes `tael_eval_score` metric points. A score
record should minimally include:

```json
{"case_id":"case_017","trace_id":"abc123","metric":"correctness","value":1.0,"scorer":"unit-tests"}
```

If the score includes a long rationale, the CLI stores it as a blob and writes
`rationale_sha256` into metric attributes.

### Reports and Comparisons

Reports are canned SQL over existing tables. Example run summary:

```sql
SELECT
  json_extract_string(attributes, '$.metric') AS metric,
  AVG(value) AS avg_value,
  MIN(value) AS min_value,
  MAX(value) AS max_value,
  COUNT(*) AS scored_cases
FROM metrics
WHERE name = 'tael_eval_score'
  AND json_extract_string(attributes, '$.run_id') = 'run_20260528_1710'
GROUP BY metric
ORDER BY metric;
```

Example failed cases:

```sql
SELECT
  json_extract_string(attributes, '$.case_id') AS case_id,
  json_extract_string(attributes, '$.trace_id') AS trace_id,
  value AS correctness
FROM metrics
WHERE name = 'tael_eval_score'
  AND json_extract_string(attributes, '$.run_id') = 'run_20260528_1710'
  AND json_extract_string(attributes, '$.metric') = 'correctness'
  AND value < 1.0
ORDER BY value ASC, case_id ASC;
```

## REST API

The TUI and CLI should not embed SQL strings directly. Add small convenience
endpoints backed by existing `Store::query_sql` and trace/comment APIs:

```
GET /api/v1/evals/runs
GET /api/v1/evals/runs/{run_id}
GET /api/v1/evals/runs/{run_id}/cases
GET /api/v1/evals/runs/{run_id}/scores
GET /api/v1/evals/runs/{run_id}/compare?baseline=<run_id>
```

These endpoints are derived views. They do not require new storage tables.

### Run Status Response

```json
{
  "run_id": "run_20260528_1710",
  "suite_id": "coding-regression",
  "code_version": "9f31c4e",
  "status": "running",
  "case_count": 100,
  "observed_cases": 42,
  "scored_cases": 38,
  "passed_cases": 31,
  "failed_cases": 7,
  "avg_scores": {
    "correctness": 0.74
  },
  "cost_usd": 1.28,
  "started_at": "2026-05-28T17:10:00Z",
  "updated_at": "2026-05-28T17:18:12Z"
}
```

Status is inferred:

- `running` when recent spans exist and scored cases are less than known case
  count.
- `complete` when observed/scored cases meet known case count.
- `unknown` when case count is missing and no recent spans indicate activity.
- `failed` only when the runner emits a failure score/comment or exits through
  `tael eval run` with known failures.

## `tael live` Eval UI

Add an Evals tab to the existing TUI:

```
1:Traces  2:Services  3:Evals
```

Also support direct entry:

```bash
tael live --evals
tael live --eval-run run_20260528_1710
```

### Evals Tab Layout

```text
+- tael --------------------------------------------------------------+
| 1:Traces  2:Services  3:Evals                                      |
+---------------------------------------------------------------------+
| Run coding-regression / run_20260528_1710                          |
| status: running   cases: 42/100   pass: 31   fail: 7   pending: 58 |
| avg correctness: 0.74   cost: $1.28   elapsed: 8m12s               |
+---------------------------------------------------------------------+
| Cases                                                               |
| PASS case_001  correctness 1.00  2.1s   $0.02  trace a1b2c3        |
| FAIL case_002  correctness 0.00  5.8s   $0.09  trace d4e5f6        |
| RUN  case_043  running       -   1.4s      -    trace 98ab12       |
| WAIT case_044  pending       -     -       -       -               |
+---------------------------------------------------------------------+
| Selected case                                                       |
| failure_mode: missing_edit                                          |
| judge: solution compiles but does not fix assertion failure         |
| Enter: open trace   c:comment   r:refresh   f:failures only        |
+---------------------------------------------------------------------+
```

The renderer may use compact symbolic statuses when terminal support is good,
but must keep ASCII fallbacks:

| Status | Symbolic label | ASCII |
|--------|----------------|-------|
| pass | check | `PASS` |
| fail | x | `FAIL` |
| running | ellipsis | `RUN` |
| pending | dot | `WAIT` |

### Interactions

| Key | Action |
|-----|--------|
| `j` / `k` | Move through cases. |
| `Enter` | Open selected case in the existing trace waterfall view. |
| `f` | Toggle failures/regressions only. |
| `s` | Cycle sort: case ID, score, duration, cost, status. |
| `r` | Refresh immediately. |
| `c` | Add a trace or span comment using existing comment flow. |
| `Tab` | Move focus between run summary, case list, and score breakdown. |
| `Esc` | Return from trace view to eval case list. |

### Data Loading

The TUI should poll the eval convenience endpoints on the same interval style as
the existing live views:

1. `GET /api/v1/evals/runs` to list recent runs.
2. `GET /api/v1/evals/runs/{run_id}` for summary.
3. `GET /api/v1/evals/runs/{run_id}/cases` for rows.
4. Existing `GET /api/v1/traces/{trace_id}` for selected case details.
5. Existing comments API for judge notes and manual labels.

Do not make the eval UI a separate dashboard. It is a navigation layer from eval
progress into existing trace/span details.

## Implementation Plan

### Phase 0: Conventions and Query Views

- Document reserved attributes and `tael_eval_score`.
- Add helper functions that derive eval runs/cases from `spans` and `metrics`.
- Add REST endpoints for run list, run status, case list, scores, and compare.
- Add CLI read commands: `eval runs`, `eval status`, `eval cases`, `eval report`,
  and `eval compare`.

No storage schema changes.

### Phase 1: Runner and Score Ingestion

- Add `tael eval run` for JSONL cases and command templating.
- Set eval env vars and OTLP endpoint for child processes.
- Emit runner spans so progress is visible even for poorly instrumented code.
- Add `tael eval score` to convert JSONL score records into metric points and
  comment/blob records.

Still no dedicated eval tables.

### Phase 2: Live Eval UI

- Add Evals tab state to `tael-cli/src/tui.rs`.
- Poll eval endpoints.
- Render run summary, case table, and selected-case notes.
- Reuse the existing trace waterfall for Enter/open.
- Reuse the existing comment flow for labels and notes.

### Phase 3: Optional Schema

Only add dedicated tables if the convention layer becomes insufficient.
Candidate tables:

```
eval_runs(id, suite_id, code_version, command, status, started_at, finished_at, metadata)
eval_cases(id, suite_id, input_sha256, expected_sha256, metadata)
span_annotations(id, trace_id, span_id, kind, key, value_json, author, created_at)
```

Do not add a dedicated score table until metrics prove inadequate. Scores are
time-series facts and fit the existing metrics model well.

## Open Questions

1. Should `tael eval run` own parallelism, retries, and timeouts, or should it
   remain a thin wrapper around external runners?
2. Should score ingestion happen through the existing OTLP metrics path, an
   internal store method, or a small REST endpoint?
3. How much case metadata should be required for `pending` visibility before a
   case emits any span?
4. Should comparison use exact `case_id` matching only, or support aliases when
   suites evolve?
5. Should comments with JSON bodies get lightweight field extraction before a
   dedicated `span_annotations` table exists?

## Decision

Start with evals as conventions over existing telemetry:

- spans identify eval execution,
- metrics hold scores,
- comments hold annotations and rationale,
- blobs hold large artifacts,
- SQL powers reports,
- `tael live` gives an eval-specific view over the same data.

This keeps Tael evals aligned with Tael's core product shape: agent-native
observability where the trace remains the execution truth.
