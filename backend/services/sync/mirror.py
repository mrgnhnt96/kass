"""
What dictation reads, copied from a shared Kass to a stand-in, so dictating
on the stand-in sounds the same: settings, styles, the dictionary, what was
learned from corrections and the trained models in use.

The shared Kass serves a :func:`snapshot`; the stand-in replaces its own
copy with it (:func:`apply`), never recording that as a change of its own.
A stand-in only copies once everything it changed has been sent, so nothing
it did is lost underneath the copy.
"""

import hashlib
import json
import logging
import os
from collections.abc import Callable
from pathlib import Path

from sqlalchemy.orm import Session

from ... import config
from ...database.models import AppStyle, CaptureSettings, DictionaryEntry, KnownName, WritingStyle
from .changes import not_recorded
from .rows import MIRRORED, OWN_SETTINGS, from_json, to_json

logger = logging.getLogger(__name__)

# What was learned from corrections, kept in the data dir.
LEARNED_FILES = ("writing-style.json", "correction-notes.json", "correction-learning.json")

# Fetches a trained model's file from the shared Kass into a path:
# (kind, file name, destination).
Download = Callable[[str, str, Path], None]


def _adapter_files(item: dict) -> list[str]:
    path = Path(item["path"])
    return sorted(entry.name for entry in path.iterdir() if entry.is_file())


def in_use_adapters() -> dict:
    """The trained models in use, with their files, minus where they are on this Mac."""
    from ..model_improvement import manager

    adapters = {}
    for kind, item in manager.in_use().items():
        if "path" not in item:
            adapters[kind] = item
            continue
        try:
            files = _adapter_files(item)
        except OSError:
            continue
        adapters[kind] = {**{key: value for key, value in item.items() if key != "path"}, "files": files}
    return adapters


def adapter_file(kind: str, name: str) -> Path | None:
    """One file of the trained model in use for ``kind``, for a stand-in to copy."""
    from ..model_improvement import manager

    item = manager.in_use().get(kind)
    if not item or "path" not in item:
        return None
    path = Path(item["path"])
    if name not in _adapter_files(item):
        return None
    return path / name


def _read_learned(name: str):
    try:
        return json.loads((config.get_data_dir() / name).read_text())
    except (OSError, ValueError):
        return None


def snapshot(db: Session) -> dict:
    """Everything a stand-in copies, with a revision that changes when any of it does."""
    settings = db.get(CaptureSettings, 1)
    data = {
        "settings": settings and {key: value for key, value in to_json(settings).items() if key not in OWN_SETTINGS},
        **{name: [to_json(row) for row in db.query(model).all()] for name, model in MIRRORED.items()},
        "files": {name: _read_learned(name) for name in LEARNED_FILES},
        "adapters": in_use_adapters(),
    }
    encoded = json.dumps(data, sort_keys=True, default=str).encode()
    return {**data, "revision": hashlib.sha256(encoded).hexdigest()}


def _replace_rows(db: Session, data: dict) -> None:
    settings = db.get(CaptureSettings, 1)
    if settings is not None and data.get("settings"):
        for key, value in from_json(CaptureSettings, data["settings"], skip=OWN_SETTINGS).items():
            setattr(settings, key, value)
    for model in (AppStyle, DictionaryEntry, KnownName, WritingStyle):
        db.query(model).delete()
    db.flush()
    for name in ("styles", "app_styles", "dictionary", "known_names"):
        model = MIRRORED[name]
        db.add_all(model(**from_json(model, row)) for row in data.get(name, []))
        db.flush()
    db.commit()


def _write_learned(files: dict) -> None:
    for name in LEARNED_FILES:
        path = config.get_data_dir() / name
        content = files.get(name)
        if content is None:
            path.unlink(missing_ok=True)
            continue
        temporary = path.with_suffix(".mirror.tmp")
        temporary.write_text(json.dumps(content, indent=2, ensure_ascii=False))
        os.replace(temporary, path)


def _copy_adapters(adapters: dict, download: Download) -> dict:
    """The shared Kass's trained models, copied here where they aren't yet, as this Mac's items."""
    from ..model_improvement import manager

    active = {}
    for kind, item in adapters.items():
        if "files" not in item:
            active[kind] = item
            continue
        folder = manager.mirrored_root() / item["id"] / kind
        try:
            for name in item["files"]:
                if Path(name).name != name:
                    raise ValueError(f"Not a file name: {name!r}")
                destination = folder / name
                if not destination.exists():
                    folder.mkdir(parents=True, exist_ok=True)
                    download(kind, name, destination)
        except Exception:
            logger.warning("Could not copy the shared Kass's %s model", kind, exc_info=True)
            continue
        active[kind] = {**{key: value for key, value in item.items() if key != "files"}, "path": str(folder)}
    return active


def _forget_cached() -> None:
    """Read everything that was copied afresh."""
    from .. import (
        correction_learning,
        correction_notes,
        dictionary,
        known_names,
        personal_examples,
        styles,
        writing_style,
    )

    writing_style._state = None
    correction_notes._state = None
    correction_learning._state = None
    correction_learning.initialize()
    personal_examples.invalidate()
    known_names.invalidate()
    styles.invalidate()
    dictionary.invalidate()


def apply(db: Session, data: dict, download: Download) -> None:
    """Replace this Mac's copy with the shared Kass's :func:`snapshot`."""
    from .. import capture_events
    from ..model_improvement import manager

    with not_recorded():
        _replace_rows(db, data)
        _write_learned(data.get("files") or {})
        manager.use_mirrored(_copy_adapters(data.get("adapters") or {}, download))
    _forget_cached()
    capture_events.publish("learned")
