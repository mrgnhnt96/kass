"""The worker's CORS origins: the app's own, plus any configured remote ones."""

from backend.utils.origins import allowed_origins


def test_configured_remote_origin(monkeypatch):
    monkeypatch.setenv("KASS_CORS_ORIGINS", " https://kass.example, ")
    assert "https://kass.example" in allowed_origins()
    assert "tauri://localhost" in allowed_origins()
