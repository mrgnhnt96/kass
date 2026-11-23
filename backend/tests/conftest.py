import pytest

from backend.services import shared_adapters


@pytest.fixture(autouse=True)
def no_shipped_adapters(tmp_path_factory, monkeypatch):
    """Tests see the adapters the build ships only when they put some there (test_shared_adapters.py)."""
    monkeypatch.setattr(shared_adapters, "ROOT", tmp_path_factory.mktemp("shared-adapters"))
    shared_adapters.manifest.cache_clear()
    yield
    shared_adapters.manifest.cache_clear()


@pytest.fixture(autouse=True)
def cleanup_pipeline(monkeypatch):
    """The worker runs under the server's cleanup pipeline, as the app starts it."""
    monkeypatch.setenv("KASS_CLEANUP_PIPELINE", "test-pipeline")
