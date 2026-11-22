"""Adapters Kass trains and ships inside the app (docs/plans/SHARED_ADAPTERS.md).

A new Mac starts from them: cleanup uses the shared cleanup adapter for its
model size, and recognition merges the shared voice adapter into turbo,
until the Mac's own training activates a personal adapter. Personal training
continues from the shared adapter, and a personal adapter built that way
records which one (its ``shared`` id). With the setting off, Kass uses the
stock models and the personal adapters that aren't built on a shared one.

Each adapter folder holds ``shared.json``, written by
scripts/shared-adapters/publish.py: its id, and for cleanup the pipeline and
flags it was tested with. A cleanup adapter tested with another pipeline is
never used, so a release that changes cleanup must retrain it.
"""

import json
import logging
from functools import lru_cache
from pathlib import Path

logger = logging.getLogger(__name__)

ROOT = Path(__file__).resolve().parent.parent / "assets" / "shared-adapters"
MANIFEST = "shared.json"
_enabled: bool | None = None
# Adapters that failed to load; unused until the next launch.
_broken: set[str] = set()


def enabled() -> bool:
    """Whether Kass uses its shared adapters (``CaptureSettings.shared_adapters``)."""
    global _enabled
    if _enabled is None:
        from ..database import session as database_session

        if database_session.SessionLocal is None:
            return True
        from .settings import get_capture_settings

        with database_session.SessionLocal() as db:
            _enabled = bool(getattr(get_capture_settings(db), "shared_adapters", True))
    return _enabled


def switched(value: bool) -> None:
    """The setting changed: cleanup picks it up on its next take; turbo reloads now."""
    global _enabled
    _enabled = bool(value)
    from .model_improvement.manager import _reload_speech

    _reload_speech()


def quarantine(path: str) -> None:
    logger.error("The shared adapter %s failed to load; using the stock model", path)
    _broken.add(str(path))


@lru_cache(maxsize=8)
def manifest(name: str) -> dict | None:
    try:
        return json.loads((ROOT / name / MANIFEST).read_text())
    except (OSError, ValueError):
        return None


def cleanup_id(model_size: str) -> str | None:
    """The id of this build's shared cleanup adapter for ``model_size``, whether or not it's on."""
    from .model_improvement.manager import pipeline_matches

    item = manifest(f"cleanup-{model_size}")
    if not item or not pipeline_matches(item.get("pipeline")):
        return None
    return item["id"]


def cleanup(model_size: str, flags: dict | None = None) -> str | None:
    """The shared cleanup adapter for ``model_size``, when it's on and tested with ``flags``."""
    if cleanup_id(model_size) is None or not enabled():
        return None
    if flags is not None and flags not in manifest(f"cleanup-{model_size}").get("tested_flags", []):
        return None
    path = str(ROOT / f"cleanup-{model_size}")
    return None if path in _broken else path


def voice_id(model_size: str) -> str | None:
    item = manifest(f"voice-{model_size}")
    return item["id"] if item else None


def voice(model_size: str) -> str | None:
    """The shared voice adapter to merge into ``model_size``, when it's on."""
    if voice_id(model_size) is None or not enabled():
        return None
    path = str(ROOT / f"voice-{model_size}")
    return None if path in _broken else path


def usable(lineage: str | None) -> bool:
    """Whether a personal adapter built on shared adapter ``lineage`` (None: on stock) may be used."""
    return lineage is None or enabled()
