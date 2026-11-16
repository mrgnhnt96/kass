#!/bin/bash
# The kass project's release script: tb release cut v<version> runs this with
# TB_RELEASE set. The release itself is cargo xtask release (crates/xtask).
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo xtask release "$@"
