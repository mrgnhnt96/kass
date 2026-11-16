#!/bin/bash
# Build Kass from this checkout and install it to /Applications. The website's
# Build from source page points here; the work is cargo xtask install
# (crates/xtask), which takes --no-pull, --rebuild-server and --no-launch.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
  echo "Rust isn't installed. Install it, open a new terminal and run this again:" >&2
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi
exec cargo xtask install "$@"
