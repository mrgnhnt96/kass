"""The health check says how long the server has been up and what it is."""

import os
import time

from fastapi import FastAPI
from fastapi.testclient import TestClient

from backend import __version__
from backend.routes import health


def test_health_reports_uptime_and_process_details(monkeypatch):
    class Whisper:
        def is_loaded(self):
            return False

    monkeypatch.setattr(health.transcribe, "get_whisper_model", Whisper)
    app = FastAPI()
    app.include_router(health.router)
    body = TestClient(app).get("/health").json()
    assert body["version"] == __version__
    assert body["pid"] == os.getpid()
    assert health.STARTED_AT <= body["started_at"] <= time.time()
    assert body["peak_memory_mb"] > 0
