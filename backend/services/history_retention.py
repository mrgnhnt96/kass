"""History retention: delete old captures without forgetting what they taught.

Captures older than the user's window (``CaptureSettings.history_retention_days``,
0 keeps them forever) are deleted with their audio at startup and every hour.
Before a capture goes, everything learned from it moves somewhere that lasts
(docs/plans/HISTORY_RETENTION.md):

- its corrections stay, with the app and style they teach copied onto them
  and their audio moved to the correction audio folder (unless the user keeps
  no recordings, audio_retention.py);
- usage stats get a ``RetiredCapture`` row of its numbers;
- the names it wrote go into ``KnownName``.

Each batch folds and deletes in one transaction, so a crash leaves the
captures as they were, and a folded capture is deleted in the same commit, so
it is never folded twice. Audio files go after the commit; a crash before
then leaves files with no capture, which the next sweep deletes once they are
older than the window.

Nothing is deleted until the user confirms the window
(``history_retention_confirmed``): saving the setting does, and so does
answering the dialog the app shows at launch while old captures are waiting.
With nothing old to delete there is nothing to ask, so it confirms itself.

The newest capture, the current take, is never deleted, nor one being
refined or retranscribed (``in_use``).
"""

import asyncio
import json
import logging
import shutil
import threading
from collections import Counter
from contextlib import contextmanager, suppress
from datetime import UTC, datetime, timedelta

from sqlalchemy import func
from sqlalchemy.dialects.sqlite import insert

from .. import config
from ..database import session as database_session
from ..database.models import Capture, CaptureFeedback, KnownName, RetiredCapture

logger = logging.getLogger(__name__)

FOREVER = 0
SWEEP_SECONDS = 60 * 60
BATCH = 200

_lock = threading.Lock()
_busy: Counter[str] = Counter()


def hold(capture_id: str) -> None:
    """Keep ``capture_id`` from being deleted until it's released as often as it was held."""
    with _lock:
        _busy[capture_id] += 1


def release(capture_id: str) -> None:
    """Undo one :func:`hold`; releasing one that isn't held does nothing."""
    with _lock:
        if _busy[capture_id] > 1:
            _busy[capture_id] -= 1
        else:
            del _busy[capture_id]


@contextmanager
def in_use(capture_id: str):
    """Keep ``capture_id`` from being deleted while the block runs."""
    hold(capture_id)
    try:
        yield
    finally:
        release(capture_id)


def correction_audio_dir():
    path = config.get_data_dir() / "correction-audio"
    path.mkdir(parents=True, exist_ok=True)
    return path


def cutoff(days: int, now: datetime | None = None) -> datetime | None:
    """Captures created before this (naive UTC) are past the window. None keeps all."""
    if days <= FOREVER:
        return None
    return (now or datetime.utcnow()) - timedelta(days=days)


def _expired(db, days: int, now: datetime | None = None):
    before = cutoff(days, now)
    if before is None:
        return None
    newest = db.query(Capture.id).order_by(Capture.created_at.desc(), Capture.id.desc()).limit(1).scalar()
    query = db.query(Capture).filter(Capture.created_at < before)
    if newest is not None:
        query = query.filter(Capture.id != newest)
    return query


def count_expiring(db, days: int, now: datetime | None = None) -> int:
    """How many captures a window of ``days`` would delete now."""
    query = _expired(db, days, now)
    return 0 if query is None else query.count()


def status(db, now: datetime | None = None) -> tuple[int, bool, int]:
    """The window, whether it is confirmed, and how many captures it would delete.

    An unconfirmed window with nothing to delete is confirmed here: asking
    only matters when something would go.
    """
    from .settings import get_capture_settings

    saved = get_capture_settings(db)
    expiring = count_expiring(db, saved.history_retention_days, now)
    if not saved.history_retention_confirmed and expiring == 0:
        saved.history_retention_confirmed = True
        db.commit()
    return saved.history_retention_days, saved.history_retention_confirmed, expiring


def _words(text: str | None) -> int:
    # As usage stats count them (server/src/usage/stats.rs): runs of non-space.
    return len(text.split()) if text else 0


def _delivered(row: Capture) -> str:
    # usage stats: COALESCE(NULLIF(TRIM(refined), ''), raw). SQLite's TRIM strips spaces only.
    refined = row.transcript_refined
    return refined if refined is not None and refined.strip(" ") else row.transcript_raw


def _keep_audio(row: Capture) -> str | None:
    """Copy a corrected capture's audio to the correction audio folder; its new storage path."""
    source = config.resolve_storage_path(row.audio_path)
    if source is None or not source.is_file():
        return None
    target = correction_audio_dir() / source.name
    shutil.copy2(source, target)
    return config.to_storage_path(target)


def _fold(db, rows: list[Capture], keep_audio: bool = True) -> None:
    """Move what ``rows`` taught into lasting storage. The caller commits.

    ``keep_audio`` False keeps no recording for corrections: the user keeps none
    (audio_retention.py).
    """
    from .phrase_seams import mid_sentence_capitals

    ids = [row.id for row in rows]
    feedback: dict[str, list[CaptureFeedback]] = {}
    for report in db.query(CaptureFeedback).filter(CaptureFeedback.capture_id.in_(ids)):
        feedback.setdefault(report.capture_id, []).append(report)

    names: dict[str, datetime] = {}
    for row in rows:
        reports = feedback.get(row.id, [])
        created = row.created_at or datetime.utcnow()
        db.execute(
            insert(RetiredCapture)
            .values(
                capture_id=row.id,
                created_at=created,
                source=row.source,
                app_bundle_id=row.app_bundle_id,
                words=_words(_delivered(row)),
                raw_words=_words(row.transcript_raw),
                duration_ms=row.duration_ms,
                fixed=bool(reports),
            )
            .on_conflict_do_nothing(index_elements=["capture_id"])
        )
        # known_names.py reads cleaned dictation, not a command's rewrite.
        if row.transcript_refined and row.source != "command":
            for name in mid_sentence_capitals(row.transcript_refined):
                names[name] = max(names.get(name, created), created)
        audio = _keep_audio(row) if reports and keep_audio else None
        for report in reports:
            report.app_bundle_id = row.app_bundle_id
            report.teaches_style_id = row.teaches_style_id
            if audio is not None:
                try:
                    snapshot = json.loads(report.snapshot)
                    snapshot["audio_path"] = audio
                    report.snapshot = json.dumps(snapshot)
                except (ValueError, TypeError):
                    logger.warning("Could not move the audio of correction %s", report.id)

    for name, seen in names.items():
        statement = insert(KnownName).values(name=name, last_seen_at=seen)
        db.execute(
            statement.on_conflict_do_update(
                index_elements=["name"],
                set_={"last_seen_at": func.max(KnownName.last_seen_at, statement.excluded.last_seen_at)},
            )
        )


def _remove_audio(paths: list[str]) -> None:
    for stored in paths:
        resolved = config.resolve_storage_path(stored)
        if resolved is not None and resolved.is_file():
            try:
                resolved.unlink()
            except OSError:
                logger.exception("Could not remove capture audio %s", resolved)


def _remove_orphan_audio(db, before: datetime) -> int:
    """Delete capture audio older than the window that no capture points to."""
    captures_dir = config.get_captures_dir()
    known = {capture_id for (capture_id,) in db.query(Capture.id)}
    removed = 0
    for path in captures_dir.iterdir():
        if not path.is_file() or path.stem in known:
            continue
        try:
            # A dictation in progress writes its file before its capture exists; it is new.
            if datetime.fromtimestamp(path.stat().st_mtime, UTC).replace(tzinfo=None) >= before:
                continue
            path.unlink()
            removed += 1
        except OSError:
            logger.exception("Could not remove orphan capture audio %s", path)
    return removed


def sweep(now: datetime | None = None) -> int:
    """Fold and delete every capture past a confirmed window. Returns how many went. Blocking."""
    if database_session.SessionLocal is None:
        return 0
    from .settings import get_capture_settings

    deleted = 0
    with database_session.SessionLocal() as db:
        keep_audio = not get_capture_settings(db).discard_audio
        days, confirmed, _ = status(db, now)
        before = cutoff(days, now)
        if before is None or not confirmed:
            return 0
        while True:
            with _lock:
                query = _expired(db, days, now)
                if _busy:
                    query = query.filter(Capture.id.notin_(list(_busy)))
                rows = query.order_by(Capture.created_at).limit(BATCH).all()
                if not rows:
                    break
                audio = [row.audio_path for row in rows if row.audio_path]
                try:
                    _fold(db, rows, keep_audio)
                    db.query(Capture).filter(Capture.id.in_([row.id for row in rows])).delete(synchronize_session=False)
                    db.commit()
                except Exception:
                    db.rollback()
                    raise
                db.expunge_all()
            _remove_audio(audio)
            deleted += len(rows)
        orphans = _remove_orphan_audio(db, before)
    if deleted:
        from . import known_names, personal_examples

        personal_examples.invalidate()
        known_names.invalidate()
        logger.info("History retention deleted %d captures older than %d days", deleted, days)
    if orphans:
        logger.info("History retention deleted %d audio files with no capture", orphans)
    return deleted


async def periodic_job():
    """Sweep at startup, then hourly."""
    while True:
        # Shield the worker so shutdown waits for its transaction to finish.
        worker = asyncio.create_task(asyncio.to_thread(sweep))
        try:
            await asyncio.shield(worker)
        except asyncio.CancelledError:
            with suppress(Exception):
                await worker
            raise
        except Exception:
            logger.exception("History retention sweep failed; keeping the captures")
        await asyncio.sleep(SWEEP_SECONDS)
