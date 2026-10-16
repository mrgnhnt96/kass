#!/bin/bash
# Cut a release: set the version, commit, tag and push.
#
#   tb release cut v0.6.0          a public release
#   tb release cut v0.7.0-beta.1   a beta
#
# Releases go through the task board: tb release cut runs this script with
# TB_RELEASE set and records the release on the board when it succeeds. Run on
# its own, the script refuses unless you pass --force. A leading v is optional.
#
# Every release, beta or not, is built from main.
#
# Pushing the v0.6.0 tag runs .github/workflows/release.yml, which builds
# the app and publishes the GitHub release with the DMG attached. The app
# checks that release to tell people an update is out.
#
# A beta is published as a prerelease: the website and the public update
# skip it, and only copies with Settings › General › Beta updates on get it.
set -euo pipefail
cd "$(dirname "$0")/.."

force=
args=()
for arg in "$@"; do
  if [ "$arg" = --force ]; then force=1; else args+=("$arg"); fi
done
version="${args[0]:-}"
version="${version#v}"
if [ -z "${TB_RELEASE:-}" ] && [ -z "$force" ]; then
  echo "Run tb release cut v${version:-<version>} instead; it runs this script and records the release. (--force to run it without the board.)" >&2
  exit 1
fi
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]]; then
  echo "Usage: $0 [v]<major.minor.patch[-beta.N]> [--force]" >&2
  exit 1
fi
tag="v$version"

branch=$(git rev-parse --abbrev-ref HEAD)
if [[ "$branch" != main ]]; then
  echo "Release from main (on $branch). Merge your branch into main first." >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "Commit or stash your changes first." >&2
  exit 1
fi
# A shared cleanup adapter tested with other cleanup code is ignored by the
# app (backend/services/shared_adapters.py): retrain it before releasing.
backend/venv/bin/python scripts/shared-adapters/publish.py --check
git fetch --tags origin
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "$tag already exists." >&2
  exit 1
fi
# A public release only has to be newer than the last public one: a hotfix
# can go out while a newer beta is out, and beta copies stay on the beta.
# A beta has to be newer than everything, or beta copies would never get it.
tags=$(git tag --list 'v[0-9]*' | sed 's/^v//' | grep -E '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$' || true)
[[ "$version" == *-beta.* ]] || tags=$(echo "$tags" | grep -v -- '-beta\.' || true)
latest=
[ -z "$tags" ] || latest="v$(./scripts/newest-version.py $tags)"
if [ -n "$latest" ] && [ "$(./scripts/newest-version.py "${latest#v}" "$version")" != "$version" ]; then
  echo "$tag isn't newer than $latest." >&2
  exit 1
fi

./scripts/set-version.sh "$version"
git add package.json app/package.json tauri/package.json tauri/src-tauri/tauri.conf.json \
  tauri/src-tauri/Cargo.toml tauri/src-tauri/Cargo.lock backend/pyproject.toml backend/__init__.py bun.lock
git commit -m "chore: release $tag"
git tag -a "$tag" -m "Kass $version"
git push origin "$branch" "$tag"

echo
echo "Pushed $tag. The release workflow builds the app and publishes the release:"
echo "  https://github.com/mrgnhnt96/kass/actions/workflows/release.yml"
