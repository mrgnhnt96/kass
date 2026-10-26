"""
What a stand-in changed, waiting to go to the shared Kass (``SyncChange``).

Recorded on every flush, so each way of changing a row (dictation, reports,
the dictionary, styles, settings) is covered without each knowing about sync.
Copying from the shared Kass isn't a change of this Mac's: it runs inside
:func:`not_recorded`.
"""

from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar
from datetime import datetime

from sqlalchemy import event, inspect
from sqlalchemy.dialects.sqlite import insert
from sqlalchemy.orm import Session

from ...database.models import SyncChange
from . import standing_in
from .rows import OWN_COLUMNS, SINGLETONS, kind_of, ref_id

_recording: ContextVar[bool] = ContextVar("sync_recording", default=True)


@contextmanager
def not_recorded() -> Iterator[None]:
    token = _recording.set(False)
    try:
        yield
    finally:
        _recording.reset(token)


def _edited(row, kind: str) -> bool:
    """Whether a column the shared Kass keeps changed."""
    own = OWN_COLUMNS.get(kind, frozenset())
    state = inspect(row)
    return any(
        attribute.key not in own and attribute.history.has_changes()
        for attribute in state.attrs
        if attribute.key in state.mapper.columns
    )


def _record(session: Session, _context) -> None:
    if not _recording.get() or not standing_in():
        return
    now = datetime.utcnow()
    changes = {}
    for row in session.new:
        kind = kind_of(row)
        if kind is not None and kind not in SINGLETONS:
            changes[(kind, ref_id(row))] = False
    for row in session.dirty:
        kind = kind_of(row)
        if kind is not None and _edited(row, kind):
            changes[(kind, ref_id(row))] = False
    for row in session.deleted:
        kind = kind_of(row)
        if kind is not None:
            changes[(kind, ref_id(row))] = True
    if not changes:
        return
    statement = insert(SyncChange).values(
        [
            {"kind": kind, "ref_id": ref, "deleted": deleted, "changed_at": now}
            for (kind, ref), deleted in changes.items()
        ]
    )
    session.connection().execute(
        statement.on_conflict_do_update(
            index_elements=["kind", "ref_id"],
            set_={"deleted": statement.excluded.deleted, "changed_at": statement.excluded.changed_at},
        )
    )


def install() -> None:
    """Record changes from here on. Safe to call more than once."""
    if not event.contains(Session, "after_flush", _record):
        event.listen(Session, "after_flush", _record)


def pending(db: Session) -> list[SyncChange]:
    return db.query(SyncChange).order_by(SyncChange.changed_at).all()


def count(db: Session) -> int:
    return db.query(SyncChange).count()


def sent(db: Session, changes: list[tuple[str, str, datetime]]) -> None:
    """Forget changes the shared Kass has, by (kind, ref id, changed at), unless
    the row changed again since they were read."""
    for kind, ref, changed_at in changes:
        db.query(SyncChange).filter(
            SyncChange.kind == kind,
            SyncChange.ref_id == ref,
            SyncChange.changed_at == changed_at,
        ).delete()
    db.commit()
