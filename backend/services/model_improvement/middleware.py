"""Foreground requests preempt background GPU work before dispatch."""

import asyncio

# Background work and its questions: the server's model improvement and
# learning calls must not count as a dictation and stop the run they drive,
# asking whether the models are idle mustn't make them busy, and the Rust
# server's correction notes job generates here (/worker/learning/generate)
# without counting as a dictation. Sizing the styles' prompt caches for the
# styles listing isn't a dictation either.
_BACKGROUND = ("/worker/learning/", "/worker/cleanup/cache-bytes")


class ForegroundPriorityMiddleware:
    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        foreground = False
        if scope["type"] == "http" and scope.get("method") in ("POST", "PUT", "PATCH", "DELETE"):
            path = scope.get("path", "")
            if not path.startswith(_BACKGROUND):
                from .manager import begin_foreground

                await asyncio.to_thread(begin_foreground)
                foreground = True
        try:
            await self.app(scope, receive, send)
        finally:
            if foreground:
                from .manager import end_foreground

                end_foreground()
