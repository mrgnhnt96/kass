"""Origins allowed to call the worker from a browser (HTTP CORS)."""

import os


def allowed_origins() -> list[str]:
    """Return explicit frontend origins, including configured remote deployments."""
    defaults = [
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "http://localhost:17493",
        "http://127.0.0.1:17493",
        "tauri://localhost",
        "https://tauri.localhost",
        "http://tauri.localhost",
    ]
    configured = os.environ.get("KASS_CORS_ORIGINS", "")
    return defaults + [origin.strip() for origin in configured.split(",") if origin.strip()]
