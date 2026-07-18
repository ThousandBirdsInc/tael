# Tracing Chidori agents

[Chidori](https://github.com/ThousandBirds/chidori) is the agent framework
where every run is durable and replayable by default. It emits standard OTLP
spans — which makes tael and Chidori two views of the same object: **a tael
trace is a pointer to a replayable Chidori run.**

## Quickstart

```bash
# Terminal 1
tael serve                       # OTLP gRPC :4317, REST API :7701

# Terminal 2
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
chidori run agent.ts --input task="..."
```

Every host call the agent makes (LLM prompts, tools, HTTP, branches) arrives
as a span, nested exactly as the run's durable call log records it. Prompt
spans carry `gen_ai.request.model` and `gen_ai.usage.*` (including prompt-cache
creation/read tokens), tool spans carry tael's typed `tool.*` fields.

```bash
tael query traces --last 10m --format table
tael query traces --attribute chidori.run_id=<run-id>
tael get trace <trace-id> --format table
```

`tael get trace` prints a correlation footer for Chidori traces:

```text
Chidori run: 2846360f-980a-4338-8af4-b480640bb8f4
Checkpoint:  examples/branching/.chidori/runs/2846360f-…
Branches:    draft-direct, outline-first
Compare:     tael experiment compare 2846360f-…
Replay ($0): chidori resume <agent.ts> 2846360f-… --ci
```

## Correlation attributes

| Attribute | Where | Use |
|---|---|---|
| `chidori.run_id` | every span | `--attribute` filter; `chidori resume <agent.ts> <run_id>` replays the run at $0 |
| `chidori.checkpoint_path` | run root span | The replayable artifact on disk |
| `chidori.branch_label` / `chidori.branch_id` | branch-variant spans | `tael experiment compare <run_id>` compares a `chidori.branch` A/B per variant |
| `chidori.prompt.request_digest` | prompt spans | Content-addressed join key for the same prompt across runs |

## Branch fan-outs are experiments

A `chidori.branch` fork stamps each variant's spans with its label, so
experiment comparison needs no `tael.experiment.*` instrumentation — the run id
is the experiment id:

```bash
tael experiment compare <chidori-run-id> --format table
tael experiment compare <chidori-run-id> --signal tool_error --last 24h
```

## Golden cases backed by checkpoints

`tael eval case add --from-trace` promotes a production failure into a
regression case. When the trace carries `chidori.run_id`, the case records the
run id and checkpoint path — the fixture is not a description of the failed
run, it is the failed run itself.

```bash
tael eval case add --from-trace <trace-id> --suite my-agent \
  --case-id <chidori-run-id> --failure-mode tool_error \
  --expected-behavior "retries with backoff instead of failing"
```

### `--cmd` recipes for `tael eval run`

Use the Chidori run id as the case id so `{case_id}` substitutes directly.

**Regression (exact replay).** Byte-identical, $0, milliseconds. Any
divergence — the agent no longer makes the recorded calls with the recorded
arguments — exits 3 and fails the case:

```bash
tael eval run cases.jsonl --suite my-agent \
  --cmd 'chidori resume agent.ts {case_id} --ci'
```

`chidori resume --ci` prints a JSON report (status, first mismatching call,
`live_cost_usd: 0.0`) and uses stable exit codes: 0 match, 3 diverged, 1 error.

**Live re-test (semantic).** Re-executes against the current agent source —
either re-run a stored branch variant fresh from its anchored fork state, or a
fresh run seeded from the case input:

```bash
tael eval run cases.jsonl --suite my-agent \
  --cmd 'chidori branch-rerun {case_id} <branch-id>'

tael eval run cases.jsonl --suite my-agent \
  --cmd 'chidori run agent.ts --input @inputs/{case_id}.json'
```

**Archiving fixtures.** A case's checkpoint can be committed to git or stored
as a blob without knowing Chidori's runs layout:

```bash
chidori checkpoint export <run-id>                 # <run-id>.chidori-run.tar.gz
chidori checkpoint import <archive> --dir ci/      # restore for CI replay
```

## The self-harness loop

The full loop — weakness mining in tael, harness proposal + controlled
branch experiments in Chidori, validation with checkpoint-backed eval suites,
`tael signal trend` as the guard — has a runnable end-to-end demo:
[`chidori/examples/self-harness-loop/`](https://github.com/ThousandBirds/chidori/tree/main/examples/self-harness-loop).

```text
            ┌──────────────── tael ────────────────┐
            │  traces · issues · signals · evals   │
            │  (weakness mining + validation)      │
            └───────▲──────────────────┬───────────┘
             OTLP   │                  │ eval run --cmd
                    │                  ▼
            ┌───────┴───────── chidori ────────────┐
            │  durable runs · checkpoints · branch │
            │  (the experiment substrate)          │
            └──────────────────────────────────────┘
```
