"""Foreground requests preempt background GPU work before dispatch."""

import asyncio

# Background work and its questions: asking whether the models are idle
# mustn't make them busy, and the Rust server's correction notes job
# generates here (/worker/learning/generate) without counting as a dictation.
# Being told which style a teach session is open in isn't a dictation either.
_BACKGROUND = (
    "/capture/learning",
    "/worker/learning/",
    "/worker/styles/state",
    "/worker/styles/teaching",
)


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
