#!/bin/bash
# Set the app version everywhere it's written down.
#
#   ./scripts/set-version.sh 0.6.0     write 0.6.0 into every file
#   ./scripts/set-version.sh 0.7.0-beta.1   a beta works the same way
#   ./scripts/set-version.sh --check   fail unless every file has the same version
#
# A leading v is optional: v0.6.0 and 0.6.0 are the same version.
#
# GitHub releases are the source of truth: scripts/release.sh calls this
# before it tags, and the release workflow runs --check against the tag.
set -euo pipefail
cd "$(dirname "$0")/.."

json_files=(tauri/src-tauri/tauri.conf.json)

versions() {
  for file in "${json_files[@]}"; do
    echo "$file $(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$file")"
  done
  echo "tauri/src-tauri/Cargo.toml $(sed -n 's/^version = "\(.*\)"/\1/p' tauri/src-tauri/Cargo.toml | head -1)"
  echo "tauri/src-tauri/Cargo.lock $(grep -A1 '^name = "kass"$' tauri/src-tauri/Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')"
  echo "backend/pyproject.toml $(sed -n 's/^version = "\(.*\)"/\1/p' backend/pyproject.toml | head -1)"
  echo "backend/__init__.py $(sed -n 's/^__version__ = "\(.*\)"/\1/p' backend/__init__.py)"
}

if [ "${1:-}" = "--check" ]; then
  expected="${2:-}"
  expected="${expected#v}"
  list=$(versions)
  echo "$list"
  unique=$(echo "$list" | awk '{print $2}' | sort -u)
  if [ "$(echo "$unique" | wc -l | tr -d ' ')" != 1 ]; then
    echo "Versions don't match. Run: ./scripts/set-version.sh <version>" >&2
    exit 1
  fi
  if [ -n "$expected" ] && [ "$unique" != "$expected" ]; then
    echo "Files say $unique, expected $expected." >&2
    exit 1
  fi
  exit 0
fi

version="${1:-}"
version="${version#v}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]]; then
  echo "Usage: $0 <major.minor.patch[-beta.N]> | --check [version]" >&2
  exit 1
fi

for file in "${json_files[@]}"; do
  # Rewrite only the top-level "version" line, so formatting stays as it is.
  python3 - "$file" "$version" <<'PY'
import re, sys
path, version = sys.argv[1], sys.argv[2]
text = open(path).read()
text, count = re.subn(r'^(  "version": )"[^"]*"', rf'\1"{version}"', text, count=1, flags=re.M)
if count != 1:
    sys.exit(f"No top-level version in {path}")
open(path, "w").write(text)
PY
done
sed -i '' "1,/^version = /s/^version = \".*\"/version = \"$version\"/" tauri/src-tauri/Cargo.toml backend/pyproject.toml
sed -i '' "/^name = \"kass\"$/{n;s/^version = \".*\"/version = \"$version\"/;}" tauri/src-tauri/Cargo.lock
sed -i '' "s/^__version__ = \".*\"/__version__ = \"$version\"/" backend/__init__.py

versions
