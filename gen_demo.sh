#!/usr/bin/env bash
# Generate demo.cast with realistic timing from actual command output.
# Captures live output from the binaries, then renders an asciinema v3 cast.
set -e

TAEL="./target/debug/tael"
TEST="./target/debug/tael-test"
CAST="demo.cast"

cargo build --quiet 2>/dev/null

# Clean state — single binary now: `tael serve`
pkill -f "tael serve" 2>/dev/null || true
pkill -f tael-server 2>/dev/null || true
rm -rf ./data
sleep 1

# Start the server (server + client live in the one `tael` binary)
$TAEL serve >/tmp/tael-demo-serve.log 2>&1 &
SERVER_PID=$!
sleep 4

# Ingest sample OTel traces (microservices + a gen_ai LLM chat)
$TEST 2>/dev/null

# Trace + span IDs we reference below
ERR_TID=$($TAEL query traces --status error --format json 2>/dev/null \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['trace_id'])")
ERR_SPAN=$($TAEL query traces --status error --format json 2>/dev/null \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['span_id'])")

# Seed comments (one attached to the failing span)
$TAEL comment add "$ERR_TID" "Stripe returned 402 — card declined, not a system fault" \
    --author oncall-bot --format json >/dev/null 2>&1
$TAEL comment add "$ERR_TID" "Confirmed: customer card expired. Closing." \
    --author triage-agent --span-id "$ERR_SPAN" --format json >/dev/null 2>&1

# Capture every output we'll replay
OUT_STATUS=$($TAEL server status --format json 2>/dev/null)
OUT_TEST=$($TEST 2>&1)
OUT_SERVICES=$($TAEL services --format table 2>/dev/null)
OUT_TRACES=$($TAEL query traces --last 1h --format table --limit 10 2>/dev/null)
OUT_ERRORS=$($TAEL query traces --status error --format table 2>/dev/null)
OUT_SLOW=$($TAEL query traces --min-duration 500ms --format table 2>/dev/null)
OUT_ATTR=$($TAEL query traces --attribute http.route=/checkout --format table 2>/dev/null)
OUT_LLM=$($TAEL query traces --text "OTLP" --format table 2>/dev/null)
OUT_TRACE=$($TAEL get trace "$ERR_TID" --format json 2>/dev/null)
OUT_SUMMARY=$($TAEL summarize --last 1h --format table 2>/dev/null)
OUT_SQL=$($TAEL query sql "SELECT service, COUNT(*) AS spans, ROUND(AVG(duration_ms),1) AS avg_ms FROM spans GROUP BY service ORDER BY spans DESC LIMIT 5" --format table 2>/dev/null)
OUT_CORRELATE=$($TAEL correlate --trace "$ERR_TID" --format table 2>/dev/null)
OUT_COMMENTS=$($TAEL comment list "$ERR_TID" --format table 2>/dev/null)
OUT_SKILL=$($TAEL skill where 2>/dev/null)

kill $SERVER_PID 2>/dev/null || true
rm -rf ./data

# --- Render the cast ---
python3 - "$CAST" "$ERR_TID" "$ERR_SPAN" \
    "$OUT_STATUS" "$OUT_TEST" "$OUT_SERVICES" "$OUT_TRACES" "$OUT_ERRORS" \
    "$OUT_SLOW" "$OUT_ATTR" "$OUT_LLM" "$OUT_TRACE" "$OUT_SUMMARY" "$OUT_SQL" \
    "$OUT_CORRELATE" "$OUT_COMMENTS" "$OUT_SKILL" \
    <<'PYEOF'
import json, sys

(cast_file, err_tid, err_span, out_status, out_test, out_services, out_traces,
 out_errors, out_slow, out_attr, out_llm, out_trace, out_summary, out_sql,
 out_correlate, out_comments, out_skill) = sys.argv[1:18]

short_tid = err_tid[:12] + "…"
short_span = err_span[:12] + "…"

t = 0.0
events = []

def emit(text, dt=0.0):
    global t
    t += dt
    text = text.replace('\r\n', '\n').replace('\n', '\r\n')
    events.append([round(t, 3), "o", text])

def type_cmd(cmd, pause_after=0.35):
    emit(f"\x1b[1;32m❯\x1b[0m {cmd}\n", 0.18)
    emit("", pause_after)

def narrate(text, dt=0.5):
    emit(f"\n\x1b[1;36m# {text}\x1b[0m\n", dt)

def show(text, dt=0.25):
    emit(text + "\n", dt)

# --- Title ---
emit("\x1b[2J\x1b[H", 0.1)
emit("""
\x1b[1;37m  ████████╗ █████╗ ███████╗██╗     \x1b[0m
\x1b[1;37m  ╚══██╔══╝██╔══██╗██╔════╝██║     \x1b[0m
\x1b[1;37m     ██║   ███████║█████╗  ██║     \x1b[0m
\x1b[1;37m     ██║   ██╔══██║██╔══╝  ██║     \x1b[0m
\x1b[1;37m     ██║   ██║  ██║███████╗███████╗\x1b[0m
\x1b[1;37m     ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝\x1b[0m

  \x1b[0;90mAI-agent-native observability · OTLP traces · logs · metrics\x1b[0m
""", 1.6)

narrate("One binary — server + client. `tael serve` runs OTLP gRPC :4317,")
narrate("the REST API :7701, and the tiered storage engine (hot→warm→cold).", 0.2)
type_cmd("tael serve &")
emit("\x1b[0;90m[tael] tael-backend storage ready · OTLP :4317 · API :7701\x1b[0m\n", 0.5)

narrate("Health check — every command speaks JSON by default")
type_cmd("tael server status --format json")
show(out_status, 0.5)

narrate("Ingest OpenTelemetry traces from sample microservices + an LLM call")
type_cmd("tael-test")
show(out_test, 0.5)

narrate("Which services are reporting, and how healthy are they?")
type_cmd("tael services --format table")
show(out_services, 1.0)

narrate("Recent traces across every service")
type_cmd("tael query traces --last 1h --format table --limit 10")
show(out_traces, 1.2)

narrate("What's broken? Filter to error traces")
type_cmd("tael query traces --status error --format table")
show(out_errors, 1.0)

narrate("Where's the latency? Spans slower than 500ms")
type_cmd("tael query traces --min-duration 500ms --format table")
show(out_slow, 1.0)

narrate("NEW — filter by any span attribute (repeatable)")
type_cmd("tael query traces --attribute http.route=/checkout --format table")
show(out_attr, 1.0)

narrate("NEW — LLM observability: full-text search over gen_ai prompt/completion")
narrate("payloads. Typed spans carry model, token counts, and cost.", 0.2)
type_cmd('tael query traces --text "OTLP" --format table')
show(out_llm, 1.2)

narrate("Pull the full failing trace — span tree, attributes, events — as JSON")
type_cmd(f"tael get trace {short_tid} --format json")
show(out_trace, 1.5)

narrate("NEW — one-shot health digest over a window (traces, services, errors,")
narrate("log severity, metric volume) — built for an agent to read in a glance.", 0.2)
type_cmd("tael summarize --last 1h --format table")
show(out_summary, 1.5)

narrate("NEW — read-only SQL straight over the telemetry tables")
type_cmd('tael query sql "SELECT service, COUNT(*) AS spans, '
         'ROUND(AVG(duration_ms),1) AS avg_ms FROM spans '
         'GROUP BY service ORDER BY spans DESC LIMIT 5"')
show(out_sql, 1.2)

narrate("NEW — correlate stitches one trace across spans, logs, and metrics")
type_cmd(f"tael correlate --trace {short_tid} --format table")
show(out_correlate, 1.2)

narrate("Agents annotate traces — and can pin a note to a specific span")
type_cmd(f"tael comment add {short_tid} 'Stripe 402 — card declined' --author oncall-bot")
emit("\x1b[0;90mcomment added by oncall-bot\x1b[0m\n", 0.3)
type_cmd(f"tael comment add {short_tid} 'Card expired. Closing.' "
         f"--author triage-agent --span-id {short_span}")
emit("\x1b[0;90mcomment added by triage-agent (span "
     f"{short_span})\x1b[0m\n", 0.3)
type_cmd(f"tael comment list {short_tid} --format table")
show(out_comments, 1.0)

narrate("Wire it into Claude Code — the skill auto-loads when you debug here")
type_cmd("tael skill where")
show(out_skill, 0.8)

# --- Outro ---
emit("\n", 0.1)
emit("""
\x1b[1;36m# ─────────────────────────────────────────────────────\x1b[0m

  \x1b[1;37mtael\x1b[0m — observability built for AI agents

  \x1b[0;90m• Single binary: `tael serve` is server + client + storage\x1b[0m
  \x1b[0;90m• OTLP traces, logs & metrics + Prometheus remote-write\x1b[0m
  \x1b[0;90m• Tiered storage engine (hot→warm→cold) — no external deps\x1b[0m
  \x1b[0;90m• Typed LLM spans (gen_ai.*): model/token/cost + payload search\x1b[0m
  \x1b[0;90m• summarize · anomalies · correlate — agent-ready analysis\x1b[0m
  \x1b[0;90m• Read-only SQL over the telemetry tables\x1b[0m
  \x1b[0;90m• `tael live` TUI: waterfall + fullscreen span viewer\x1b[0m
""", 2.0)

# v3 cast: relative delays
header = json.dumps({"version": 3, "term": {"cols": 132, "rows": 44},
                     "title": "tael — AI-agent-native observability"})
with open(cast_file, 'w') as f:
    f.write(header + '\n')
    prev = 0.0
    for ts, code, data in events:
        f.write(json.dumps([round(ts - prev, 3), code, data]) + '\n')
        prev = ts

print(f"Wrote {cast_file} ({len(events)} events, {t:.0f}s)")
PYEOF

echo "Done! Play with: asciinema play demo.cast"
