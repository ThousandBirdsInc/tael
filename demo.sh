#!/usr/bin/env bash
# Tael demo script — run inside: asciinema rec demo.cast -c ./demo.sh
set -e

# Typing effect
type_cmd() {
    local cmd="$1"
    printf "\033[1;32m❯\033[0m "
    for (( i=0; i<${#cmd}; i++ )); do
        printf "%s" "${cmd:$i:1}"
        sleep 0.04
    done
    echo ""
    sleep 0.3
    eval "$cmd"
}

narrate() {
    echo ""
    echo -e "\033[1;36m# $1\033[0m"
    sleep 1.5
}

TAEL="./target/debug/tael"
SERVER="./target/debug/tael-server"
TEST="./target/debug/tael-test"

# Pre-build
cargo build --quiet 2>/dev/null

# Clean state
pkill -f tael-server 2>/dev/null || true
rm -rf ./data
sleep 0.5

clear
echo ""
echo -e "\033[1;37m  ████████╗ █████╗ ███████╗██╗     \033[0m"
echo -e "\033[1;37m  ╚══██╔══╝██╔══██╗██╔════╝██║     \033[0m"
echo -e "\033[1;37m     ██║   ███████║█████╗  ██║     \033[0m"
echo -e "\033[1;37m     ██║   ██╔══██║██╔══╝  ██║     \033[0m"
echo -e "\033[1;37m     ██║   ██║  ██║███████╗███████╗\033[0m"
echo -e "\033[1;37m     ╚═╝   ╚═╝  ╚═╝╚══════╝╚══════╝\033[0m"
echo ""
echo -e "  \033[0;90mAI-agent-native observability\033[0m"
echo ""
sleep 3

# --- Start the server ---
narrate "Start the tael server (OTLP gRPC :4317, REST API :7701)"
type_cmd "$SERVER &"
sleep 3
echo ""

narrate "Check server health"
type_cmd "$TAEL server status --format json"
sleep 2

# --- Ingest data ---
narrate "Ingest OpenTelemetry traces from sample microservices"
type_cmd "$TEST"
sleep 2

# --- Explore services ---
narrate "See which services are reporting"
type_cmd "$TAEL services --format table"
sleep 3

# --- Query all traces ---
narrate "Query recent traces across all services"
type_cmd "$TAEL query traces --last 1h --format table --limit 10"
sleep 3

# --- Find errors ---
narrate "Find error traces — what's broken?"
type_cmd "$TAEL query traces --status error --format table"
sleep 3

# --- Find slow queries ---
narrate "Find slow spans (>500ms) — what's the bottleneck?"
type_cmd "$TAEL query traces --min-duration 500ms --format table"
sleep 3

# --- Filter by service ---
narrate "Drill into a specific service"
type_cmd "$TAEL query traces --service payment-service --format table"
sleep 2

# --- Get full trace ---
narrate "Get the full trace for the error — see the entire request flow"
TRACE_ID=$($TAEL query traces --status error --format json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)['spans'][0]['trace_id'])")
type_cmd "$TAEL get trace $TRACE_ID --format json"
sleep 4

# --- Add comments ---
narrate "Agents can annotate traces with comments for collaboration"
type_cmd "$TAEL comment add $TRACE_ID 'Payment declined — Stripe returning 402 for this card' --author oncall-bot"
sleep 1.5
type_cmd "$TAEL comment add $TRACE_ID 'Customer contacted, card expired. Not a system issue.' --author debug-agent"
sleep 1.5

narrate "View the comment thread on this trace"
type_cmd "$TAEL comment list $TRACE_ID --format table"
sleep 3

# --- JSON output for agents ---
narrate "Every command returns structured JSON — designed for agent consumption"
type_cmd "$TAEL comment list $TRACE_ID --format json"
sleep 3

# --- Wrap up ---
echo ""
echo -e "\033[1;36m# ─────────────────────────────────────────────────────\033[0m"
echo ""
echo -e "  \033[1;37mtael\033[0m — observability built for AI agents"
echo ""
echo -e "  \033[0;90m• Ingests OpenTelemetry traces via standard OTLP gRPC\033[0m"
echo -e "  \033[0;90m• CLI-first: structured JSON output for agent workflows\033[0m"
echo -e "  \033[0;90m• DuckDB storage — zero dependencies, single binary\033[0m"
echo -e "  \033[0;90m• Comments for agent-to-agent collaboration on traces\033[0m"
echo -e "  \033[0;90m• Interactive TUI with waterfall trace visualization\033[0m"
echo ""
sleep 5

# Cleanup
kill %1 2>/dev/null || true
rm -rf ./data
