#!/usr/bin/env bash
# Multi-process, real-network failover drill (docs/tael-server-scaling-ha.md
# §5.1, gap-closure roadmap D2 residual).
#
# The in-process kill-the-leader test in CI proves the WAL-shipping logic;
# this drill proves the same story with real processes, real TCP, and a real
# SIGKILL:
#
#   1. start a standby tael-server and a leader that ships its WAL to it
#      synchronously (TAEL_WAL_STANDBYS, required_acks = all), with chitchat
#      gossip election between them
#   2. write traffic to the leader over OTLP gRPC
#   3. confirm every acked write is already on the standby
#   4. SIGKILL the leader mid-flight
#   5. confirm gossip elects the standby, it serves all acked data, and it
#      accepts new writes as the promoted owner
#
# Usage: scripts/failover-drill.sh
#   TAEL_BIN=…       path to a `tael` binary   (default: cargo build -p tael-cli)
#   TAEL_TEST_BIN=…  path to a `tael-test` bin (default: cargo build -p tael-test)
#
# Exits 0 on a clean drill, non-zero (with the failing step named) otherwise.

set -euo pipefail

# ── Ports (uncommon, to avoid colliding with a local tael) ─────────────
LEADER_REST=17701 LEADER_GRPC=14317 LEADER_GOSSIP=19890
STANDBY_REST=17711 STANDBY_GRPC=14327 STANDBY_GOSSIP=19891

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d -t tael-failover-XXXXXX)"
LEADER_PID="" STANDBY_PID=""

cleanup() {
  if [ -n "$LEADER_PID" ]; then kill -9 "$LEADER_PID" 2>/dev/null || true; fi
  if [ -n "$STANDBY_PID" ]; then kill -9 "$STANDBY_PID" 2>/dev/null || true; fi
  if [ "${KEEP_WORK:-0}" = "1" ]; then echo "work dir kept: $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# ── Binaries ────────────────────────────────────────────────────────────
if [ -z "${TAEL_BIN:-}" ]; then
  step "building tael (debug)"
  (cd "$ROOT" && cargo build -q -p tael-cli)
  TAEL_BIN="$ROOT/target/debug/tael"
fi
if [ -z "${TAEL_TEST_BIN:-}" ]; then
  step "building tael-test traffic generator (debug)"
  (cd "$ROOT" && cargo build -q -p tael-test)
  TAEL_TEST_BIN="$ROOT/target/debug/tael-test"
fi

# ── Helpers ─────────────────────────────────────────────────────────────
wait_ready() { # url, name
  for _ in $(seq 1 100); do
    if curl -sf "$1/readyz" >/dev/null 2>&1; then return 0; fi
    sleep 0.2
  done
  { echo "--- logs ---"; tail -n 60 "$WORK"/*.log; fail "$2 never became ready at $1"; }
}

trace_count() { # rest base url
  curl -sf "$1/api/v1/traces?limit=1000" |
    python3 -c 'import json,sys; print(len(json.load(sys.stdin)["spans"]))'
}

is_leader() { # rest base url -> "True"/"False"
  curl -sf "$1/internal/cluster" |
    python3 -c 'import json,sys; print(json.load(sys.stdin).get("is_leader", False))'
}

start_node() { # name, rest, grpc, gossip, seeds, node_id, extra-env...
  local name="$1" rest="$2" grpc="$3" gossip="$4" seeds="$5" node_id="$6"
  shift 6
  local dir="$WORK/$name"
  mkdir -p "$dir"
  env "$@" \
    TAEL_DATA_DIR="$dir/data" \
    TAEL_WAL_DIR="$dir/wal" \
    TAEL_REST_API_ADDR="127.0.0.1:$rest" \
    TAEL_OTLP_GRPC_ADDR="127.0.0.1:$grpc" \
    TAEL_OTLP_HTTP_ADDR=off \
    TAEL_DD_AGENT_ADDR=off \
    TAEL_CLUSTER_LISTEN="127.0.0.1:$gossip" \
    TAEL_CLUSTER_SEEDS="$seeds" \
    TAEL_NODE_ID="$node_id" \
    TAEL_CLUSTER_ID=failover-drill \
    RUST_LOG=info \
    "$TAEL_BIN" serve >"$WORK/$name.log" 2>&1 &
  echo $!
}

# ── 1. Standby first (the leader ships to it synchronously) ────────────
step "starting standby (REST :$STANDBY_REST)"
STANDBY_PID=$(start_node standby "$STANDBY_REST" "$STANDBY_GRPC" "$STANDBY_GOSSIP" \
  "127.0.0.1:$LEADER_GOSSIP" "b-standby")
wait_ready "http://127.0.0.1:$STANDBY_REST" standby

# Node ids order the election: "a-leader" < "b-standby", so the leader leads
# while alive and the standby takes over when it drops from the live set.
step "starting leader (REST :$LEADER_REST, ships WAL to standby, acks=all)"
LEADER_PID=$(start_node leader "$LEADER_REST" "$LEADER_GRPC" "$LEADER_GOSSIP" \
  "127.0.0.1:$STANDBY_GOSSIP" "a-leader" \
  TAEL_WAL_STANDBYS="http://127.0.0.1:$STANDBY_REST")
wait_ready "http://127.0.0.1:$LEADER_REST" leader

# ── 2. Traffic to the leader ────────────────────────────────────────────
step "writing traffic to the leader over OTLP gRPC"
TAEL_OTLP_GRPC_ADDR="http://127.0.0.1:$LEADER_GRPC" "$TAEL_TEST_BIN" >/dev/null

LEADER_SPANS=$(trace_count "http://127.0.0.1:$LEADER_REST")
[ "$LEADER_SPANS" -gt 0 ] || fail "leader accepted no spans"
echo "   leader holds $LEADER_SPANS spans"

# ── 3. Everything acked is already on the standby ───────────────────────
step "verifying synchronous replication (standby holds every acked span)"
STANDBY_SPANS=$(trace_count "http://127.0.0.1:$STANDBY_REST")
[ "$STANDBY_SPANS" -eq "$LEADER_SPANS" ] ||
  fail "standby holds $STANDBY_SPANS spans, leader acked $LEADER_SPANS — replication is not replicate-before-ack"
echo "   standby holds all $STANDBY_SPANS spans"

# ── 4. Kill the leader, hard ────────────────────────────────────────────
step "SIGKILL the leader (pid $LEADER_PID)"
kill -9 "$LEADER_PID"
LEADER_PID=""

step "waiting for gossip to elect the standby"
ELECTED="False"
for _ in $(seq 1 300); do # phi-accrual detection takes a few seconds
  ELECTED=$(is_leader "http://127.0.0.1:$STANDBY_REST" || echo False)
  [ "$ELECTED" = "True" ] && break
  sleep 0.2
done
[ "$ELECTED" = "True" ] ||
  fail "standby never took leadership; see $WORK/standby.log"
echo "   standby is leader"

# ── 5. The promoted standby serves history and accepts new writes ──────
step "verifying the promoted standby serves all acked data"
AFTER=$(trace_count "http://127.0.0.1:$STANDBY_REST")
[ "$AFTER" -eq "$LEADER_SPANS" ] || fail "promoted standby lost data: $AFTER != $LEADER_SPANS"

step "writing new traffic to the promoted standby"
TAEL_OTLP_GRPC_ADDR="http://127.0.0.1:$STANDBY_GRPC" "$TAEL_TEST_BIN" >/dev/null
FINAL=$(trace_count "http://127.0.0.1:$STANDBY_REST")
[ "$FINAL" -gt "$LEADER_SPANS" ] || fail "promoted standby did not accept new writes"
echo "   promoted standby now holds $FINAL spans ($LEADER_SPANS pre-failover + new)"

printf '\nfailover drill PASSED: replicate-before-ack, election, zero acked-write loss\n'
