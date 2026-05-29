# Tael Evals Agent Demo

This demo reworks the Wikipedia-agent take-home into a small, deterministic
agent that shows how Tael can collect eval progress, scores, comments, and
trace-linked reports.

It intentionally has no external model API dependency. The agent searches a
tiny local "wiki" corpus, answers a question, exports OpenTelemetry spans to
Tael, and the eval scorer reads those spans back from Tael to emit
`tael_eval_score` records.

## Run

From the Tael repo root:

```bash
# Terminal 1
cargo run --bin tael -- serve --data-dir /tmp/tael-evals-demo

# Terminal 2
./demo/tael-evals-agent/run_demo.sh
```

While it runs:

```bash
cargo run --bin tael -- live --evals
```

Afterward:

```bash
cargo run --bin tael -- eval runs --format table
cargo run --bin tael -- eval report demo-local-wiki --format table
```

## What It Demonstrates

- `tael eval run` creates runner spans with `tael.eval.*` attributes.
- The demo agent exports nested OpenTelemetry spans to Tael's OTLP gRPC ingest.
- `evals/score_case.py` grades answer/search/calibration/overall.
- `tael eval score` ingests score JSONL as normal metrics.
- `tael live --evals` shows run progress and opens case traces.

The code is deliberately small so it is easy to edit during demos.

The Python side is a `uv` project. `run_demo.sh` uses `uv run --project
demo/tael-evals-agent ...`, so the OpenTelemetry SDK dependencies are resolved
from `pyproject.toml`.
