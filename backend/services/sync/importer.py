"""
A shared Kass applying what a stand-in changed while it was away.

Each change carries the row as the stand-in last had it, or says it was
deleted, and when. Where both Macs changed the same row, the later edit
wins. Applying the same changes twice leaves the same result, so a batch
the stand-in sends again after a lost reply does no harm (take reports
aside: they're counts with no id of their own).

Reports are applied as rows, then everything learned from reports is
refreshed the way a new report refreshes it here.
"""

import logging
from datetime import datetime
from pathlib import Path

from sqlalchemy.orm import Session

from ... import config
from ...database.models import (
    AppStyle,
    Capture,
    CaptureFeedback,
    CaptureSettings,
    DictionaryEntry,
    TakeReport,
    WritingStyle,
)
from .. import capture_events, dictionary, styles
from .rows import KINDS, OWN_SETTINGS, edited_at, from_json

logger = logging.getLogger(__name__)

AUDIO_SUFFIXES = (".wav", ".mp3", ".m4a", ".flac", ".ogg", ".webm")


def _newer_here(row, changed_at: datetime) -> bool:
    here = edited_at(row)
    return here is not None and here > changed_at


def _upsert(db: Session, model: type, ref, values: dict, changed_at: datetime) -> bool:
    row = db.get(model, ref)
    if row is None:
        db.add(model(**values))
        return True
    if _newer_here(row, changed_at):
        return False
    for key, value in values.items():
        setattr(row, key, value)
    return True


def _style(db: Session, change: dict, changed_at: datetime) -> None:
    if change["deleted"]:
        row = db.get(WritingStyle, change["id"])
        if row is not None and not row.is_default and not _newer_here(row, changed_at):
            db.commit()
            styles.delete_style(db, row.id)
        return
    values = from_json(WritingStyle, change["row"])
    # One default: the shared Kass keeps its own.
    values.pop("is_default", None)
    _upsert(db, WritingStyle, change["id"], values, changed_at)


def _app_style(db: Session, change: dict, changed_at: datetime) -> None:
    row = db.get(AppStyle, change["id"])
    if change["deleted"]:
        if row is not None and not _newer_here(row, changed_at):
            db.delete(row)
        return
    values = from_json(AppStyle, change["row"])
    if db.get(WritingStyle, values.get("style_id")) is None:
        return
    _upsert(db, AppStyle, change["id"], values, changed_at)


def _settings(db: Session, change: dict, changed_at: datetime) -> None:
    if change["deleted"]:
        return
    row = db.get(CaptureSettings, 1)
    if row is None or _newer_here(row, changed_at):
        return
    for key, value in from_json(CaptureSettings, change["row"], skip=OWN_SETTINGS).items():
        setattr(row, key, value)


def _dictionary(db: Session, change: dict, changed_at: datetime) -> None:
    row = db.get(DictionaryEntry, change["id"])
    if change["deleted"]:
        if row is not None and not _newer_here(row, changed_at):
            db.delete(row)
        return
    values = from_json(DictionaryEntry, change["row"])
    # The same word added on both Macs under different ids: keep the later one.
    same_word = (
        db.query(DictionaryEntry)
        .filter(
            DictionaryEntry.scope == values.get("scope"),
            DictionaryEntry.scope_id == values.get("scope_id"),
            DictionaryEntry.key == values.get("key"),
            DictionaryEntry.id != change["id"],
        )
        .first()
    )
    if same_word is not None:
        if _newer_here(same_word, changed_at):
            return
        db.delete(same_word)
        db.flush()
    _upsert(db, DictionaryEntry, change["id"], values, changed_at)


def _save_audio(capture_id: str, filename: str, data: bytes) -> str:
    suffix = Path(filename).suffix.lower()
    if suffix not in AUDIO_SUFFIXES:
        suffix = ".wav"
    path = config.get_captures_dir() / f"{capture_id}{suffix}"
    path.write_bytes(data)
    return config.to_storage_path(path)


def _capture(db: Session, change: dict, audio: dict[str, tuple[str, bytes]]) -> None:
    from ..captures import delete_capture

    if change["deleted"]:
        delete_capture(change["id"], db)
        return
    values = from_json(Capture, change["row"])
    row = db.get(Capture, change["id"])
    sent = audio.get(change["id"])
    if row is None:
        if sent is not None:
            values["audio_path"] = _save_audio(change["id"], *sent)
        else:
            values["audio_deleted"] = values.get("audio_deleted") or bool(values.get("audio_path"))
        db.add(Capture(**values))
        return
    # Its recording and where it came from stay as the shared Kass has them.
    for key in ("audio_path", "audio_deleted", "mac_id", "mac_name", "created_at"):
        values.pop(key, None)
    for key, value in values.items():
        setattr(row, key, value)


def _feedback(db: Session, change: dict, changed: dict[tuple[str, str], bool]) -> None:
    row = db.get(CaptureFeedback, change["id"])
    if change["deleted"]:
        if row is not None:
            changed[(row.target, row.source)] = True
            db.delete(row)
            db.flush()
            dictionary.delete_added_by(db, row.id)
        return
    values = from_json(CaptureFeedback, change["row"])
    if db.get(Capture, values.get("capture_id")) is None:
        return
    changed.setdefault((values["target"], values["source"]), False)
    if row is None:
        db.add(CaptureFeedback(**values))
        return
    # Reports are never edited; a withdrawn one before it can trim its text.
    row.expected_text = values["expected_text"]


def _take(db: Session, change: dict) -> None:
    if not change["deleted"]:
        db.add(TakeReport(**from_json(TakeReport, change["row"], skip=frozenset({"id"}))))


def apply(db: Session, changes: list[dict], audio: dict[str, tuple[str, bytes]]) -> None:
    """Apply a stand-in's changes, oldest first within each kind. ``audio``
    holds new captures' recordings by capture id: (file name, bytes)."""
    from ..capture_feedback import reports_changed

    order = list(KINDS)
    ordered = sorted(changes, key=lambda change: (order.index(change["kind"]), change["changed_at"]))
    # Which reports changed, by target and source, and whether one went.
    reports: dict[tuple[str, str], bool] = {}
    for change in ordered:
        changed_at = datetime.fromisoformat(change["changed_at"])
        kind = change["kind"]
        if kind == "style":
            _style(db, change, changed_at)
        elif kind == "app_style":
            _app_style(db, change, changed_at)
        elif kind == "settings":
            _settings(db, change, changed_at)
        elif kind == "dictionary":
            _dictionary(db, change, changed_at)
        elif kind == "capture":
            _capture(db, change, audio)
        elif kind == "feedback":
            _feedback(db, change, reports)
        elif kind == "take":
            _take(db, change)
        db.flush()
    db.commit()
    styles.invalidate()
    dictionary.invalidate()
    for (target, source), withdrawn in reports.items():
        reports_changed(target, source, db, withdrawn=withdrawn)
    capture_events.publish("changed")
    capture_events.publish("learned")
    logger.info("Applied %d changes from a Mac that dictated while this one was away", len(changes))
