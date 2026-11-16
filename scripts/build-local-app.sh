#!/bin/bash
# Build a local macOS app with a stable signing identity so privacy grants can persist.
# Backend binaries must already exist (run build-server.sh when backend code changes).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.bun/bin:$HOME/.cargo/bin:$PATH"
# The same identity every build, so macOS keeps the app's privacy grants.
identity=$(./scripts/signing-identity.sh)

# The server folder is copied into the app as-is, so sign its code first:
# every library, then the executable with the app's hardened-runtime
# entitlements. The bundler then seals the folder into the app signature.
server=tauri/src-tauri/binaries/kass-server
if [ ! -x "$server/kass-server" ]; then
  echo "Server not built at $server. Run scripts/build-server.sh first." >&2
  exit 1
fi
# Notarization (APPLE_API_KEY set, see release.yml) needs a secure timestamp
# on every signature; local builds skip the round trip to Apple.
timestamp=--timestamp=none
[ -n "${APPLE_API_KEY:-}" ] && timestamp=--timestamp
find "$server/_internal" -type f -print0 | xargs -0 file | grep 'Mach-O' | cut -d: -f1 | tr '\n' '\0' |
  xargs -0 -n 32 -P 8 codesign --force "$timestamp" --options runtime --sign "$identity"
codesign --force "$timestamp" --options runtime --entitlements tauri/src-tauri/Entitlements.plist \
  --sign "$identity" "$server/kass-server"

config=$(mktemp /tmp/kass-local-signing.XXXXXX)
trap 'rm -f "$config"' EXIT
# With the updater key set (release.yml), the bundler also packs the app as a
# signed .app.tar.gz for the background updater.
updater=False
[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && updater=True
python3 - "$config" "$identity" "$updater" <<'PY'
import json, sys
with open(sys.argv[1], 'w') as config:
    json.dump({'bundle': {'createUpdaterArtifacts': sys.argv[3] == 'True', 'macOS': {'signingIdentity': sys.argv[2]}}}, config)
PY
if command -v bun >/dev/null 2>&1; then
  bun=(bun)
else
  bun=(npx --yes bun@1.3.8)
fi
# The screens, then the app with them inside, then the Tauri CLI packs and
# signs the bundle (it only bundles; the app no longer uses Tauri).
"${bun[@]}" run --cwd tauri build
(cd tauri/src-tauri && cargo build --release --features custom-protocol)
"${bun[@]}" run --cwd tauri tauri bundle --bundles app --config "$config"
codesign --verify --deep --strict tauri/src-tauri/target/release/bundle/macos/Kass.app
