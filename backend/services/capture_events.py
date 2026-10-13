"""
Tells every connected app when the captures table changes.

A Mac sharing its Kass (beta ``network``) has apps on other Macs reading its
captures, and they don't hear the Tauri events the dictating Mac sends. So
the server announces each committed change itself, over ``GET
/captures/events``, and the apps refetch their captures and Insights. The
stream also says which Macs have an app listening, so the apps know when
another Mac is connected.

Changes are noticed at the session, not in each route: dictation, commands,
refinement, retention and the rest all commit through SQLAlchemy, often from
worker threads, so one hook covers them all.
"""

import asyncio
import contextlib
import json
import logging
import threading
from dataclasses import dataclass, field

from sqlalchemy import event
from sqlalchemy.orm import Session

from ..database import Capture
from ..utils.macs import Mac

logger = logging.getLogger(__name__)

# Commits pending since the last flush, per session: True once one touched captures.
_SESSION_FLAG = "kass_captures_changed"

_lock = threading.Lock()
_listeners: set["_Listener"] = set()


@dataclass(eq=False)
class _Listener:
    """One open stream: its loop, the Mac it's on, and the events it has yet to send."""

    loop: asyncio.AbstractEventLoop
    mac: Mac | None
    wake: asyncio.Event = field(default_factory=asyncio.Event)
    pending: set[str] = field(default_factory=set)

    def notify(self, kind: str) -> None:
        # Pending events collapse: one ``changed`` says it all.
        self.pending.add(kind)
        self.wake.set()


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


def publish(kind: str = "changed") -> None:
    """Tell every listener the captures (``changed``) or connected Macs (``peers``) changed. Any thread."""
    with _lock:
        listeners = list(_listeners)
    for listener in listeners:
        # A closed loop means that listener is going away.
        with contextlib.suppress(RuntimeError):
            listener.loop.call_soon_threadsafe(listener.notify, kind)


def connected_macs() -> list[dict]:
    """The Macs with an app listening, each once, in a steady order."""
    with _lock:
        macs = {listener.mac.id: listener.mac for listener in _listeners if listener.mac}
    return [{"id": mac.id, "name": mac.name} for mac in sorted(macs.values(), key=lambda mac: mac.id)]


def _event(kind: str) -> str:
    data = json.dumps(connected_macs()) if kind == "peers" else "{}"
    return f"event: {kind}\ndata: {data}\n\n"


async def subscribe(mac: Mac | None = None, keepalive_s: float = 15.0):
    """Yield Server-Sent Events: ``changed`` when captures change, ``peers`` with
    the Macs listening when that changes, and comments to keep the line open."""
    listener = _Listener(asyncio.get_running_loop(), mac)
    with _lock:
        _listeners.add(listener)
    publish("peers")
    try:
        # Says the stream is up, so an app that reconnects refetches what it missed.
        yield _event("changed")
        while True:
            try:
                await asyncio.wait_for(listener.wake.wait(), timeout=keepalive_s)
            except TimeoutError:
                yield ": keepalive\n\n"
                continue
            listener.wake.clear()
            kinds, listener.pending = listener.pending, set()
            for kind in sorted(kinds):
                yield _event(kind)
    finally:
        with _lock:
            _listeners.discard(listener)
        publish("peers")
