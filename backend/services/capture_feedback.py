"""Local correction records for evaluation and future training datasets."""

import json
import logging

from sqlalchemy.orm import Session

from ..database.models import CaptureFeedback
from ..models import CaptureFeedbackCreate, CaptureFeedbackResponse
from . import correction_learning, known_names, personal_examples, writing_style
from .captures import get_capture
from .spelling import join_spelling
from .text_merge import take_back

logger = logging.getLogger(__name__)


def to_response(row: CaptureFeedback) -> CaptureFeedbackResponse:
    return CaptureFeedbackResponse(
        id=row.id,
        capture_id=row.capture_id,
        target=row.target,
        expected_text=row.expected_text,
        notes=row.notes,
        snapshot=json.loads(row.snapshot),
        source=row.source,
        created_at=row.created_at,
    )


def save_feedback(capture_id: str, request: CaptureFeedbackCreate, db: Session, filed_by: str | None = None):
    """Keep a correction and what it teaches. ``filed_by`` is the voice edit
    capture that filed it, which takes it back when deleted. A request that
    ``replaces`` an earlier report of the capture amends it: the earlier one
    goes, with what it taught, in the same commit."""
    capture = get_capture(capture_id, db)
    if capture is None:
        return None
    if capture != request.snapshot:
        raise ValueError("Capture changed. Refresh it before reporting a correction.")
    original = capture.transcript_raw if request.target == "raw" else capture.transcript_refined
    if original is None:
        raise ValueError("This capture has no refined output to report.")
    expected = request.expected_text
    # Spoken fixes come from speech: letters the user spelled ("M-E-G-H-A-N")
    # are one word, as they would have typed it.
    if request.source != "manual":
        expected = join_spelling(expected)
    if expected == original:
        raise ValueError("Expected output must differ from the model output.")
    replaced = None
    if request.replaces is not None:
        replaced = db.get(CaptureFeedback, request.replaces)
        if replaced is None or replaced.capture_id != capture_id or replaced.target != request.target:
            raise ValueError("The correction being amended is gone. Refresh the capture.")
        db.delete(replaced)
    row = CaptureFeedback(
        capture_id=capture_id,
        target=request.target,
        expected_text=expected,
        notes=request.notes.strip(),
        snapshot=capture.model_dump_json(),
        source=request.source,
        filed_by=filed_by,
    )
    db.add(row)
    db.commit()
    db.refresh(row)
    _reports_changed(row.target, row.source, db, withdrawn=replaced is not None)
    return to_response(row)


def latest_feedback(db: Session, capture_id: str, target: str) -> CaptureFeedback | None:
    """The capture's newest report of ``target``: its text as corrected so far."""
    return (
        db.query(CaptureFeedback)
        .filter(CaptureFeedback.capture_id == capture_id, CaptureFeedback.target == target)
        .order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id.desc())
        .first()
    )


def withdraw_feedback(capture_id: str, report_id: str, db: Session) -> bool:
    """Delete one report and everything it taught.

    A capture's reports stack, each made from the one before, so the newer
    ones lose this one's changes and keep their own (text_merge); one left
    changing nothing goes too. Examples, habits and names are read from the
    reports, so they drop it at once. Rules and the cleanup adapter are
    relearned without it at the next idle moment
    (correction_learning.request_run).
    """
    row = db.get(CaptureFeedback, report_id)
    if row is None or row.capture_id != capture_id:
        return False
    stack = (
        db.query(CaptureFeedback)
        .filter(CaptureFeedback.capture_id == capture_id, CaptureFeedback.target == row.target)
        .order_by(CaptureFeedback.created_at, CaptureFeedback.id)
        .all()
    )
    at = stack.index(row)
    snapshot = json.loads(row.snapshot)
    original = snapshot.get("transcript_raw" if row.target == "raw" else "transcript_refined") or ""
    before = stack[at - 1].expected_text if at else original
    changed = {(row.target, row.source)}
    for newer in stack[at + 1 :]:
        changed.add((newer.target, newer.source))
        text = take_back(before, row.expected_text, newer.expected_text)
        if text == original:
            db.delete(newer)
        else:
            newer.expected_text = text
    db.delete(row)
    db.commit()
    for target, source in changed:
        _reports_changed(target, source, db, withdrawn=True)
    return True


def forget_capture(capture_id: str, db: Session) -> None:
    """Before a capture the user deletes goes: withdraw its own reports and
    the ones it filed as a voice edit, with everything they taught, and the
    words it spelled into the dictionary. Commits."""
    from . import dictionary

    rows = (
        db.query(CaptureFeedback)
        .filter((CaptureFeedback.capture_id == capture_id) | (CaptureFeedback.filed_by == capture_id))
        .all()
    )
    changed = {(row.target, row.source) for row in rows}
    for row in rows:
        db.delete(row)
    db.commit()
    for target, source in changed:
        _reports_changed(target, source, db, withdrawn=True)
    dictionary.delete_added_by(db, capture_id)


def _reports_changed(target: str, source: str, db: Session, withdrawn: bool = False) -> None:
    # A withdrawn report may be in the cleanup adapter's training data too.
    correction_learning.request_run(retrain=withdrawn)
    if source not in CaptureFeedback.EXPLICIT_SOURCES:
        return
    known_names.invalidate()
    if target != "refined":
        return
    personal_examples.invalidate()
    try:
        writing_style.refresh_feedback(db)
    except Exception:
        logger.warning("Could not update the writing style from a correction", exc_info=True)


def list_feedback(db: Session, capture_id: str | None = None):
    query = db.query(CaptureFeedback)
    if capture_id is not None:
        query = query.filter(CaptureFeedback.capture_id == capture_id)
    return [to_response(row) for row in query.order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id).all()]
