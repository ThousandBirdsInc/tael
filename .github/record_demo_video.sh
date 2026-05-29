#!/usr/bin/env bash
# Record a terminal video demo that covers eval commands and TUI navigation.
#
# Requires:
#   - vhs: https://github.com/charmbracelet/vhs
#   - ttyd and ffmpeg, used by vhs
#   - uv, for demo/tael-evals-agent
#
# Usage:
#   .github/record_demo_video.sh
#   .github/record_demo_video.sh .github/demo-video.mp4
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

OUTPUT="${1:-$SCRIPT_DIR/demo-video.mp4}"
RUN_ID="${TAEL_DEMO_RUN_ID:-demo-local-wiki-video}"
REST_PORT="${TAEL_DEMO_REST_PORT:-7771}"
OTLP_PORT="${TAEL_DEMO_OTLP_PORT:-4377}"
SERVER_URL="http://127.0.0.1:${REST_PORT}"
OTLP_ENDPOINT="http://127.0.0.1:${OTLP_PORT}"
OTLP_GRPC_ADDR="$OTLP_ENDPOINT"
TAEL_BIN="${TAEL_BIN:-$ROOT_DIR/target/debug/tael}"
TEST_BIN="${TEST_BIN:-$ROOT_DIR/target/debug/tael-test}"
DEMO_DIR="$ROOT_DIR/demo/tael-evals-agent"
SERVER_PID=""

usage() {
  cat <<EOF
Usage: $0 [output.mp4]

Records a terminal video of the tael demo to an MP4 by default.

Environment:
  TAEL_DEMO_RUN_ID       Eval run id to seed and show (default: $RUN_ID)
  TAEL_DEMO_REST_PORT    Local REST port for the demo server (default: $REST_PORT)
  TAEL_DEMO_OTLP_PORT    Local OTLP gRPC port for the demo server (default: $OTLP_PORT)
  TAEL_BIN               Path to tael binary (default: $TAEL_BIN)
  TEST_BIN               Path to tael-test binary (default: $TEST_BIN)
  SKIP_BUILD=1           Reuse existing target/debug binaries
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

shell_quote() {
  printf "%q" "$1"
}

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "${WORK_DIR:-}" && -d "${WORK_DIR:-}" ]]; then
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

wait_for_server() {
  local i
  for i in $(seq 1 80); do
    if "$TAEL_BIN" server status --server "$SERVER_URL" --format json >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done

  echo "error: tael server did not become ready at $SERVER_URL" >&2
  echo "server log:" >&2
  tail -80 "$SERVER_LOG" >&2 || true
  exit 1
}

need cargo
need python3
need uv
need vhs
need ttyd
need ffmpeg

cd "$ROOT_DIR"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --quiet --bins
fi

if [[ ! -x "$TAEL_BIN" ]]; then
  echo "error: tael binary is not executable: $TAEL_BIN" >&2
  exit 1
fi

if [[ ! -x "$TEST_BIN" ]]; then
  echo "error: tael-test binary is not executable: $TEST_BIN" >&2
  exit 1
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tael-demo-video.XXXXXX")"
DATA_DIR="$WORK_DIR/data"
WAL_DIR="$WORK_DIR/wal"
SERVER_LOG="$WORK_DIR/tael-server.log"
TAPE="$WORK_DIR/demo-video.tape"

echo "Starting demo server on $SERVER_URL / $OTLP_ENDPOINT"
"$TAEL_BIN" serve \
  --rest-api-addr "127.0.0.1:${REST_PORT}" \
  --otlp-grpc-addr "127.0.0.1:${OTLP_PORT}" \
  --data-dir "$DATA_DIR" \
  --wal-dir "$WAL_DIR" \
  >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
wait_for_server

echo "Seeding trace data"
TAEL_OTLP_GRPC_ADDR="$OTLP_GRPC_ADDR" "$TEST_BIN" >/dev/null 2>&1

echo "Seeding eval run $RUN_ID"
TAEL_BIN="$TAEL_BIN" \
TAEL_SERVER="$SERVER_URL" \
TAEL_OTLP_ENDPOINT="$OTLP_ENDPOINT" \
TAEL_DEMO_RUN_ID="$RUN_ID" \
  "$DEMO_DIR/run_demo.sh" >/dev/null

ROOT_Q="$(shell_quote "$ROOT_DIR")"
TAEL_Q="$(shell_quote "$TAEL_BIN")"
SERVER_Q="$(shell_quote "$SERVER_URL")"
RUN_ID_Q="$(shell_quote "$RUN_ID")"

cat >"$TAPE" <<EOF
Output "$OUTPUT"

Set Shell "bash"
Set Width 1440
Set Height 1080
Set FontSize 18
Set Framerate 30
Set TypingSpeed 18 ms

Type "cd $ROOT_Q"
Enter
Sleep 300 ms
Type "clear"
Enter
Sleep 400 ms
Type "printf 'tael demo: TUI navigation + trace-native evals\\n\\n'"
Enter
Sleep 900 ms

Type "$TAEL_Q server status --server $SERVER_Q --format table"
Enter
Sleep 2

Type "$TAEL_Q eval report $RUN_ID_Q --server $SERVER_Q --format table"
Enter
Sleep 4

Type "$TAEL_Q eval cases $RUN_ID_Q --server $SERVER_Q --format table"
Enter
Sleep 3

Type "$TAEL_Q live --server $SERVER_Q --eval-run $RUN_ID_Q --interval 1"
Enter
Sleep 3

Type "j"
Sleep 700 ms
Type "j"
Sleep 700 ms
Type "f"
Sleep 900 ms
Type "r"
Sleep 900 ms
Enter
Sleep 2
Backspace
Sleep 1

Type "1"
Sleep 1
Type "j"
Sleep 700 ms
Type "j"
Sleep 700 ms
Enter
Sleep 2
Backspace
Sleep 1

Type "4"
Sleep 1
Type "j"
Sleep 700 ms
Type "+"
Sleep 700 ms
Type "-"
Sleep 700 ms

Type "2"
Sleep 1
Type "j"
Sleep 700 ms
Enter
Sleep 1

Type "3"
Sleep 1
Type "\\"
Sleep 700 ms
Type "q"
Sleep 1

Type "$TAEL_Q eval scores $RUN_ID_Q --server $SERVER_Q --format table"
Enter
Sleep 3

Type "$TAEL_Q eval runs --server $SERVER_Q --format table"
Enter
Sleep 3
EOF

mkdir -p "$(dirname "$OUTPUT")"
echo "Recording $OUTPUT"
vhs "$TAPE"

echo "Wrote $OUTPUT"
