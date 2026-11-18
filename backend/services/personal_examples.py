"""The user's own "when I say this, I mean this" examples.

Two sources feed it: corrections to refined output (Teach Kass) and
calibration rewrites the user edited. Only explicit corrections count: a
redictation report teaches correction learning alone. Cleanup shows the model the same
recent examples on every dictation, so a correction counts on the very next
dictation, and the model's prompt cache keeps the wait short. Examples too old
to fit are summarized into correction notes, so none stop counting.

Corrections are immutable training records, so removing one from the user's
examples hides it here without deleting the record.

Examples belong to a writing style (docs/plans/PER_APP_STYLE.md): a
correction to the style its capture's app is assigned to now, a calibration
rewrite to the style it was run for. Cleanup in a style sees only its own.
"""

import json
import logging
import threading

from . import writing_style
from .refinement import strip_stt_artifacts

logger = logging.getLogger(__name__)

MAX_EXAMPLE_CHARS = 800
# Enough to show how the user writes without bloating the prompt.
MAX_PROMPT_EXAMPLES = 16
MAX_PROMPT_CHARS = 6000

_lock = threading.RLock()
# Style id -> its examples, newest first.
_cache: dict[str, list[dict]] | None = None


def invalidate() -> None:
    global _cache
    with _lock:
        _cache = None


def _from_corrections() -> list[dict]:
    from sqlalchemy import func

    from ..database import session as database_session
    from ..database.models import Capture, CaptureFeedback

    if database_session.SessionLocal is None:
        return []
    with database_session.SessionLocal() as db:
        rows = (
            # A correction whose capture history retention deleted keeps its own copy.
            db.query(CaptureFeedback, func.coalesce(Capture.teaches_style_id, CaptureFeedback.teaches_style_id))
            .outerjoin(Capture, Capture.id == CaptureFeedback.capture_id)
            .filter(CaptureFeedback.target == "refined", CaptureFeedback.teaches_style())
            .order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id.desc())
            .all()
        )
    examples = []
    seen = set()
    for row, teaches in rows:
        # The latest correction of a capture replaces earlier ones.
        if row.capture_id in seen:
            continue
        seen.add(row.capture_id)
        try:
            snapshot = json.loads(row.snapshot)
            # Captures saved before Whisper's loops were stripped at the source.
            said = strip_stt_artifacts(snapshot.get("transcript_raw") or "")
        except (ValueError, TypeError, AttributeError):
            continue
        if said and row.expected_text:
            examples.append(
                {
                    "id": f"correction:{row.id}",
                    "source": "correction",
                    "said": said,
                    "meant": row.expected_text,
                    "created_at": row.created_at.isoformat() if row.created_at else None,
                    "app_bundle_id": snapshot.get("app_bundle_id"),
                    "app_name": snapshot.get("app_name"),
                    "teaches_style_id": teaches,
                }
            )
    return examples


def _by_style() -> dict[str, list[dict]]:
    from .styles import correction_style, snapshot

    try:
        corrections = _from_corrections()
    except Exception:
        logger.warning("Could not load corrections as examples", exc_info=True)
        corrections = []
    styles = snapshot()
    hidden = set(writing_style.hidden_examples())
    grouped: dict[str, list[dict]] = {style.id: writing_style.calibration_examples(style.id) for style in styles.styles}
    for example in corrections:
        style = correction_style(styles, example["app_bundle_id"], example.pop("teaches_style_id"))
        grouped.setdefault(style, []).append(example)
    for style_id, examples in grouped.items():
        examples = [
            e
            for e in examples
            if e["id"] not in hidden
            and max(len(e["said"]), len(e["meant"])) <= MAX_EXAMPLE_CHARS
            and e["said"] != e["meant"]
        ]
        examples.sort(key=lambda e: e["created_at"] or "", reverse=True)
        grouped[style_id] = examples
    return grouped


def all_examples(style: str | None = None) -> list[dict]:
    """Every example cleanup in ``style`` may use, newest first. None is the default style."""
    global _cache
    from .styles import default_id

    with _lock:
        if _cache is None:
            _cache = _by_style()
        return list(_cache.get(style or default_id(), []))


def in_prompt(style: str | None = None) -> list[dict]:
    """The most recent examples of ``style`` that fit the prompt budget, newest first.

    Older examples are summarized into correction notes instead.
    """
    chosen, size = [], 0
    for example in all_examples(style)[:MAX_PROMPT_EXAMPLES]:
        size += len(example["said"]) + len(example["meant"])
        if size > MAX_PROMPT_CHARS:
            break
        chosen.append(example)
    return chosen


def for_prompt(style: str | None = None, extra: list[tuple[str, str]] | None = None) -> list[tuple[str, str]]:
    """The examples every cleanup in ``style`` shows the model, oldest first.

    Every dictation in a style gets the same list, so the cleanup model's
    cached prompt covers it and only the new transcript is read. A new example
    goes at the end, keeping everything before it cached. ``extra`` examples,
    such as a calibration run's rewrites before they are saved, come last.
    """
    chosen = [(example["said"], example["meant"]) for example in in_prompt(style)]
    return [*reversed(chosen), *(extra or [])]
