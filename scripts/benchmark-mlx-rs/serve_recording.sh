#!/bin/sh
# parity.py --server-cmd: the Rust API on PORT in front of the recording Python worker on PORT+1.
# usage: serve_recording.sh <python> <port> <data_dir>   (RECORD_DIR set)
set -e
PY=$1; PORT=$2; DATA=$3; WPORT=$((PORT + 1))
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
"$PY" "$ROOT/scripts/benchmark-mlx-rs/record_calls.py" --port "$WPORT" --data-dir "$DATA" &
WORKER=$!
trap 'kill $WORKER $API 2>/dev/null' EXIT INT TERM
until curl -sf "http://127.0.0.1:$WPORT/health" >/dev/null; do
  kill -0 $WORKER 2>/dev/null || exit 1
  sleep 0.2
done
"$ROOT/target/release/examples/serve" "127.0.0.1:$PORT" "127.0.0.1:$WPORT" "$DATA/kass.db" &
API=$!
wait $API
