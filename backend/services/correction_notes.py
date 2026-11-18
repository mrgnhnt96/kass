"""Short rules summarized from the user's older examples, read for cleanup.

Cleanup shows the model only the most recent examples, so without these an
example would stop counting once enough newer ones pushed it out. The Rust
server summarizes them in an idle-time job and owns the file
(tauri/src-tauri/server/src/correction_notes.rs and learned.rs); the worker
only reads the notes into the cleanup prompt, and rereads the file when the
server says it changed (``/worker/learned/changed``).

Each writing style (styles.py) has its own notes. ``style`` arguments are
style ids; None is the default style.
"""

import json
import logging
import threading

from .. import config

logger = logging.getLogger(__name__)

_lock = threading.RLock()
_state = None


def _path():
    return config.get_data_dir() / "correction-notes.json"


def _empty() -> dict:
    """One style's notes."""
    return {
        "notes": [],
        "considered_ids": [],
        "history": [],
        "last_run": None,
        "outcome": "waiting",
        "metrics": None,
    }


def _read() -> dict:
    """The whole file. Version 1 held one list; it becomes the migrated style's."""
    global _state
    with _lock:
        if _state is None:
            state = {"version": 2, "styles": {}}
            try:
                loaded = json.loads(_path().read_text())
                if loaded.get("version") == 1:
                    from .styles import MIGRATED_STYLE

                    state["styles"][MIGRATED_STYLE] = {k: v for k, v in loaded.items() if k != "version"}
                elif loaded.get("version") == 2 and isinstance(loaded.get("styles"), dict):
                    state = loaded
                else:
                    raise ValueError("Unsupported correction notes")
            except FileNotFoundError:
                pass
            except (OSError, ValueError, TypeError):
                logger.exception("Could not read correction notes; starting without them")
            _state = state
        return _state


def reload() -> None:
    """Read the file afresh: the Rust server changed it (``/worker/learned/changed``)."""
    global _state
    with _lock:
        _state = None


def _style_id(style: str | None) -> str:
    if style is not None:
        return style
    from .styles import default_id

    return default_id()


def _load(style: str | None = None) -> dict:
    return {**_empty(), **_read()["styles"].get(_style_id(style), {})}


def notes(style: str | None = None) -> list[str]:
    return list(_load(style)["notes"])


def prompt_section(style: str | None = None, candidate: list[str] | None = None) -> str | None:
    """Refinement instructions from the user's older corrections in ``style``."""
    current = notes(style) if candidate is None else candidate
    if not current:
        return None
    lines = "\n".join(f"- {note}" for note in current)
    return f"Rules this speaker taught in earlier corrections. Follow them:\n{lines}"
