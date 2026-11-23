"""Startup prepares saved capture models before the server becomes ready."""

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from backend.routes.sync import Startup
from backend.services import model_startup, shared_adapters

DEFAULT = {"flags": {"style": "personal"}, "model_size": "1.7B", "system": "Default system", "cache_key": "personal"}
CHAT = {"flags": {"style": "chat"}, "model_size": "1.7B", "system": "Chat system", "examples": [["hi", "Hi."]]}


@pytest.fixture
def startup(monkeypatch):
    saved = Startup(stt_model="small", llm_model="1.7B", auto_refine=True, language="en", prompts=[DEFAULT, CHAT])
    stt = SimpleNamespace(
        _is_model_cached=MagicMock(return_value=True),
        load_model=AsyncMock(),
        is_loaded=MagicMock(return_value=True),
        transcribe_array=AsyncMock(return_value=""),
    )
    llm = SimpleNamespace(
        _is_model_cached=MagicMock(return_value=True),
        load_model=AsyncMock(),
        is_loaded=MagicMock(return_value=True),
        generate=AsyncMock(return_value="Okay."),
    )
    monkeypatch.setattr(model_startup.transcribe, "get_whisper_model", lambda: stt)
    monkeypatch.setattr(model_startup.llm, "get_llm_model", lambda: llm)
    monkeypatch.setattr(model_startup, "keep_weights_resident", MagicMock())
    monkeypatch.setattr(shared_adapters, "_enabled", None)
    monkeypatch.setattr(model_startup, "_style_warmup", None)
    yield saved, stt, llm
    if model_startup._style_warmup is not None:
        model_startup._style_warmup.cancel()


@pytest.mark.asyncio
async def test_keeps_weights_resident_before_loading(startup):
    # Wired before the loads so the weights never sit pageable; macOS pages
    # idle weights out and the next dictation waits seconds to fault them in.
    saved, stt, _ = startup
    stt.load_model.side_effect = lambda size: model_startup.keep_weights_resident.assert_called_once()
    await model_startup.load_startup_models(saved)
    stt.load_model.assert_awaited_once()


@pytest.mark.asyncio
async def test_wiring_failure_does_not_block_startup(startup, caplog):
    saved, stt, _ = startup
    model_startup.keep_weights_resident.side_effect = RuntimeError("wired limit too large")
    await model_startup.load_startup_models(saved)
    assert "Could not keep model weights resident" in caplog.text
    stt.load_model.assert_awaited_once_with("small")


@pytest.mark.asyncio
async def test_loads_saved_models_in_order(startup):
    saved, stt, llm = startup
    order = []
    stt.load_model.side_effect = lambda size: order.append(("stt", size))
    llm.load_model.side_effect = lambda size: order.append(("llm", size))
    await model_startup.load_startup_models(saved)
    assert order == [("stt", "small"), ("llm", "1.7B")]


@pytest.mark.asyncio
async def test_missing_models_are_not_downloaded(startup):
    saved, stt, llm = startup
    stt._is_model_cached.return_value = False
    llm._is_model_cached.return_value = False
    await model_startup.load_startup_models(saved)
    stt.load_model.assert_not_awaited()
    llm.load_model.assert_not_awaited()


@pytest.mark.asyncio
async def test_disabled_refinement_only_loads_whisper(startup):
    saved, stt, llm = startup
    saved.auto_refine = False
    await model_startup.load_startup_models(saved)
    stt.load_model.assert_awaited_once_with("small")
    llm._is_model_cached.assert_not_called()
    llm.load_model.assert_not_awaited()


@pytest.mark.asyncio
async def test_load_failure_does_not_block_other_models(startup, caplog):
    saved, stt, llm = startup
    stt.load_model.side_effect = RuntimeError("Model cannot load")
    await model_startup.load_startup_models(saved)
    llm.load_model.assert_awaited_once_with("1.7B")
    assert "Could not load startup Whisper model small" in caplog.text


@pytest.mark.asyncio
async def test_warms_the_default_styles_prompt_as_dictation_sends_it(startup):
    saved, _, llm = startup
    await model_startup.load_startup_models(saved)
    first = llm.generate.await_args_list[0].kwargs
    assert first["prompt"] == "okay"
    assert first["system"] == "Default system"
    assert first["model_size"] == "1.7B"
    assert first["max_tokens"] == model_startup.WARM_TOKENS


@pytest.mark.asyncio
async def test_caches_the_other_styles_prompts_after(startup):
    saved, _, llm = startup
    await model_startup.load_startup_models(saved)
    await model_startup._style_warmup
    assert llm.generate.await_count == 2
    prefill = llm.generate.await_args_list[1].kwargs
    assert (prefill["prompt"], prefill["max_tokens"]) == ("", 1)
    assert (prefill["system"], prefill["examples"]) == ("Chat system", [("hi", "Hi.")])


@pytest.mark.asyncio
async def test_no_prompts_warms_nothing(startup):
    saved, _, llm = startup
    saved.prompts = []
    await model_startup.load_startup_models(saved)
    llm.load_model.assert_awaited_once_with("1.7B")
    llm.generate.assert_not_awaited()


@pytest.mark.asyncio
async def test_takes_the_shared_adapters_setting_before_loading(startup):
    saved, stt, _ = startup
    saved.shared_adapters = False
    stt.load_model.side_effect = lambda size: assert_off()

    def assert_off():
        assert not shared_adapters.enabled()

    await model_startup.load_startup_models(saved)
    stt.load_model.assert_awaited_once()


@pytest.mark.asyncio
async def test_warm_up_failure_does_not_block_startup(startup, caplog):
    saved, _, llm = startup
    llm.generate.side_effect = RuntimeError("generate failed")
    await model_startup.load_startup_models(saved)
    assert "Could not warm the refinement prompt" in caplog.text


@pytest.mark.asyncio
async def test_unloaded_refinement_model_is_not_warmed(startup):
    saved, _, llm = startup
    llm.is_loaded.return_value = False
    await model_startup.load_startup_models(saved)
    llm.generate.assert_not_awaited()


@pytest.mark.asyncio
async def test_runs_whisper_once_so_the_first_dictation_is_warm(startup, tmp_path, monkeypatch):
    from backend import config

    saved, stt, _ = startup
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    await model_startup.load_startup_models(saved)
    stt.transcribe_array.assert_awaited_once()
    samples, rate = stt.transcribe_array.await_args.args[:2]
    # One second at a Mac microphone's rate, so resampling is warmed too.
    assert rate == 48000
    assert len(samples) == 48000
    # Warmed as the first phrase of a dictation (no earlier text), the call
    # streaming makes, so its one-time setup is paid here and not after release.
    # Whisper runs even if the newest recording is silent: warming it is the point.
    assert stt.transcribe_array.await_args.kwargs == {
        "language": "en",
        "model_size": "small",
        "previous_text": "",
        "check_speech": False,
    }


@pytest.mark.asyncio
async def test_auto_language_warms_with_detection(startup):
    saved, stt, _ = startup
    saved.language = "auto"
    await model_startup.load_startup_models(saved)
    assert stt.transcribe_array.await_args.kwargs["language"] is None


@pytest.mark.asyncio
async def test_whisper_warm_up_failure_does_not_block_startup(startup, caplog):
    saved, stt, llm = startup
    stt.transcribe_array.side_effect = RuntimeError("no GPU")
    await model_startup.load_startup_models(saved)
    assert "Could not warm Whisper" in caplog.text
    llm.generate.assert_awaited()


@pytest.mark.asyncio
async def test_unloaded_whisper_is_not_warmed(startup):
    saved, stt, _ = startup
    stt.is_loaded.return_value = False
    await model_startup.load_startup_models(saved)
    stt.transcribe_array.assert_not_awaited()


def test_the_app_sends_its_settings_and_prompts(monkeypatch):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes import sync

    received = []

    async def load(startup):
        received.append(startup)

    monkeypatch.setattr(model_startup, "load_startup_models", load)
    app = FastAPI()
    app.include_router(sync.router)
    body = {
        "stt_model": "turbo",
        "llm_model": "4B",
        "auto_refine": True,
        "language": "auto",
        "shared_adapters": False,
        "prompts": [DEFAULT],
    }
    assert TestClient(app).post("/worker/models/startup", json=body).status_code == 200
    [startup] = received
    assert (startup.stt_model, startup.shared_adapters) == ("turbo", False)
    assert startup.prompts[0].system == "Default system"
    assert TestClient(app).post("/worker/models/startup", json={}).status_code == 422


def _write_capture(directory, name, seconds, rate=48000, value=1000):
    import numpy as np
    import soundfile as sf

    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    sf.write(path, np.full(int(seconds * rate), value, dtype=np.int16), rate, subtype="PCM_16")
    return path


@pytest.mark.asyncio
async def test_warms_whisper_with_the_most_recent_recording(startup, tmp_path, monkeypatch):
    import os

    from backend import config

    saved, stt, _ = startup
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    older = _write_capture(config.get_captures_dir(), "older.wav", 2, value=500)
    os.utime(older, (1, 1))
    _write_capture(config.get_captures_dir(), "newest.wav", 9, rate=44100)
    await model_startup.load_startup_models(saved)
    samples, rate = stt.transcribe_array.await_args.args[:2]
    # The newest recording, trimmed so loading stays short.
    assert rate == 44100
    assert len(samples) == model_startup.WARM_SECONDS * 44100
    assert int(samples[0]) == 1000


@pytest.mark.asyncio
async def test_unreadable_recordings_fall_back_to_noise(startup, tmp_path, monkeypatch):
    from backend import config

    saved, stt, _ = startup
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    config.get_captures_dir().mkdir(parents=True, exist_ok=True)
    (config.get_captures_dir() / "broken.wav").write_bytes(b"not audio")
    await model_startup.load_startup_models(saved)
    samples, rate = stt.transcribe_array.await_args.args[:2]
    assert rate == 48000
    assert len(samples) == 48000
