# Python Style Guide

Target: **Python 3.12+** | Formatter/Linter: **Ruff** | Config: `backend/pyproject.toml`

This guide sets the conventions for Kass's Python model worker (`backend/`): Whisper speech-to-text, Qwen3 cleanup and command generation, and training, all on MLX. Older code that doesn't follow it yet should be brought in line as you touch it -- don't reformat entire files in unrelated PRs.

---

## Formatting

Enforced by `ruff format` (Black-compatible).

- **Line length**: 120 characters.
- **Indent**: 4 spaces. No tabs.
- **Trailing commas**: Required on multi-line function signatures, arguments, collections.
- **Quotes**: Double quotes (`"`) for strings. Single quotes are acceptable in f-string expressions and dict keys inside f-strings where avoiding escapes improves readability.

Run: `ruff format backend/`

---

## Imports

Enforced by ruff's `isort` rules (rule set `I`).

**Grouping** -- three blocks separated by a blank line:

```python
import asyncio                          # 1. stdlib
from pathlib import Path

import numpy as np                      # 2. third-party
from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

from backend.config import get_data_dir  # 3. local (absolute)
from ..services import transcribe       #    or relative
```

**Rules:**
- Within the `backend` package, use **relative imports** for sibling/child modules: `from ..services import transcribe`, `from ..utils.audio import load_audio`.
- Absolute imports are fine for top-level references from entry points (`main.py`, `server.py`).
- Never use wildcard imports (`from module import *`).
- One import per line for `from X import Y` when there are 4+ names; below that, comma-separated is fine.
- **Lazy imports** are acceptable for heavy dependencies (transformers, mlx, mlx_lm) inside functions to reduce startup time. Add a comment: `# lazy: heavy import`.

---

## Type Annotations

Python 3.12 means we use **built-in generics and union syntax natively**. No `from __future__ import annotations`, no `typing.List`/`typing.Dict`.

```python
# Yes
def process(items: list[str], config: dict[str, int] | None = None) -> tuple[int, str]: ...

# No
from typing import List, Dict, Optional, Tuple
def process(items: List[str], config: Optional[Dict[str, int]] = None) -> Tuple[int, str]: ...
```

**What to annotate:**
- All public function signatures (parameters + return type).
- Private functions: parameters at minimum; return type encouraged.
- Module-level variables: only when the type isn't obvious from the assignment.
- Route handlers: parameters are annotated via FastAPI's dependency injection. Add explicit `-> SomeResponse` return types when the route doesn't use `response_model`.

**Imports from `typing` that are still needed** (no built-in equivalent):
`Literal`, `TypeAlias`, `Protocol`, `runtime_checkable`, `Callable`, `Any`, `ClassVar`, `TypeVar`, `overload`, `TYPE_CHECKING`.

Use `collections.abc` for abstract types: `Sequence`, `Mapping`, `Iterable`, `Iterator`, `Generator`.

---

## Naming

| Thing | Convention | Example |
|-------|-----------|---------|
| Module | `snake_case` | `task_queue.py` |
| Class | `PascalCase` | `ProgressManager` |
| Function / method | `snake_case` | `get_llm_model` |
| Variable | `snake_case` | `sample_rate` |
| Constant | `UPPER_SNAKE_CASE` | `MAX_ALIGNMENTS` |
| Private | `_leading_underscore` | `_generations` |
| Type alias | `PascalCase` | `TokenCounts = dict[str, int]` |

**Specific conventions:**
- Pydantic request bodies are named for the call they carry: `Cleanup`, `RecognizeHead`, `KeepResident`. Add `Request`/`Response` suffixes when a route has a model for each side: `TokenCountsRequest`, `TokenCountsResponse`.
- Backend classes use engine-name prefix: `MLXSTTBackend`, `MLXQwenLLMBackend`.

---

## Docstrings

**Google style**. Required on all public functions, classes, and modules.

```python
def load_audio(
    path: str,
    sample_rate: int = 24000,
    mono: bool = True,
) -> tuple[np.ndarray, int]:
    """Load an audio file at the target sample rate.

    Decodes with librosa, which resamples on load, so callers never see the
    file's own rate.

    Args:
        path: Path to the audio file.
        sample_rate: Target sample rate. Defaults to 24000.
        mono: Mix down to one channel. Defaults to True.

    Returns:
        Tuple of (audio array, sample rate).
    """
```

**Short form** is fine for simple functions:

```python
def get_models_dir() -> Path:
    """Get models directory path."""
```

**When to skip**: Private helpers under ~5 lines where the name and signature make intent obvious.

**Module docstrings**: A single sentence at the top of every file describing its purpose.

```python
"""Fire-and-forget background tasks that are kept alive until they finish."""
```

---

## Comments

Comments explain **why**, not **what**. If the code needs a comment to explain what it does, the code should be rewritten to be clearer. The exceptions are non-obvious performance choices, external constraints, and concurrency/race-condition reasoning -- those always deserve a comment.

### No section dividers

Do not use ASCII dividers to create visual sections in files:

```python
# No -- any of these:
# ============================================
# CLEANUP ENDPOINTS
# ============================================

# ---------------------------------------------------------------------------
# Device detection
# ---------------------------------------------------------------------------

# --- Load model --------------------------------------------------
```

If a file needs section dividers to be navigable, the file is too long. Split it into modules. Within a function, if you need labeled sections to follow the logic, extract those sections into named functions.

### Inline comments

Inline comments (end-of-line) are fine when they add information the code can't express:

```python
# Yes -- explains a non-obvious constraint or gives context:
def listener(partial: str) -> None:  # MLX worker thread
MAX_HEAD_BYTES = 256 * 1024  # a bigger JSON head is a broken client

# No -- restates the code:
# Check if a cleanup with this id is already running
if request.id in _generations:

# Load the LLM backend
backend = llm_service.get_llm_model()

# Remove the alignment
_alignments.pop(key, None)
```

Delete comments that narrate what the next line of code obviously does. If the function name, variable name, or method call already communicates intent, the comment is noise.

### Block comments

Use block comments for **why** explanations -- constraints, workarounds, non-obvious decisions:

```python
# PyInstaller + multiprocessing: child processes re-execute the frozen binary
# with internal arguments. freeze_support() handles this and exits early.
multiprocessing.freeze_support()

# A recognition queued meanwhile goes first: the cleanup prefills it anyway.
with speculative_mlx_work():
    await backend.generate(prompt="", system=request.system, max_tokens=1, temperature=0)
```

Keep block comments tight. Two to three lines is normal. If you need a paragraph, it probably belongs in the docstring or a design doc.

### Linter/type-checker suppression

Always add a reason after `noqa` and `type: ignore`:

```python
from .app import app  # noqa: F401 -- re-export for uvicorn "backend.main:app"
```

Bare `# noqa` or `# type: ignore` with no explanation are not allowed.

### TODO / FIXME

Use sparingly. Every `TODO` must include a brief description of what needs doing. Don't use them as a substitute for tracking work properly:

```python
# TODO: bound alignments by memory rather than count once they keep the phrase's audio
MAX_ALIGNMENTS = 64
```

Never commit `HACK`, `XXX`, or `FIXME` -- fix the problem or file an issue.

### Commented-out code

Delete it. That's what git is for. If you need to document that something was intentionally removed, a short tombstone comment is acceptable:

```python
# Removed config.json-only check -- too lenient, doesn't confirm weights exist.
```

---

## Error Handling

Errors follow a **two-layer pattern**:

### 1. Services and backends -- raise built-in exceptions

Services and backends raise `ValueError` for bad input and `RuntimeError` for a broken environment, with a message that says what's wrong:

```python
# backend/services/commands.py
raise ValueError("Select text to rewrite first")
```

Don't add custom exception classes until a caller needs to tell two failures apart.

### 2. Route layer -- translate to HTTPException

Route handlers catch those exceptions and convert:

```python
@router.post("/recognize")
async def recognize(request: Request):
    try:
        head, samples = _parse_recognize(await request.body())
    except ValueError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
```

**Background work** (model startup, warm-up, training) catches `Exception` broadly and logs with `logger.exception()`, saying what happens instead, so one failed model doesn't stop the worker:

```python
except Exception:
    logger.exception("Couldn't reload the speech model; it reloads on next use")
```

A streaming route that has already started its response can't raise an `HTTPException`; it sends `{"error": ...}` as its last line instead.

**Never**: silently swallow exceptions, use bare `except:`, or catch `BaseException`.

---

## Async

1. **Don't declare `async def` unless the function awaits something.** Blocking work belongs in a sync function that the route runs off the event loop.
2. **CPU-bound work** that doesn't touch MLX (audio decoding, numpy operations) goes through `asyncio.to_thread()`:
   ```python
   path, duration = await asyncio.to_thread(_decoded, request.path)
   ```
3. **MLX work** (model loads, transcription, generation, unloads) runs on the single MLX thread through `run_on_mlx_thread()` (`services/mlx_thread.py`), never through `asyncio.to_thread()`. MLX's Metal stream belongs to the thread that first used it, so a model loaded on one thread and run on another fails, and one job at a time keeps a load and an unload from interleaving. Routes call the backend's methods (`transcribe`, `generate`), which do this for them. Head-start work that no result waits on, such as prefilling a style's prompt, goes inside `speculative_mlx_work()` so a recognition never queues behind it.
4. **Fire-and-forget tasks**: use `create_background_task()` (`services/task_queue.py`), which keeps a reference so the task isn't garbage collected mid-run:
   ```python
   create_background_task(shutdown_async())
   ```

---

## Logging

Use the `logging` module. Not `print()`.

```python
import logging

logger = logging.getLogger(__name__)

logger.info("Loading model %s on %s", model_name, device)
logger.warning("Cache miss for %s, downloading", repo_id)
logger.exception("Could not load startup %s model %s", name, size)  # logs traceback automatically
```

**Rules:**
- Use `%s`-style placeholders in log calls (not f-strings). This avoids formatting the string if the log level is filtered out.
- Use `logger.exception()` inside `except` blocks -- it captures the traceback.
- Logger name should be `__name__` (yields `backend.utils.audio`, etc.).
- Move any remaining `print()` call to logging when you touch its file.

---

## Constants

- Define at **module level** in the file where they're primarily used.
- Use `UPPER_SNAKE_CASE`.
- Data-directory paths and other settings shared across modules go in `backend/config.py`.
- Magic numbers in function bodies should be extracted to named constants:
  ```python
  # No
  if len(audio) > 24000 * 60 * 10:

  # Yes
  MAX_AUDIO_DURATION_SAMPLES = SAMPLE_RATE * 60 * 10
  if len(audio) > MAX_AUDIO_DURATION_SAMPLES:
  ```

---

## Function Signatures

- **Keyword-only arguments** (after `*`) for functions with 3+ parameters, especially when several share the same type:
  ```python
  def is_model_cached(
      hf_repo: str,
      *,
      weight_extensions: tuple[str, ...] = (".safetensors", ".bin"),
      required_files: list[str] | None = None,
  ) -> bool:
  ```
- Parameters on **separate lines** when the signature exceeds ~100 characters or has 3+ params.
- **Trailing comma** after the last parameter in multi-line signatures.
- Default values inline with the parameter.

---

## String Formatting

- **f-strings** for runtime string construction.
- **`%s`-style** for `logging` calls (lazy evaluation).
- **`.format()`**: avoid; f-strings are preferred.

---

## Testing

Framework: **pytest** with `pytest-asyncio`.

- Test files: `test_<module>.py` in `backend/tests/`.
- Use `conftest.py` for shared fixtures (environment variables, stubbed adapters, mock backends).
- Group related tests in classes: `class TestAudioopRuntime:`.
- Use `@pytest.mark.asyncio` for async tests.
- Use `@pytest.mark.parametrize` to reduce repetition.
- Manual integration scripts stay in `tests/` but are clearly marked (filename prefix `manual_` or documented in `tests/README.md`).

---

## Project Layout

```
backend/
  app.py              # FastAPI app: the /worker routers, startup and shutdown
  main.py             # Entry point (imports app, runs uvicorn)
  config.py           # Data directory paths
  models.py           # The /worker/health response
  server.py           # Worker launcher, parent-pid watchdog
  routes/             # /worker endpoints kass-server calls (validation, delegation, response formatting)
  services/           # Model calls, the MLX thread, Whisper artifact stripping, training
  backends/           # Whisper STT and Qwen3 LLM implementations (MLX)
  utils/              # Shared utilities (audio loading, memory, progress tracking)
  tests/              # pytest suite
```

kass-server (Rust) owns `kass.db`. Worker code gets what a call needs in the request body, or reads it from kass-server's routes (as model improvement does from `/capture/learning/training/*`). Don't add a database, ORM or SQL to the worker.

---

## Ruff Adoption

`pyproject.toml` configures ruff for linting and formatting. Run:

```bash
# Lint (check)
ruff check backend/

# Lint (auto-fix)
ruff check backend/ --fix

# Format
ruff format backend/
```

Introduce ruff fixes file-by-file as you touch them. Don't run `--fix` across the entire codebase in one shot -- that creates unreviewable diffs.
