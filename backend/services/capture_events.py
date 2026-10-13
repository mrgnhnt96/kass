"""
Tells every connected app when the captures table changes.

A Mac sharing its Kass (beta ``network``) has apps on other Macs reading its
captures, and they don't hear the Tauri events the dictating Mac sends. So
the server announces each committed change itself, over ``GET
/captures/events``, and the apps refetch their captures and Insights.

Changes are noticed at the session, not in each route: dictation, commands,
refinement, retention and the rest all commit through SQLAlchemy, often from
worker threads, so one hook covers them all.
"""

import asyncio
import contextlib
import logging
import threading

from sqlalchemy import event
from sqlalchemy.orm import Session

from ..database import Capture

logger = logging.getLogger(__name__)

# Commits pending since the last flush, per session: True once one touched captures.
_SESSION_FLAG = "kass_captures_changed"

_lock = threading.Lock()
_listeners: set[tuple[asyncio.AbstractEventLoop, asyncio.Queue]] = set()


def _touches_captures(session: Session) -> bool:
    return any(isinstance(obj, Capture) for obj in (*session.new, *session.dirty, *session.deleted))


def _before_flush(session: Session, _context, _instances) -> None:
    if _touches_captures(session):
        session.info[_SESSION_FLAG] = True


def _do_orm_execute(state) -> None:
    # Bulk updates and deletes (history retention) skip the flush.
    if (state.is_update or state.is_delete) and any(
        mapper.class_ is Capture for mapper in state.all_mappers
    ):
        state.session.info[_SESSION_FLAG] = True


def _after_commit(session: Session) -> None:
    if session.info.pop(_SESSION_FLAG, False):
        publish()


def _after_rollback(session: Session) -> None:
    session.info.pop(_SESSION_FLAG, None)


def install() -> None:
    """Watch every session for capture changes. Safe to call more than once."""
    for name, fn in (
        ("before_flush", _before_flush),
        ("do_orm_execute", _do_orm_execute),
        ("after_commit", _after_commit),
        ("after_rollback", _after_rollback),
    ):
        if not event.contains(Session, name, fn):
            event.listen(Session, name, fn)


def publish() -> None:
    """Tell every listener the captures changed. Callable from any thread."""
    with _lock:
        listeners = list(_listeners)
    for loop, queue in listeners:
        # A closed loop means that listener is going away.
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(_offer, queue)


def _offer(queue: asyncio.Queue) -> None:
    # One pending notice says it all, so a full queue drops the rest.
    with contextlib.suppress(asyncio.QueueFull):
        queue.put_nowait(None)


async def subscribe(keepalive_s: float = 15.0):
    """Yield Server-Sent Events: ``changed`` per change, and comments to keep the line open."""
    entry = (asyncio.get_running_loop(), asyncio.Queue(maxsize=1))
    with _lock:
        _listeners.add(entry)
    try:
        # Says the stream is up, so an app that reconnects refetches what it missed.
        yield "event: changed\ndata: {}\n\n"
        while True:
            try:
                await asyncio.wait_for(entry[1].get(), timeout=keepalive_s)
            except TimeoutError:
                yield ": keepalive\n\n"
                continue
            yield "event: changed\ndata: {}\n\n"
    finally:
        with _lock:
            _listeners.discard(entry)
