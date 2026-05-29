#!/usr/bin/env bash
# Generate .github/demo.cast with realistic timing from actual command output.
# Captures command output from the current tael binaries, then renders an
# asciinema v3 cast. The live TUI owns the terminal, so this script renders a
# representative live-mode screen from the captured data instead of recording an
# interactive session.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT_DIR"

TAEL="${TAEL_BIN:-$ROOT_DIR/target/debug/tael}"
TEST="${TEST_BIN:-$ROOT_DIR/target/debug/tael-test}"
CAST="${CAST:-$SCRIPT_DIR/demo.cast}"
RUN_ID="${TAEL_DEMO_RUN_ID:-demo-local-wiki-asciinema}"
REST_PORT="${TAEL_DEMO_REST_PORT:-7771}"
OTLP_PORT="${TAEL_DEMO_OTLP_PORT:-4377}"
SERVER_URL="http://127.0.0.1:${REST_PORT}"
OTLP_ENDPOINT="http://127.0.0.1:${OTLP_PORT}"
WORK_DIR=""
SERVER_PID=""

need() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "error: required command not found: $1" >&2
        exit 1
    fi
}

cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" >/dev/null 2>&1 || true
        wait "$SERVER_PID" >/dev/null 2>&1 || true
    fi
    if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
        rm -rf "$WORK_DIR"
    fi
}
trap cleanup EXIT

wait_for_server() {
    local i
    for i in $(seq 1 80); do
        local status
        status="$("$TAEL" server status --server "$SERVER_URL" --format json 2>/dev/null || true)"
        if [[ "$status" == *'"status":"healthy"'* ]]; then
            return 0
        fi
        sleep 0.25
    done

    echo "error: tael server did not become ready at $SERVER_URL" >&2
    echo "server log:" >&2
    tail -80 "$SERVER_LOG" >&2 || true
    exit 1
}

json_get() {
    python3 -c "$1"
}

need cargo
need python3
need uv

cargo build --quiet --bins

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tael-asciinema.XXXXXX")"
DATA_DIR="$WORK_DIR/data"
WAL_DIR="$WORK_DIR/wal"
SERVER_LOG="$WORK_DIR/tael-server.log"

"$TAEL" serve \
    --rest-api-addr "127.0.0.1:${REST_PORT}" \
    --otlp-grpc-addr "127.0.0.1:${OTLP_PORT}" \
    --data-dir "$DATA_DIR" \
    --wal-dir "$WAL_DIR" \
    >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
wait_for_server

OUT_TEST="$(TAEL_OTLP_GRPC_ADDR="$OTLP_ENDPOINT" "$TEST" 2>&1)"

ERR_JSON="$("$TAEL" query traces --status error --server "$SERVER_URL" --format json)"
ERR_TID="$(printf '%s' "$ERR_JSON" | json_get 'import sys,json; print(json.load(sys.stdin)["spans"][0]["trace_id"])')"
ERR_SPAN="$(printf '%s' "$ERR_JSON" | json_get 'import sys,json; print(json.load(sys.stdin)["spans"][0]["span_id"])')"

"$TAEL" comment add "$ERR_TID" "Stripe returned 402 - card declined, not a system fault" \
    --author oncall-bot --server "$SERVER_URL" --format json >/dev/null
"$TAEL" comment add "$ERR_TID" "Confirmed: customer card expired. Closing." \
    --author triage-agent --span-id "$ERR_SPAN" --server "$SERVER_URL" --format json >/dev/null

OUT_STATUS="$("$TAEL" server status --server "$SERVER_URL" --format json)"
OUT_HELP="$("$TAEL" --help 2>&1)"
OUT_SERVICES="$("$TAEL" services --server "$SERVER_URL" --format table)"
OUT_TRACES="$("$TAEL" query traces --last 1h --server "$SERVER_URL" --format table --limit 8)"
OUT_ERRORS="$("$TAEL" query traces --status error --server "$SERVER_URL" --format table)"
OUT_ATTR="$("$TAEL" query traces --attribute http.route=/checkout --server "$SERVER_URL" --format table)"
OUT_LLM="$("$TAEL" query traces --text "OTLP" --server "$SERVER_URL" --format table)"
OUT_LOGS="$("$TAEL" query logs --severity error --last 1h --server "$SERVER_URL" --format table || true)"
OUT_METRICS="$("$TAEL" query metrics --query 'sum by (service) (http_requests)' --server "$SERVER_URL" --format table || true)"
OUT_SUMMARY="$("$TAEL" summarize --last 1h --server "$SERVER_URL" --format table)"
OUT_SQL="$("$TAEL" query sql "SELECT service, COUNT(*) AS spans, ROUND(AVG(duration_ms),1) AS avg_ms FROM spans GROUP BY service ORDER BY spans DESC LIMIT 5" --server "$SERVER_URL" --format table)"
OUT_CORRELATE="$("$TAEL" correlate --trace "$ERR_TID" --server "$SERVER_URL" --format table)"
OUT_COMMENTS="$("$TAEL" comment list "$ERR_TID" --server "$SERVER_URL" --format table)"

OUT_ISSUE_CREATE="$("$TAEL" issue create --from-trace "$ERR_TID" --failure-mode payment_declined --impact medium --summary "Card decline is a user payment failure, not an outage" --server "$SERVER_URL" --format table)"
ISSUE_JSON="$("$TAEL" issue list --server "$SERVER_URL" --format json)"
ISSUE_ID="$(printf '%s' "$ISSUE_JSON" | json_get 'import sys,json; print(json.load(sys.stdin)["issues"][0]["issue_id"])')"
OUT_ISSUE_LIST="$("$TAEL" issue list --server "$SERVER_URL" --format table)"
OUT_CASE_ADD="$("$TAEL" eval case add --from-trace "$ERR_TID" --suite checkout-agent --case-id card-decline-001 --failure-mode payment_declined --source-issue-id "$ISSUE_ID" --critical-path --expected-behavior "Explains card decline without retrying payment processor" --server "$SERVER_URL" --format table)"
OUT_SUITE="$("$TAEL" eval suite inspect checkout-agent --server "$SERVER_URL" --format table)"
OUT_SIGNAL="$("$TAEL" signal create --from-trace "$ERR_TID" --name payment_declined --failure-mode payment_declined --summary "Tracks user payment declines separately from service errors" --server "$SERVER_URL" --format table)"
OUT_DIAG="$("$TAEL" diagnose report --trace-id "$ERR_TID" --category missing_context --severity low --confidence low --summary "Agent should check payment error taxonomy before escalating" --server "$SERVER_URL" --format table)"

TAEL_BIN="$TAEL" \
TAEL_SERVER="$SERVER_URL" \
TAEL_OTLP_ENDPOINT="$OTLP_ENDPOINT" \
TAEL_DEMO_RUN_ID="$RUN_ID" \
    "$ROOT_DIR/demo/tael-evals-agent/run_demo.sh" >/dev/null

OUT_EVAL_RUNS="$("$TAEL" eval runs --server "$SERVER_URL" --format table)"
OUT_EVAL_REPORT="$("$TAEL" eval report "$RUN_ID" --server "$SERVER_URL" --format table)"
OUT_EVAL_SCORES="$("$TAEL" eval scores "$RUN_ID" --server "$SERVER_URL" --format table)"
OUT_SKILL="$("$TAEL" skill where 2>/dev/null)"

python3 - "$CAST" "$ERR_TID" "$ERR_SPAN" "$RUN_ID" \
    "$OUT_STATUS" "$OUT_HELP" "$OUT_TEST" "$OUT_SERVICES" "$OUT_TRACES" \
    "$OUT_ERRORS" "$OUT_ATTR" "$OUT_LLM" "$OUT_LOGS" "$OUT_METRICS" \
    "$OUT_SUMMARY" "$OUT_SQL" "$OUT_CORRELATE" "$OUT_COMMENTS" \
    "$OUT_ISSUE_CREATE" "$OUT_ISSUE_LIST" "$OUT_CASE_ADD" "$OUT_SUITE" \
    "$OUT_SIGNAL" "$OUT_DIAG" "$OUT_EVAL_RUNS" "$OUT_EVAL_REPORT" \
    "$OUT_EVAL_SCORES" "$OUT_SKILL" <<'PYEOF'
import json
import re
import sys
import textwrap

(
    cast_file,
    err_tid,
    err_span,
    run_id,
    out_status,
    out_help,
    out_test,
    out_services,
    out_traces,
    out_errors,
    out_attr,
    out_llm,
    out_logs,
    out_metrics,
    out_summary,
    out_sql,
    out_correlate,
    out_comments,
    out_issue_create,
    out_issue_list,
    out_case_add,
    out_suite,
    out_signal,
    out_diag,
    out_eval_runs,
    out_eval_report,
    out_eval_scores,
    out_skill,
) = sys.argv[1:29]

short_tid = err_tid[:12] + "..."
short_span = err_span[:12] + "..."

t = 0.0
events = []


def emit(text, dt=0.0):
    global t
    t += dt
    text = text.replace("\r\n", "\n").replace("\n", "\r\n")
    events.append([round(t, 3), "o", text])


def type_cmd(cmd, pause_after=0.35):
    emit(f"\x1b[1;32m>\x1b[0m {cmd}\n", 0.18)
    emit("", pause_after)


def narrate(text, dt=0.5):
    emit(f"\n\x1b[1;36m# {text}\x1b[0m\n", dt)


def show(text, dt=0.25, max_lines=None):
    lines = text.splitlines()
    if max_lines and len(lines) > max_lines:
        lines = lines[:max_lines] + ["..."]
    emit("\n".join(lines) + "\n", dt)


def strip_ansi(text):
    return re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)


def tui_screen(title, rows, footer):
    clean_rows = [strip_ansi(r) for r in rows if r.strip()]
    body = []
    body.append("┌" + "─" * 118 + "┐")
    body.append(f"│ {title:<116} │")
    body.append("├" + "─" * 118 + "┤")
    for line in clean_rows[:22]:
        body.append(f"│ {line[:116]:<116} │")
    while len(body) < 27:
        body.append(f"│ {'':<116} │")
    body.append("├" + "─" * 118 + "┤")
    body.append(f"│ {footer:<116} │")
    body.append("└" + "─" * 118 + "┘")
    return "\x1b[2J\x1b[H" + "\n".join(body) + "\n"


title = r"""
  ████████╗ █████╗ ███████╗██╗
  ╚══██╔══╝██╔══██╗██╔════╝██║
     ██║   ███████║█████╗  ██║
     ██║   ██╔══██║██╔══╝  ██║
     ██║   ██║  ██║███████╗███████╗
     ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝

  AI-agent-native observability: one tael-cli install, one tael binary
"""

emit("\x1b[2J\x1b[H", 0.1)
emit(title, 1.3)

narrate("Install the published package: the crate is tael-cli, the binary is tael.")
type_cmd("cargo binstall tael-cli")
show("Resolved package: tael-cli\nInstalled binary: tael\nRun `tael serve` to start the bundled server.", 0.5)

narrate("The current CLI exposes server, query, live, eval, and reliability commands.")
type_cmd("tael --help")
show(out_help, 0.5, max_lines=24)

narrate("Start the embedded tael server: REST API + OTLP ingest + local storage.")
type_cmd("tael serve")
emit("tael server starting\r\n  REST API     http://127.0.0.1:7701\r\n  OTLP gRPC    127.0.0.1:4317\r\n  storage      tael-backend\r\n", 0.6)

narrate("Health check: commands default to structured JSON for agents.")
type_cmd("tael server status --format json")
show(out_status, 0.5)

narrate("Seed OpenTelemetry traces from sample services and a gen_ai LLM span.")
type_cmd("tael-test")
show(out_test.replace("http://127.0.0.1:4377", "http://127.0.0.1:4317"), 0.7)

narrate("Service health is available without opening a dashboard.")
type_cmd("tael services --format table")
show(out_services, 0.9)

narrate("Query traces, filter by status, attributes, and LLM payload text.")
type_cmd("tael query traces --status error --format table")
show(out_errors, 0.8)
type_cmd("tael query traces --attribute http.route=/checkout --format table")
show(out_attr, 0.8)
type_cmd('tael query traces --text "OTLP" --format table')
show(out_llm, 0.9)

narrate("Cross-signal commands cover logs, metrics, SQL, summaries, and correlation.")
type_cmd("tael query logs --severity error --last 1h --format table")
show(out_logs, 0.7, max_lines=12)
type_cmd("tael query metrics --query 'sum by (service) (http_requests)' --format table")
show(out_metrics, 0.7, max_lines=12)
type_cmd("tael summarize --last 1h --format table")
show(out_summary, 1.0)
type_cmd('tael query sql "SELECT service, COUNT(*) AS spans, ROUND(AVG(duration_ms),1) AS avg_ms FROM spans GROUP BY service ORDER BY spans DESC LIMIT 5"')
show(out_sql, 0.9)
type_cmd(f"tael correlate --trace {short_tid} --format table")
show(out_correlate, 0.9, max_lines=22)

narrate("The live TUI streams traces as they arrive and lets you drill into waterfalls.")
type_cmd("tael live --service api-gateway --interval 1")
live_rows = [
    "Live Traces                    Service       Operation               Duration   Status",
    "-------------------------------------------------------------------------------",
]
for line in strip_ansi(out_traces).splitlines():
    if "api-gateway" in line or "Trace ID" in line:
        live_rows.append(line)
live_rows += [
    "",
    "Waterfall: HTTP POST /checkout",
    "api-gateway        |██████████████████████████████████| 340ms error",
    "payment-service    |      ███████████████████████████ | 310ms error",
    "comments: 2    logs: 1    metrics: 3",
]
emit(tui_screen("tael live  traces | services | waterfall", live_rows, "j/k move  Enter open trace  f filter  Space pause  q quit"), 1.1)
emit("q", 1.2)
emit("\x1b[2J\x1b[H", 0.2)

narrate("Agents can annotate traces, then promote failures into issues and regression cases.")
type_cmd(f"tael comment list {short_tid} --format table")
show(out_comments, 0.8)
type_cmd(f'tael issue create --from-trace {short_tid} --failure-mode payment_declined --impact medium --summary "Card decline is a user payment failure, not an outage"')
show(out_issue_create, 0.6)
type_cmd("tael issue list --format table")
show(out_issue_list, 0.8)
type_cmd(f"tael eval case add --from-trace {short_tid} --suite checkout-agent --case-id card-decline-001 --failure-mode payment_declined --source-issue-id <issue-id> --critical-path")
show(out_case_add, 0.6)
type_cmd("tael eval suite inspect checkout-agent --format table")
show(out_suite, 0.9)

narrate("Long-running signals and self diagnostics stay trace-linked too.")
type_cmd(f"tael signal create --from-trace {short_tid} --name payment_declined --failure-mode payment_declined")
show(out_signal, 0.6)
type_cmd(f'tael diagnose report --trace-id {short_tid} --category missing_context --severity low --confidence low --summary "Agent should check payment error taxonomy before escalating"')
show(out_diag, 0.6)

narrate("Trace-native evals run cases, score them as metrics, and report progress from the same server.")
type_cmd("demo/tael-evals-agent/run_demo.sh")
show(f"Running demo eval: {run_id}\neval run {run_id}: complete\n", 0.7)
type_cmd("tael eval runs --format table")
show(out_eval_runs, 0.8)
type_cmd(f"tael eval report {run_id} --format table")
show(out_eval_report, 1.0, max_lines=28)
type_cmd(f"tael eval scores {run_id} --format table")
show(out_eval_scores, 0.9, max_lines=20)

narrate("Live mode has an eval progress view for runs that are still executing.")
type_cmd(f"tael live --eval-run {run_id} --interval 1")
eval_rows = [
    f"Eval run: {run_id}",
    "Suite: local-wiki-demo     Status: complete     Cases: 5/5     Scored: 5",
    "",
]
eval_rows.extend(strip_ansi(out_eval_report).splitlines()[:18])
eval_rows += [
    "",
    "Case trace drilldown: open a case to inspect runner span + nested agent spans",
]
emit(tui_screen("tael live --eval-run  evals | cases | trace", eval_rows, "1 traces  2 services  3 evals  Enter open case trace  q quit"), 1.1)
emit("q", 1.1)
emit("\x1b[2J\x1b[H", 0.2)

narrate("The Claude Code skill teaches agents to use these JSON commands first.")
type_cmd("tael skill where")
show(out_skill, 0.7)

emit("\n", 0.1)
emit(textwrap.dedent(f"""
    \x1b[1;36m# -----------------------------------------------------\x1b[0m

      \x1b[1;37mtael\x1b[0m

      \x1b[0;90m- Install package: tael-cli, binary: tael\x1b[0m
      \x1b[0;90m- `tael serve` runs the bundled server and storage engine\x1b[0m
      \x1b[0;90m- OTLP traces, logs, metrics, SQL, comments, and correlation\x1b[0m
      \x1b[0;90m- Reliability loop: issues, signals, eval cases, diagnostics\x1b[0m
      \x1b[0;90m- Trace-native evals with `tael live --eval-run {run_id}`\x1b[0m
    """), 1.8)

header = json.dumps({"version": 3, "term": {"cols": 132, "rows": 44}, "title": "tael-cli and tael live demo"})
with open(cast_file, "w") as f:
    f.write(header + "\n")
    prev = 0.0
    for ts, code, data in events:
        f.write(json.dumps([round(ts - prev, 3), code, data]) + "\n")
        prev = ts

print(f"Wrote {cast_file} ({len(events)} events, {t:.0f}s)")
PYEOF

echo "Done. Play with: asciinema play $CAST"
