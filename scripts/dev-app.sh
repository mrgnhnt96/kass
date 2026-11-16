#!/bin/bash
# Run a debug build of the desktop app for development. The server runs
# separately (just dev-backend).
set -euo pipefail
cd "$(dirname "$0")/../tauri/src-tauri"
export PATH="$HOME/.cargo/bin:$PATH"
exec cargo run
