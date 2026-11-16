#!/bin/bash
# Build Kass from this checkout and install it to /Applications.
#
#   ./scripts/install.sh
#
# For working on Kass. Everyone else installs the DMG from GitHub
# Releases. Checks what the build needs and says how to install anything
# missing, pulls the latest code, builds the server (only when it changed)
# and the app, and replaces /Applications/Kass.app. Every build is signed
# with the same identity, so updates keep the app's privacy permissions.
#
# Options:
#   --no-pull          build the checkout as it is
#   --rebuild-server   rebuild the Python server even if it hasn't changed
#   --no-launch        don't open Kass afterwards
set -euo pipefail

# Always /Applications: macOS privacy permissions (Input Monitoring,
# Accessibility, Microphone) don't work reliably for an app anywhere else.
app=/Applications/Kass.app
bundle_id="com.mrgnhnt.kass"

pull=1
rebuild_server=0
launch=1
for arg in "$@"; do
  case "$arg" in
  --no-pull) pull=0 ;;
  --rebuild-server) rebuild_server=1 ;;
  --no-launch) launch=0 ;;
  -h | --help)
    sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "Unknown option: $arg (see --help)" >&2
    exit 1
    ;;
  esac
done

if [ -t 1 ]; then
  bold=$'\033[1m' dim=$'\033[2m' red=$'\033[31m' green=$'\033[32m' yellow=$'\033[33m' reset=$'\033[0m'
else
  bold='' dim='' red='' green='' yellow='' reset=''
fi
step() { echo; echo "${bold}==> $*${reset}"; }
ok() { echo "  ${green}✓${reset} $*"; }
warn() { echo "  ${yellow}!${reset} $*"; }
die() {
  echo "${red}✗ $*${reset}" >&2
  exit 1
}

# Tools installed by rustup's installer live here, and a fresh shell may
# not have them on PATH yet.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# ─── Check what the build needs ───────────────────────────────────────

step "Checking requirements"

if [ "$(uname)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
  die "Kass only runs on Apple Silicon Macs."
fi

missing=()
need() { missing+=("$1|$2"); }
has_brew=0
command -v brew >/dev/null 2>&1 && has_brew=1
brew_first=""
[ "$has_brew" = 1 ] || brew_first='/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'$'\n      '

if xcode-select -p >/dev/null 2>&1; then
  ok "Xcode Command Line Tools"
else
  need "Xcode Command Line Tools (compilers, git, codesign)" "xcode-select --install"
fi

if command -v git >/dev/null 2>&1; then ok "git"; fi

if ./scripts/setup-python.sh --check; then
  ok "Python 3.12"
else
  need "Python 3.12 (the server's ML packages don't support 3.13 yet)" "${brew_first}brew install python@3.12"
fi

if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
  ok "Rust $(rustc --version | cut -d' ' -f2)"
else
  need "Rust (builds the app)" "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
fi

free_gb=$(df -g "$root" | awk 'NR == 2 { print $4 }')
if [ "${free_gb:-0}" -lt 15 ]; then
  warn "Only ${free_gb} GB free. A first build needs about 15 GB (Python packages, Rust build)."
fi

if [ ${#missing[@]} -gt 0 ]; then
  echo
  echo "${red}${bold}Some things Kass needs to build are missing.${reset} Install them, then run this again:"
  for item in "${missing[@]}"; do
    echo
    echo "  ${bold}${item%%|*}${reset}"
    echo "      ${item#*|}"
  done
  echo
  echo "${dim}Open a new terminal after installing, so the new commands are on your PATH.${reset}"
  exit 1
fi

if [ ! -w "$(dirname "$app")" ]; then
  die "$(dirname "$app") isn't writable by $(whoami). Run this from an administrator account."
fi

# ─── Get the latest code ──────────────────────────────────────────────

if [ "$pull" = 1 ] && [ -d .git ] && git rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1; then
  step "Updating the code"
  if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    warn "This checkout has local changes, so it's built as it is."
  elif git pull --ff-only --quiet; then
    ok "$(git rev-parse --abbrev-ref HEAD) at $(git log -1 --format='%h %s')"
  else
    warn "Couldn't fast-forward to the latest code; building what's here."
  fi
fi

# A stamp of the inputs, to skip steps whose inputs haven't changed.
stamp_of() { { "$@" || true; } 2>/dev/null | shasum | cut -d' ' -f1; }

# ─── Dependencies ─────────────────────────────────────────────────────

step "Setting up the Python environment"
python_stamp=$(stamp_of cat backend/requirements.txt scripts/setup-python.sh)
python_stamp_file=backend/venv/.kass-install-stamp
if [ -f "$python_stamp_file" ] && [ "$(cat "$python_stamp_file")" = "$python_stamp" ]; then
  ok "Up to date"
else
  ./scripts/setup-python.sh
  echo "$python_stamp" >"$python_stamp_file"
  ok "Python packages"
fi

# ─── Build ────────────────────────────────────────────────────────────

step "Building the server"
server_inputs() {
  git rev-parse HEAD:backend
  git diff HEAD -- backend
  git ls-files --others --exclude-standard backend
  cat scripts/build-server.sh "$python_stamp_file"
}
server_stamp=$(stamp_of server_inputs)
server_stamp_file=tauri/src-tauri/binaries/.kass-server-stamp
if [ "$rebuild_server" = 0 ] && [ -x tauri/src-tauri/binaries/kass-server/kass-server ] &&
  [ -f "$server_stamp_file" ] && [ "$(cat "$server_stamp_file")" = "$server_stamp" ]; then
  ok "Unchanged since the last build"
else
  echo "  ${dim}This takes a few minutes.${reset}"
  ./scripts/build-server.sh
  echo "$server_stamp" >"$server_stamp_file"
  ok "Server built"
fi

step "Building the app"
echo "  ${dim}The first build compiles the Rust app shell and takes a while.${reset}"
./scripts/build-local-app.sh
built=tauri/src-tauri/target/release/bundle/macos/Kass.app
ok "Built and signed with \"$(codesign -dvv "$built" 2>&1 | sed -n 's/^Authority=//p' | head -n 1)\""

# ─── Install ──────────────────────────────────────────────────────────

step "Installing to $app"
# An ad-hoc signature prints its requirement as a comment ("# designated => …").
requirement() { codesign -d -r- "$1" 2>&1 | sed -n 's/^\(# \)\{0,1\}designated => //p'; }
had_app=false
old_requirement=""
[ -d "$app" ] && had_app=true && old_requirement=$(requirement "$app")
new_requirement=$(requirement "$built")

if pgrep -f "$app/Contents/MacOS/" >/dev/null 2>&1; then
  osascript -e 'quit app id "'"$bundle_id"'"' >/dev/null 2>&1 || true
  for _ in $(seq 1 30); do
    pgrep -f "$app/Contents/MacOS/" >/dev/null 2>&1 || break
    sleep 0.5
  done
  pkill -f "$app/Contents/" 2>/dev/null || true
  ok "Quit the running app"
fi
rm -rf "$app"
ditto "$built" "$app"
codesign --verify --deep --strict "$app"
ok "Installed"

# Builds from a checkout are internal builds: internal features run
# (backend/internal.py). The release DMG never writes this file.
data_dir="$HOME/Library/Application Support/$bundle_id"
mkdir -p "$data_dir"
touch "$data_dir/internal"
ok "Internal features on (delete $data_dir/internal to turn them off)"

# TODO(2026-10-13): remove the Voicebox-era cleanup below (the ~/Applications
# copy, Voicebox.app and the Voicebox Input input method). By then anyone
# building from source has run this at least once since the rename to Herga.
# Earlier versions could install to ~/Applications. Only the copy in
# /Applications gets working permissions, so don't leave another one to
# open by mistake.
old_app="$HOME/Applications/Voicebox.app"
if [ -d "$old_app" ]; then
  pkill -f "$old_app/Contents/" 2>/dev/null || true
  if mv "$old_app" "$HOME/.Trash/Voicebox $(date '+%Y-%m-%d %H.%M.%S').app" 2>/dev/null; then
    ok "Moved the old copy in ~/Applications to the Trash"
  else
    rm -rf "$old_app"
    ok "Removed the old copy in ~/Applications"
  fi
fi

# Kass was Voicebox. Retire the old app, or it would still start at login
# and answer the same hotkey. Upstream Voicebox, the text-to-speech app,
# shares the old bundle id, so only a copy whose data folder holds a file
# only this app writes counts as ours.
voicebox_app=/Applications/Voicebox.app
if [ -d "$voicebox_app" ]; then
  voicebox_id=$(plutil -extract CFBundleIdentifier raw "$voicebox_app/Contents/Info.plist" 2>/dev/null || true)
  ours=false
  [ "$voicebox_id" = com.mrgnhnt.voicebox ] && ours=true
  if [ "$voicebox_id" = sh.voicebox.app ]; then
    for file in writing-style.json correction-learning.json launch-at-login-defaulted dictation-device.txt; do
      [ -e "$HOME/Library/Application Support/sh.voicebox.app/$file" ] && ours=true
    done
  fi
  if $ours; then
    osascript -e 'quit app id "'"$voicebox_id"'"' >/dev/null 2>&1 || true
    pkill -f "$voicebox_app/Contents/" 2>/dev/null || true
    for service in Microphone Accessibility ListenEvent PostEvent; do
      tccutil reset "$service" "$voicebox_id" >/dev/null 2>&1 || true
    done
    if mv "$voicebox_app" "$HOME/.Trash/Voicebox $(date '+%Y-%m-%d %H.%M.%S').app" 2>/dev/null; then
      ok "Moved Voicebox.app, Kass's old name, to the Trash"
    else
      rm -rf "$voicebox_app"
      ok "Removed Voicebox.app, Kass's old name"
    fi
    warn "Kass keeps your captures and settings, but macOS will ask for"
    warn "Microphone, Accessibility and Input Monitoring again under the new name."
  fi
fi

# Kass was Herga. Retire the old app the same way, so it doesn't start at
# login and answer the same hotkey.
herga_app=/Applications/Herga.app
herga_id=com.mrgnhnt.herga
if [ -d "$herga_app" ] &&
  [ "$(plutil -extract CFBundleIdentifier raw "$herga_app/Contents/Info.plist" 2>/dev/null || true)" = "$herga_id" ]; then
  osascript -e 'quit app id "'"$herga_id"'"' >/dev/null 2>&1 || true
  pkill -f "$herga_app/Contents/" 2>/dev/null || true
  for service in Microphone Accessibility ListenEvent PostEvent; do
    tccutil reset "$service" "$herga_id" >/dev/null 2>&1 || true
  done
  if mv "$herga_app" "$HOME/.Trash/Herga $(date '+%Y-%m-%d %H.%M.%S').app" 2>/dev/null; then
    ok "Moved Herga.app, Kass's old name, to the Trash"
  else
    rm -rf "$herga_app"
    ok "Removed Herga.app, Kass's old name"
  fi
  warn "Kass keeps your captures and settings, but macOS will ask for"
  warn "Microphone, Accessibility and Input Monitoring again under the new name."
fi

# An unreadable old requirement counts as a change, so it resets instead of
# silently keeping grants that may not apply.
if $had_app && [ "$old_requirement" != "$new_requirement" ]; then
  # The old grants belong to the old signature and no longer apply. Clear
  # them so System Settings asks again cleanly instead of showing switches
  # that are on but don't work.
  for service in Microphone Accessibility ListenEvent PostEvent; do
    tccutil reset "$service" "$bundle_id" >/dev/null 2>&1 || true
  done
  warn "This build is signed differently from the one it replaced, so macOS"
  warn "will ask for Microphone, Accessibility and Input Monitoring again."
  warn "It's signed the same way from now on, so updates keep them."
fi

# Earlier versions installed the Voicebox Input input method. Switch off it
# first if it's the selected keyboard, so removing it doesn't leave the
# input menu pointing at nothing.
im_app="$HOME/Library/Input Methods/Voicebox Input.app"
if [ -d "$im_app" ]; then
  step "Removing the Voicebox Input input method"
  swift - <<'SWIFT' >/dev/null 2>&1 || warn "Could not turn off Voicebox Input; remove it from your input sources in System Settings."
import Carbon

let filter = [kTISPropertyInputSourceID as String: "sh.voicebox.inputmethod.VoiceboxInput"] as CFDictionary
let sources = TISCreateInputSourceList(filter, true)?.takeRetainedValue() as? [TISInputSource] ?? []
for source in sources {
    if let raw = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsSelected),
       CFBooleanGetValue(Unmanaged<CFBoolean>.fromOpaque(raw).takeUnretainedValue()),
       let layout = TISCopyCurrentASCIICapableKeyboardLayoutInputSource()?.takeRetainedValue() {
        TISSelectInputSource(layout)
    }
    TISDisableInputSource(source)
}
SWIFT
  pkill -f "$im_app/Contents/MacOS/" 2>/dev/null || true
  rm -rf "$im_app"
  ok "Removed $im_app"
fi

if [ "$launch" = 1 ]; then
  open "$app"
  ok "Opened Kass"
fi

echo
echo "${green}${bold}Kass is installed.${reset} Run this script again to update."
