#!/bin/bash
# Run the desktop app for development: Vite serves the screens with hot
# reload, and a debug build of the app loads them from it.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.bun/bin:$HOME/.cargo/bin:$PATH"

vite_pid=""
if ! curl -sf http://localhost:5173 >/dev/null 2>&1; then
  bun run --cwd tauri dev &
  vite_pid=$!
  trap '[ -n "$vite_pid" ] && kill "$vite_pid" 2>/dev/null; wait' EXIT
  for _ in $(seq 1 100); do
    curl -sf http://localhost:5173 >/dev/null 2>&1 && break
    sleep 0.2
  done
fi

cd tauri/src-tauri
cargo run
