"""
Which Mac a request came from, for a Kass shared over the network (beta
``network``).

Each app says which Mac it runs on: an id that stays put and the Mac's name,
in the ``X-Kass-Mac`` and ``X-Kass-Mac-Name`` headers, or the ``mac`` and
``mac_name`` query parameters where a browser can't set headers (WebSocket,
EventSource). The name is percent-encoded, since Mac names aren't ASCII
("Morgan’s MacBook Pro").

``MacMiddleware`` puts the sender in ``current_mac`` for the whole request,
and ``install`` stamps it on every capture the request creates, so each way
of saving one (streamed dictation, uploads, commands) is covered.
"""

import re
from contextvars import ContextVar
from dataclasses import dataclass
from urllib.parse import parse_qs, unquote

from sqlalchemy import event
from sqlalchemy.orm import Session

_ID = re.compile(r"^[A-Za-z0-9-]{1,64}$")
_MAX_NAME = 100


@dataclass(frozen=True)
class Mac:
    id: str
    name: str | None = None


current_mac: ContextVar[Mac | None] = ContextVar("current_mac", default=None)


def mac_of(scope) -> Mac | None:
    """The Mac an HTTP or WebSocket ASGI scope says it came from, if any."""
    headers = {key.decode("latin-1").lower(): value.decode("latin-1") for key, value in scope.get("headers") or []}
    query = parse_qs((scope.get("query_string") or b"").decode("latin-1"))
    mac_id = headers.get("x-kass-mac") or next(iter(query.get("mac", [])), None)
    if not mac_id or not _ID.match(mac_id):
        return None
    raw = headers.get("x-kass-mac-name")
    # parse_qs has already decoded the query parameter.
    name = unquote(raw) if raw is not None else next(iter(query.get("mac_name", [])), None)
    name = (name or "").strip()[:_MAX_NAME] or None
    return Mac(id=mac_id, name=name)


class MacMiddleware:
    """Sets ``current_mac`` for each HTTP request and WebSocket."""

    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] not in ("http", "websocket"):
            await self.app(scope, receive, send)
            return
        token = current_mac.set(mac_of(scope))
        try:
            await self.app(scope, receive, send)
        finally:
            current_mac.reset(token)


def _stamp_captures(session: Session, _context, _instances) -> None:
    from ..database import Capture

    mac = current_mac.get()
    if mac is None:
        return
    for obj in session.new:
        if isinstance(obj, Capture) and obj.mac_id is None:
            obj.mac_id = mac.id
            obj.mac_name = mac.name


def install() -> None:
    """Stamp new captures with the Mac whose request made them. Safe to call more than once."""
    if not event.contains(Session, "before_flush", _stamp_captures):
        event.listen(Session, "before_flush", _stamp_captures)
