import pytest

from backend.services import dictionary, shared_adapters


@pytest.fixture(autouse=True)
def fresh_styles():
    """The dictionary snapshot is process-wide; each test reads its own database."""
    dictionary.invalidate()
    yield
    dictionary.invalidate()


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
