"""
Tells the Rust server when what the worker learned changes.

Learning kept in files (corrections, the writing style, trained models)
calls :func:`publish` with ``learned`` when it saves. The Rust server follows
``GET /worker/captures/events`` and passes each event on to every app.
"""

import asyncio
import contextlib
import threading
from dataclasses import dataclass, field

_lock = threading.Lock()
_listeners: set["_Listener"] = set()


@dataclass(eq=False)
class _Listener:
    """One open stream: its loop and the events it has yet to send."""

    loop: asyncio.AbstractEventLoop
    wake: asyncio.Event = field(default_factory=asyncio.Event)
    pending: set[str] = field(default_factory=set)

    def notify(self, kind: str) -> None:
        # Pending events collapse: one ``learned`` says it all.
        self.pending.add(kind)
        self.wake.set()


def publish(kind: str = "changed") -> None:
    """Tell every listener the captures (``changed``) or anything else they
    show (``learned``) changed. Any thread."""
    with _lock:
        listeners = list(_listeners)
    for listener in listeners:
        # A closed loop means that listener is going away.
        with contextlib.suppress(RuntimeError):
            listener.loop.call_soon_threadsafe(listener.notify, kind)


def _event(kind: str) -> str:
    return f"event: {kind}\ndata: {{}}\n\n"


async def subscribe(keepalive_s: float = 15.0):
    """Yield Server-Sent Events: ``changed`` and ``learned`` as they're
    published, and comments to keep the line open."""
    listener = _Listener(asyncio.get_running_loop())
    with _lock:
        _listeners.add(listener)
    try:
        # Says the stream is up, so the server refetches what it missed.
        yield _event("changed")
        yield _event("learned")
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
