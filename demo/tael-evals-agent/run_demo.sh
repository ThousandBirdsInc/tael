#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEMO="$ROOT/demo/tael-evals-agent"
RUN_ID="${TAEL_DEMO_RUN_ID:-demo-local-wiki}"
SUITE_ID="local-wiki-demo"
SCORES="$DEMO/out/scores.jsonl"
TAEL_BIN="${TAEL_BIN:-$ROOT/target/debug/tael}"
UV_CMD=(uv run --project "$DEMO")

if [[ -x "$TAEL_BIN" ]]; then
  TAEL_CMD=("$TAEL_BIN")
else
  TAEL_CMD=(cargo run --quiet --bin tael --)
fi

rm -rf "$DEMO/out"
mkdir -p "$DEMO/out"

echo "Running demo eval: $RUN_ID"

"${TAEL_CMD[@]}" eval run "$DEMO/cases.jsonl" \
  --run-id "$RUN_ID" \
  --suite "$SUITE_ID" \
  --cmd "uv run --project $DEMO python $DEMO/agent/simple_agent.py --case-id '{case_id}' --cases-file $DEMO/cases.jsonl" \
  --server "${TAEL_SERVER:-http://127.0.0.1:7701}" \
  --otlp-endpoint "${TAEL_OTLP_ENDPOINT:-http://127.0.0.1:4317}"

while IFS= read -r case_json; do
  case_id="$("${UV_CMD[@]}" python -c 'import json,sys; print(json.loads(sys.argv[1])["case_id"])' "$case_json")"
  export TAEL_EVAL_RUN_ID="$RUN_ID"
  export TAEL_EVAL_SUITE_ID="$SUITE_ID"
  export TAEL_EVAL_CASE_ID="$case_id"
  "${UV_CMD[@]}" python "$DEMO/evals/score_case.py" \
    --case-json "$case_json" \
    --run-id "$RUN_ID" \
    --suite-id "$SUITE_ID" \
    --server "${TAEL_SERVER:-http://127.0.0.1:7701}" \
    --scores-out "$SCORES" \
    >/dev/null
done < "$DEMO/cases.jsonl"

"${TAEL_CMD[@]}" eval score "$RUN_ID" "$SCORES" \
  --server "${TAEL_SERVER:-http://127.0.0.1:7701}" \
  --format table

echo
"${TAEL_CMD[@]}" eval report "$RUN_ID" \
  --server "${TAEL_SERVER:-http://127.0.0.1:7701}" \
  --format table
