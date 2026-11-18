# Kass Backend

FastAPI server powering Kass dictation: Whisper speech-to-text followed by a local LLM cleanup pass. Runs locally as a Tauri sidecar or standalone via `python -m backend.main`.

## Running

```bash
# From the repo root (recommended)
cargo xtask dev-backend

# Standalone
python -m backend.main --host 127.0.0.1 --port 17494

# With custom data directory
python -m backend.main --data-dir /path/to/data
```

The server auto-initializes the SQLite database on first startup. Before reporting ready, it loads the installed Whisper model selected in capture settings and the selected refinement model when auto-refine is enabled. Missing models are left to the existing download/setup flow; a model load failure is logged without preventing the server from opening. Other models are downloaded from HuggingFace on first use.

## Architecture

```
backend/
  app.py                  # FastAPI app factory, CORS, lifecycle events
  main.py                 # Entry point (imports app, runs uvicorn)
  config.py               # Data directory paths and configuration
  models.py               # Pydantic request/response schemas
  server.py               # Tauri sidecar launcher, parent-pid watchdog

  routes/                 # Thin HTTP handlers — validation, delegation, response formatting
  services/               # Business logic, CRUD, orchestration
  backends/               # Whisper STT and Qwen3 LLM implementations (MLX)
  database/               # ORM models, session management (the app creates kass.db)
  utils/                  # Shared utilities (audio loading, progress tracking)
```

### Request flow

```
HTTP request
  -> routes/        (validate input, parse params)
  -> services/      (business logic, database queries, orchestration)
  -> backends/      (STT / LLM inference)
  -> utils/         (audio loading, progress tracking)
```

Route handlers are intentionally thin. They validate input, delegate to a service function, and format the response. All business logic lives in `services/`.

### Key modules

**services/captures.py**, **services/capture_stream.py** -- Dictation captures: upload or stream audio, run Whisper, persist the capture, and chain refinement.

**services/refinement.py** -- Builds the cleanup prompt and runs the local Qwen3 LLM over a raw transcript.

**backends/__init__.py** -- Protocol definitions (`STTBackend`, `LLMBackend`), model config registry, and factory functions.

**backends/base.py** -- Shared utilities used by the backends: HuggingFace cache checks, progress tracking.

**database/** -- SQLAlchemy ORM models with a re-exporting `__init__.py` for backward compatibility. Migrations run automatically on startup.

### Platform

The backend runs on Apple Silicon Macs only. Whisper (mlx-audio) and the Qwen3 LLM (mlx-lm) both run on MLX with Metal acceleration. Startup fails with a clear error on any other platform or when MLX cannot load (`utils/platform_detect.py`).

## API

Full interactive documentation is available at `http://localhost:17494/docs` when the server is running. In the app, Kass's API listens on port 17493 (the `kass-server` crate in `tauri/src-tauri/server`) and passes every request on to this server on 17494.

| Domain | Prefix | Description |
|--------|--------|-------------|
| Health | `/`, `/health` | Server status, filesystem checks, shutdown |
| Transcription | `/transcribe` | Whisper-based audio-to-text |
| Captures | `/captures`, `/capture` | Dictation captures, refinement, feedback, readiness, correction learning |
| Streaming dictation | `WS /captures/stream` | PCM recognition/refinement during recording, with one final capture |
| LLM | `/llm/generate` | Local Qwen3 text generation |
| Writing style | `/writing-style` | Learned punctuation habits and personal examples |
| Settings | `/settings/captures` | Capture and refinement defaults |
| Models | `/models` | Download, unload, delete, migrate, status |
| Tasks | `/tasks` | Active download tracking |

## Data directory

```
{data_dir}/
  kass.db             # SQLite database
  captures/               # Recorded dictation audio
```

Default location is the OS-specific app data directory. Override with `--data-dir` or the `KASS_DATA_DIR` environment variable.

## Code quality

Linting and formatting are enforced by [ruff](https://docs.astral.sh/ruff/), configured in `pyproject.toml`. See `STYLE_GUIDE.md` for conventions.

```bash
cargo xtask check-python   # lint + format check
cargo xtask fix            # auto-fix lint issues + reformat
cargo xtask test           # run pytest, then the app's Rust tests
```

## Dependencies

Runtime dependencies are in `requirements.txt`. mlx-lm and mlx-audio are installed separately with `--no-deps` (see the note in that file). Dev tools (ruff, pytest) are installed automatically by `cargo xtask setup-python`.
