#!/usr/bin/env bash
# tael live demo — record with:  asciinema rec demo.cast -c ./demo.sh
# (gen_demo.sh renders a cast with cleaner timing; this one runs for real.)
set -e

type_cmd() {
    local cmd="$1"
    printf "\033[1;32m❯\033[0m "
    for (( i=0; i<${#cmd}; i++ )); do printf "%s" "${cmd:$i:1}"; sleep 0.03; done
    echo ""; sleep 0.3
    eval "$cmd"
}
narrate() { echo ""; echo -e "\033[1;36m# $1\033[0m"; sleep 1.4; }

TAEL="./target/debug/tael"
TEST="./target/debug/tael-test"

cargo build --quiet 2>/dev/null
pkill -f "tael serve" 2>/dev/null || true
rm -rf ./data; sleep 0.5

clear
echo ""
echo -e "\033[1;37m  ████████╗ █████╗ ███████╗██╗     \033[0m"
echo -e "\033[1;37m  ╚══██╔══╝██╔══██╗██╔════╝██║     \033[0m"
echo -e "\033[1;37m     ██║   ███████║█████╗  ██║     \033[0m"
echo -e "\033[1;37m     ██║   ██╔══██║██╔══╝  ██║     \033[0m"
echo -e "\033[1;37m     ██║   ██║  ██║███████╗███████╗\033[0m"
echo -e "\033[1;37m     ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝\033[0m"
echo ""
echo -e "  \033[0;90mAI-agent-native observability · OTLP traces · logs · metrics\033[0m"
echo ""; sleep 3

narrate "One binary — server + client. OTLP gRPC :4317, REST API :7701, tiered storage"
type_cmd "$TAEL serve &"
sleep 4; echo ""

narrate "Health check — every command speaks JSON by default"
type_cmd "$TAEL server status --format json"; sleep 2

narrate "Ingest OpenTelemetry traces from sample microservices + an LLM call"
type_cmd "$TEST"; sleep 2

narrate "Which services are reporting, and how healthy?"
type_cmd "$TAEL services --format table"; sleep 3

narrate "Recent traces across every service"
type_cmd "$TAEL query traces --last 1h --format table --limit 10"; sleep 3

narrate "What's broken? Filter to error traces"
type_cmd "$TAEL query traces --status error --format table"; sleep 3

narrate "Where's the latency? Spans slower than 500ms"
type_cmd "$TAEL query traces --min-duration 500ms --format table"; sleep 3

narrate "NEW — filter by any span attribute (repeatable)"
type_cmd "$TAEL query traces --attribute http.route=/checkout --format table"; sleep 3

narrate "NEW — LLM observability: full-text search over gen_ai prompt/completion payloads"
type_cmd "$TAEL query traces --text 'OTLP' --format table"; sleep 3

ERR_TID=$($TAEL query traces --status error --format json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['trace_id'])")
ERR_SPAN=$($TAEL query traces --status error --format json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['span_id'])")

narrate "Pull the full failing trace — span tree, attributes, events — as JSON"
type_cmd "$TAEL get trace $ERR_TID --format json"; sleep 4

narrate "NEW — one-shot health digest over a window (built for an agent to read)"
type_cmd "$TAEL summarize --last 1h --format table"; sleep 4

narrate "NEW — read-only SQL straight over the telemetry tables"
type_cmd "$TAEL query sql 'SELECT service, COUNT(*) AS spans, ROUND(AVG(duration_ms),1) AS avg_ms FROM spans GROUP BY service ORDER BY spans DESC LIMIT 5'"; sleep 3

narrate "NEW — correlate stitches one trace across spans, logs, and metrics"
type_cmd "$TAEL correlate --trace $ERR_TID --format table"; sleep 3

narrate "Agents annotate traces — and can pin a note to a specific span"
type_cmd "$TAEL comment add $ERR_TID 'Stripe 402 — card declined' --author oncall-bot"; sleep 1
type_cmd "$TAEL comment add $ERR_TID 'Card expired. Closing.' --author triage-agent --span-id $ERR_SPAN"; sleep 1
type_cmd "$TAEL comment list $ERR_TID --format table"; sleep 3

narrate "Wire it into Claude Code — the skill auto-loads when you debug here"
type_cmd "$TAEL skill where"; sleep 2

echo ""
echo -e "\033[1;36m# ─────────────────────────────────────────────────────\033[0m"
echo ""
echo -e "  \033[1;37mtael\033[0m — observability built for AI agents"
echo ""
echo -e "  \033[0;90m• Single binary: \`tael serve\` is server + client + storage\033[0m"
echo -e "  \033[0;90m• OTLP traces, logs & metrics + Prometheus remote-write\033[0m"
echo -e "  \033[0;90m• Tiered storage engine (hot→warm→cold) — no external deps\033[0m"
echo -e "  \033[0;90m• Typed LLM spans (gen_ai.*): model/token/cost + payload search\033[0m"
echo -e "  \033[0;90m• summarize · anomalies · correlate — agent-ready analysis\033[0m"
echo -e "  \033[0;90m• Read-only SQL over the telemetry tables\033[0m"
echo -e "  \033[0;90m• \`tael live\` TUI: waterfall + fullscreen span viewer\033[0m"
echo ""; sleep 5

kill %1 2>/dev/null || true
rm -rf ./data
