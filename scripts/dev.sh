#!/usr/bin/env bash
# Local development helper: builds, starts a signal server and an agent, then
# prints the exact commands to connect. Everything runs on one machine.
#
#   ./scripts/dev.sh            # LAN-only agent + probe instructions
#   ./scripts/dev.sh --signal   # also start the signal server and register
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CODE="${CODE:-654321}"
AGENT_PORT="${AGENT_PORT:-34789}"
SIGNAL_PORT="${SIGNAL_PORT:-34790}"
STATE_DIR="${STATE_DIR:-/tmp/velarouter-dev}"
WITH_SIGNAL=0
[[ "${1:-}" == "--signal" ]] && WITH_SIGNAL=1

echo "==> building (release)"
cargo build --release -q

mkdir -p "$STATE_DIR"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT INT TERM

if [[ $WITH_SIGNAL -eq 1 ]]; then
  echo "==> signal server on :$SIGNAL_PORT"
  ./target/release/velarouter signal \
    --bind "0.0.0.0:$SIGNAL_PORT" \
    --public-url "http://127.0.0.1:$SIGNAL_PORT" &
  PIDS+=($!)
  sleep 1
fi

echo "==> agent on :$AGENT_PORT (pairing code $CODE)"
SIGNAL_ARG=()
[[ $WITH_SIGNAL -eq 1 ]] && SIGNAL_ARG=(--signal "http://127.0.0.1:$SIGNAL_PORT")
./target/release/velarouter agent \
  --code "$CODE" \
  --port "$AGENT_PORT" \
  --state-dir "$STATE_DIR" \
  ${SIGNAL_ARG[@]+"${SIGNAL_ARG[@]}"} &
PIDS+=($!)
sleep 2

cat <<EOF

────────────────────────────────────────────────────────────
 浏览器      http://127.0.0.1:$AGENT_PORT/
 配对码      $CODE

 无头验证：
   ./target/release/velarouter probe \\
     --url ws://127.0.0.1:$AGENT_PORT/vr \\
     --code $CODE --frames 3 --save /tmp/vr-shot.jpg

 局域网发现：
   ./target/release/velarouter discover

 环境自检：
   ./target/release/velarouter doctor
────────────────────────────────────────────────────────────
 按 Ctrl+C 结束
EOF

wait
