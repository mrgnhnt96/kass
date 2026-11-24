# Kass model worker

The Python process that runs Kass's models: Whisper speech-to-text, Qwen3 cleanup and command generation, and training. It has no public API. The Rust server (`kass-server` in `app/server`) owns kass.db and every HTTP and WebSocket route, and calls this worker's `/worker` endpoints on a private port. The app starts it as a sidecar; for development run it with `python -m backend.main`.

## Running

```bash
# From the repo root (recommended)
cargo xtask dev-backend

# Standalone, on a kass.db the Rust server created
python -m backend.main --host 127.0.0.1 --port 17494 --data-dir /path/to/data
```

The worker opens kass.db, which the app creates and migrates (`kass_server::db::prepare`), and fails clearly if tables or columns are missing. Before reporting ready, it loads the installed Whisper model selected in capture settings and the selected refinement model when auto-refine is enabled. A model load failure is logged without preventing the worker from opening.

## Architecture

```
backend/
  app.py                  # FastAPI app: the /worker routers, startup and shutdown
  main.py                 # Entry point (imports app, runs uvicorn)
  config.py               # Data directory paths and configuration
  models.py               # The /worker/health response
  server.py               # Worker launcher, parent-pid watchdog

  routes/                 # /worker endpoints the Rust server calls
  services/               # Model calls, Whisper artifact stripping, training
  backends/               # Whisper STT and Qwen3 LLM implementations (MLX)
  utils/                  # Audio loading, memory, progress tracking
```

### Key modules

**routes/stream_models.py** -- Recognize, alignments, cleanup and command generation, keep-resident and foreground: the calls a dictation makes.

**services/stt_artifacts.py** -- What Whisper writes that nobody said (loops, split laughs and spelled letters), taken out before the worker answers. The rest of cleanup, its prompt and text rules run in the Rust server (`app/text`, `kass_text::refinement`).

**services/model_improvement/** -- Voice and cleanup training. Kass's server sends the settings and style flags and prepares the training text; the worker reads reports and dictations from its `/capture/learning/training/*` routes, not from kass.db.

**backends/__init__.py** -- Protocol definitions (`STTBackend`, `LLMBackend`), model config registry, and factory functions.

### Platform

The backend runs on Apple Silicon Macs only. Whisper (mlx-audio) and the Qwen3 LLM (mlx-lm) both run on MLX with Metal acceleration. Startup fails with a clear error on any other platform or when MLX cannot load (`utils/platform_detect.py`).

## API

The worker only answers `/worker/*` and `/health`, on 127.0.0.1. Kass's API listens on port 17493 in the Rust server, which serves every route from kass.db and calls the worker for model work. Interactive docs for the worker endpoints are at `http://localhost:17494/docs` while it runs.

## Data directory

```
{data_dir}/
  kass.db             # SQLite database
  captures/               # Recorded dictation audio
```

The app passes its own (`~/Library/Application Support/com.mrgnhnt.kass`) with `--data-dir`. Without that flag the worker uses `KASS_DATA_DIR`, or `data/` in the directory it starts in. `cargo xtask dev` and `cargo xtask dev-backend` set `KASS_DATA_DIR` to the app's folder, so the debug app and the worker open the same kass.db.

## Code quality

Linting and formatting are enforced by [ruff](https://docs.astral.sh/ruff/), configured in `pyproject.toml`. See `STYLE_GUIDE.md` for conventions.

```bash
cargo xtask check-python   # lint + format check
cargo xtask fix            # auto-fix lint issues + reformat
cargo xtask test           # run pytest, then the app's Rust tests
```

## Dependencies

Runtime dependencies are in `requirements.txt`. mlx-lm and mlx-audio are installed separately with `--no-deps` (see the note in that file). Dev tools (ruff, pytest) are installed automatically by `cargo xtask setup-python`.
