"""The worker's model calls the Rust server makes for /models/*."""

from fastapi import FastAPI
from fastapi.testclient import TestClient

from backend.routes import models as worker_models


def _client(monkeypatch, *, loaded=(), load_error=None, unload_error=None):
    import backend.backends as backends

    calls = []

    def load_func(config):
        async def load():
            calls.append(f"load {config.model_name}")
            if load_error:
                raise RuntimeError(load_error)

        return load

    async def unload(config):
        calls.append(f"unload {config.model_name}")
        if unload_error:
            raise RuntimeError(unload_error)
        return config.model_name in loaded

    monkeypatch.setattr(backends, "check_model_loaded", lambda config: config.model_name in loaded)
    monkeypatch.setattr(backends, "get_model_load_func", load_func)
    monkeypatch.setattr(backends, "unload_model_by_config", unload)
    app = FastAPI()
    app.include_router(worker_models.router)
    return TestClient(app), calls


def test_lists_the_loaded_models(monkeypatch):
    client, _ = _client(monkeypatch, loaded=("whisper-turbo", "qwen3-4b"))
    assert client.post("/worker/models/loaded").json() == {"loaded": ["whisper-turbo", "qwen3-4b"]}


def test_loads_a_model_or_says_why_not(monkeypatch):
    client, calls = _client(monkeypatch)
    assert client.post("/worker/models/load", json={"model_name": "whisper-base"}).json() == {}
    assert calls == ["load whisper-base"]

    client, _ = _client(monkeypatch, load_error="Not enough memory")
    assert client.post("/worker/models/load", json={"model_name": "qwen3-4b"}).json() == {"error": "Not enough memory"}


def test_unloads_a_model_saying_whether_it_was_loaded(monkeypatch):
    client, _ = _client(monkeypatch, loaded=("qwen3-0.6b",))
    assert client.post("/worker/models/unload", json={"model_name": "qwen3-0.6b"}).json() == {"was_loaded": True}
    assert client.post("/worker/models/unload", json={"model_name": "whisper-small"}).json() == {"was_loaded": False}

    client, _ = _client(monkeypatch, unload_error="busy")
    assert client.post("/worker/models/unload", json={"model_name": "qwen3-0.6b"}).json() == {"error": "busy"}


def test_unknown_models_are_refused(monkeypatch):
    client, _ = _client(monkeypatch)
    for path in ("/worker/models/load", "/worker/models/unload"):
        response = client.post(path, json={"model_name": "whisper-tiny"})
        assert response.status_code == 400
        assert response.json() == {"detail": "Unknown model: whisper-tiny"}
