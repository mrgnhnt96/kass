# Kass development commands (macOS, Apple Silicon)
# Install: brew install just (or cargo install just)
# Usage: just --list

# Directories
backend_dir := "backend"
tauri_dir := "tauri"
app_dir := "app"
venv := backend_dir / "venv"

venv_bin := venv / "bin"
python := venv_bin / "python"

# ─── Setup ────────────────────────────────────────────────────────────

# Full project setup (python venv + JS deps + dev sidecar)
setup: setup-python setup-js setup-hooks
    @echo ""
    @echo "Setup complete! Run: just dev"

# Create venv (Python 3.12) and install Python dependencies
setup-python:
    ./scripts/setup-python.sh

# Install the pre-push hook (scripts/hooks/pre-push) that lints and formats pushed files
setup-hooks:
    git config core.hooksPath scripts/hooks

# Install JavaScript dependencies
setup-js:
    bun install

# ─── Development ──────────────────────────────────────────────────────

# Start backend (if not already running) + frontend for development
dev: _ensure-venv _ensure-sidecar
    #!/usr/bin/env bash
    set -euo pipefail

    backend_pid=""
    if curl -sf http://127.0.0.1:17493/health > /dev/null 2>&1; then
        echo "Backend already running on http://localhost:17493"
    else
        echo "Starting backend on http://localhost:17493 ..."
        {{ venv_bin }}/uvicorn backend.main:app --reload --port 17493 &
        backend_pid=$!
        sleep 2
    fi

    trap '[ -n "$backend_pid" ] && kill "$backend_pid" 2>/dev/null; wait' EXIT

    echo "Starting Tauri desktop app..."
    cd {{ tauri_dir }} && bun run tauri dev

# Start backend only
dev-backend: _ensure-venv
    {{ venv_bin }}/uvicorn backend.main:app --reload --port 17493

# Start Tauri desktop app only (backend must be running separately)
dev-frontend: _ensure-sidecar
    cd {{ tauri_dir }} && bun run tauri dev

# Kill all dev processes
kill:
    -pkill -f "uvicorn backend.main:app" 2>/dev/null || true
    -pkill -f "vite" 2>/dev/null || true
    @echo "Dev processes killed."

# ─── Build ────────────────────────────────────────────────────────────

# Build everything (server binary + desktop app)
build: build-server build-tauri

# Build Python server binary
build-server: _ensure-venv
    PATH="{{ venv_bin }}:$PATH" ./scripts/build-server.sh

# Build the desktop app, signed the same way every time so updates keep
# the app's privacy permissions
build-tauri:
    ./scripts/build-local-app.sh

# Pack the built app into Kass.dmg with the branded window (run `just build` first)
dmg:
    ./scripts/build-dmg.sh

# Check requirements, build, and install or update /Applications/Kass.app
install:
    ./scripts/install.sh

# Cut vX.Y.Z through the task board (tb release cut runs scripts/release.sh); CI builds and publishes the GitHub release
release version:
    tb release cut v{{ trim_start_match(version, "v") }}

# ─── Code Quality ────────────────────────────────────────────────────

# Run all checks (JS + Python lint + format)
check: check-js check-python check-rust

# JS/TS: lint + format (Biome), then typecheck app and tauri (tsc)
check-js:
    bun run check
    bun run typecheck

# Rust: format check (rustfmt) and lint (clippy, warnings fail)
check-rust:
    cd {{ tauri_dir }}/src-tauri && cargo fmt --check
    cd {{ tauri_dir }}/src-tauri && cargo clippy --all-targets --quiet -- -D warnings

# Python: lint + format check (ruff)
check-python: _ensure-venv
    {{ venv_bin }}/ruff check {{ backend_dir }}
    {{ venv_bin }}/ruff format --check {{ backend_dir }}

# Lint with Biome (JS) + ruff (Python)
lint: _ensure-venv
    bun run lint
    {{ venv_bin }}/ruff check {{ backend_dir }}

# Format with Biome (JS) + ruff (Python)
format: _ensure-venv
    bun run format
    {{ venv_bin }}/ruff format {{ backend_dir }}

# Fix lint + format issues (JS + Python)
fix: _ensure-venv
    bun run check:fix
    {{ venv_bin }}/ruff check {{ backend_dir }} --fix
    {{ venv_bin }}/ruff format {{ backend_dir }}

# Python lint only
lint-python: _ensure-venv
    {{ venv_bin }}/ruff check {{ backend_dir }}

# Python format only
format-python: _ensure-venv
    {{ venv_bin }}/ruff format {{ backend_dir }}

# Python auto-fix lint issues
fix-python: _ensure-venv
    {{ venv_bin }}/ruff check {{ backend_dir }} --fix
    {{ venv_bin }}/ruff format {{ backend_dir }}

# Run Python tests
test: _ensure-venv
    {{ venv_bin }}/python -m pytest {{ backend_dir }}/tests -v
    cd {{ tauri_dir }}/src-tauri && cargo test --quiet

# ─── Database ─────────────────────────────────────────────────────────

# Initialize SQLite database
db-init: _ensure-venv
    {{ python }} -c "from backend.database import init_db; init_db()"

# Reset database (delete + reinit)
db-reset:
    rm -f {{ backend_dir }}/data/kass.db
    just db-init

# ─── Utilities ────────────────────────────────────────────────────────


# Open API docs in browser
docs:
    open http://localhost:17493/docs

# Tail backend logs
logs:
    tail -f {{ backend_dir }}/logs/*.log 2>/dev/null || echo "No log files found"

# ─── Clean ────────────────────────────────────────────────────────────

# Clean build artifacts
clean:
    rm -rf {{ tauri_dir }}/src-tauri/target/release
    rm -rf {{ app_dir }}/dist

# Clean Python venv and cache
clean-python:
    rm -rf {{ venv }}
    find {{ backend_dir }} -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true

# Nuclear clean (everything including node_modules)
clean-all: clean clean-python
    rm -rf node_modules
    rm -rf {{ app_dir }}/node_modules
    rm -rf {{ tauri_dir }}/node_modules
    cd {{ tauri_dir }}/src-tauri && cargo clean

# ─── Internal ─────────────────────────────────────────────────────────

# Ensure venv exists (prompt to run setup if not)
[private]
_ensure-venv:
    #!/usr/bin/env bash
    if [ ! -d "{{ venv }}" ]; then
        echo "Python venv not found. Run: just setup"
        exit 1
    fi

# Ensure Tauri dev sidecar placeholder exists
[private]
_ensure-sidecar:
    bun run setup:dev
