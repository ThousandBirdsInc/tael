#!/usr/bin/env bash
# Generate a demo.cast with proper timing from actual command output
set -e

TAEL="./target/debug/tael"
SERVER="./target/debug/tael-server"
TEST="./target/debug/tael-test"
CAST="demo.cast"

# Ensure built
cargo build --quiet 2>/dev/null

# Clean state
pkill -f tael-server 2>/dev/null || true
rm -rf ./data
sleep 1

# Start server
$SERVER &
SERVER_PID=$!
sleep 4

# Ingest data
$TEST 2>/dev/null

# Get a trace ID for the error trace
TRACE_ID=$($TAEL query traces --status error --format json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['trace_id'])")

# Add comments
$TAEL comment add "$TRACE_ID" "Payment declined — Stripe returning 402 for this card" --author oncall-bot --format json >/dev/null 2>&1
$TAEL comment add "$TRACE_ID" "Customer contacted, card expired. Not a system issue." --author debug-agent --format json >/dev/null 2>&1

# Capture all outputs
OUT_STATUS=$($TAEL server status --format json 2>/dev/null)
OUT_TEST=$($TEST 2>&1)
OUT_SERVICES=$($TAEL services --format table 2>/dev/null)
OUT_TRACES=$($TAEL query traces --last 1h --format table --limit 10 2>/dev/null)
OUT_ERRORS=$($TAEL query traces --status error --format table 2>/dev/null)
OUT_SLOW=$($TAEL query traces --min-duration 500ms --format table 2>/dev/null)
OUT_SVC=$($TAEL query traces --service payment-service --format table 2>/dev/null)
OUT_TRACE=$($TAEL get trace "$TRACE_ID" --format json 2>/dev/null)
OUT_COMMENTS=$($TAEL comment list "$TRACE_ID" --format table 2>/dev/null)
OUT_COMMENTS_JSON=$($TAEL comment list "$TRACE_ID" --format json 2>/dev/null)

kill $SERVER_PID 2>/dev/null || true
rm -rf ./data

# --- Generate cast file ---
python3 - "$CAST" "$TRACE_ID" \
    "$OUT_STATUS" \
    "$OUT_TEST" \
    "$OUT_SERVICES" \
    "$OUT_TRACES" \
    "$OUT_ERRORS" \
    "$OUT_SLOW" \
    "$OUT_SVC" \
    "$OUT_TRACE" \
    "$OUT_COMMENTS" \
    "$OUT_COMMENTS_JSON" \
    <<'PYEOF'
import json, sys, os

cast_file = sys.argv[1]
trace_id = sys.argv[2]
out_status = sys.argv[3]
out_test = sys.argv[4]
out_services = sys.argv[5]
out_traces = sys.argv[6]
out_errors = sys.argv[7]
out_slow = sys.argv[8]
out_svc = sys.argv[9]
out_trace = sys.argv[10]
out_comments = sys.argv[11]
out_comments_json = sys.argv[12]

t = 0.0
events = []

def emit(text, dt=0.0):
    global t
    t += dt
    # Normalize line endings: replace \n with \r\n but avoid doubling
    text = text.replace('\r\n', '\n').replace('\n', '\r\n')
    events.append(json.dumps([round(t, 3), "o", text]))

def type_cmd(cmd, pause_after=0.1):
    emit(f"\x1b[1;32m❯\x1b[0m {cmd}\n", 0.15)
    emit("", pause_after)

def narrate(text, dt=0.4):
    emit(f"\n\x1b[1;36m# {text}\x1b[0m\n", dt)

def show_output(text, dt=0.2):
    emit(text + "\n", dt)

# --- Title ---
emit("\x1b[2J\x1b[H", 0.1)
title = """
\x1b[1;37m  ████████╗ █████╗ ███████╗██╗     \x1b[0m
\x1b[1;37m  ╚══██╔══╝██╔══██╗██╔════╝██║     \x1b[0m
\x1b[1;37m     ██║   ███████║█████╗  ██║     \x1b[0m
\x1b[1;37m     ██║   ██╔══██║██╔══╝  ██║     \x1b[0m
\x1b[1;37m     ██║   ██║  ██║███████╗███████╗\x1b[0m
\x1b[1;37m     ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝\x1b[0m

  \x1b[0;90mAI-agent-native observability\x1b[0m
"""
emit(title, 1.5)

# --- Start server ---
narrate("Start the tael server (OTLP gRPC :4317, REST API :7701)")
type_cmd("tael-server &")

narrate("Check server health")
type_cmd("tael server status --format json")
show_output(out_status, 0.5)

narrate("Ingest OpenTelemetry traces from sample microservices")
type_cmd("tael-test")
show_output(out_test, 0.5)

narrate("See which services are reporting")
type_cmd("tael services --format table")
show_output(out_services, 1.0)

narrate("Query recent traces across all services")
type_cmd("tael query traces --last 1h --format table --limit 10")
show_output(out_traces, 1.2)

narrate("Find error traces — what's broken?")
type_cmd("tael query traces --status error --format table")
show_output(out_errors, 1.0)

narrate("Find slow spans (>500ms) — where's the bottleneck?")
type_cmd("tael query traces --min-duration 500ms --format table")
show_output(out_slow, 1.0)

narrate("Drill into a specific service")
type_cmd("tael query traces --service payment-service --format table")
show_output(out_svc, 0.8)

narrate("Get the full trace — see the entire request flow as structured JSON")
type_cmd(f"tael get trace {trace_id} --format json")
show_output(out_trace, 1.5)

narrate("Agents can annotate traces with comments")
type_cmd(f"tael comment add {trace_id} 'Payment declined — Stripe 402' --author oncall-bot")
emit("Comment added by oncall-bot\n", 0.3)
type_cmd(f"tael comment add {trace_id} 'Card expired. Not a system issue.' --author debug-agent")
emit("Comment added by debug-agent\n", 0.3)

narrate("View the comment thread on this trace")
type_cmd(f"tael comment list {trace_id} --format table")
show_output(out_comments, 1.0)

narrate("JSON output — every command is machine-readable")
type_cmd(f"tael comment list {trace_id} --format json")
show_output(out_comments_json, 1.0)

# --- Outro ---
emit("\n", 0.1)
outro = """
\x1b[1;36m# ─────────────────────────────────────────────────────\x1b[0m

  \x1b[1;37mtael\x1b[0m — observability built for AI agents

  \x1b[0;90m• Ingests OpenTelemetry traces via standard OTLP gRPC\x1b[0m
  \x1b[0;90m• CLI-first: structured JSON output for agent workflows\x1b[0m
  \x1b[0;90m• DuckDB storage — zero dependencies, single binary\x1b[0m
  \x1b[0;90m• Comments for agent-to-agent collaboration on traces\x1b[0m
  \x1b[0;90m• Interactive TUI with waterfall trace visualization\x1b[0m
"""
emit(outro, 2.0)

# Write cast file
# Convert absolute timestamps to relative delays (v3 format)
header = json.dumps({"version": 3, "term": {"cols": 110, "rows": 42}, "title": "tael — AI-agent-native observability"})
with open(cast_file, 'w') as f:
    f.write(header + '\n')
    prev_t = 0.0
    for ev_str in events:
        ev = json.loads(ev_str)
        dt = ev[0] - prev_t
        prev_t = ev[0]
        f.write(json.dumps([round(dt, 3), ev[1], ev[2]]) + '\n')

print(f"Wrote {cast_file} ({len(events)} events, {t:.0f}s duration)")
PYEOF

echo "Done! Play with: asciinema play demo.cast"
