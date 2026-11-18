"""The worker's model calls for a dictation streamed through the Rust server."""

import asyncio
import json
import struct
from types import SimpleNamespace

import numpy as np
import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient

from backend.backends.qwen_llm_backend import generation_hint, generation_listener, generation_stop, prompt_cache_key
from backend.backends.word_timing import Word
from backend.routes import stream_models
from backend.services import commands, llm as llm_service
from backend.services.model_improvement import manager


@pytest.fixture
def worker():
    app = FastAPI()
    app.include_router(stream_models.router)
    return TestClient(app)


def recognize_body(head: dict, samples: list[int]) -> bytes:
    encoded = json.dumps(head).encode()
    return struct.pack("<I", len(encoded)) + encoded + np.asarray(samples, dtype="<i2").tobytes()


class Alignment:
    def words(self):
        return [Word("Hello", 0.1, 0.4), Word("there.", 0.5, 0.9)]


class Whisper:
    def __init__(self):
        self.calls = []

    async def transcribe_array(self, samples, rate, language, model, **options):
        self.calls.append((samples.tolist(), rate, language, model, options))
        if options["alignments"] is not None:
            options["alignments"].append(Alignment())
        return " Hello there. "

    async def transcribe(self, path, language, model, **options):
        self.calls.append((path, language, model, options))
        return " The whole take. "


def test_recognize_reads_the_head_and_pcm_and_keeps_the_alignment_for_later(worker, monkeypatch):
    whisper = Whisper()
    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: whisper)
    head = dict(rate=48000, language="en", stt_model="turbo", previous_text="So", vocabulary=["Kass"], alignment=True)

    answer = worker.post("/worker/recognize", content=recognize_body(head, [1, -2, 3])).json()

    assert answer["text"] == "Hello there."
    samples, rate, language, model, options = whisper.calls[0]
    assert (samples, rate, language, model) == ([1, -2, 3], 48000, "en", "turbo")
    assert options["previous_text"] == "So"
    assert options["vocabulary"] == ("Kass",)
    assert options["check_speech"] is False
    words = worker.post(f"/worker/alignments/{answer['alignment']}/words").json()["words"]
    assert words == [{"text": "Hello", "start": 0.1, "end": 0.4}, {"text": "there.", "start": 0.5, "end": 0.9}]
    # Asked for once: the words went with the answer.
    assert worker.post(f"/worker/alignments/{answer['alignment']}/words").status_code == 404


def test_recognize_without_alignment_or_audio(worker, monkeypatch):
    whisper = Whisper()
    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: whisper)

    answer = worker.post("/worker/recognize", content=recognize_body({"rate": 16000}, [5])).json()
    assert answer["alignment"] is None
    assert whisper.calls[0][4]["alignments"] is None

    assert worker.post("/worker/recognize", content=recognize_body({"rate": 16000}, [])).json()["text"] == ""
    assert len(whisper.calls) == 1


@pytest.mark.parametrize(
    "body",
    [b"", struct.pack("<I", 99) + b"{}", struct.pack("<I", 2) + b"{}" + b"\x01", recognize_body({"rate": 0}, [1])],
)
def test_recognize_refuses_a_broken_body(worker, body):
    assert worker.post("/worker/recognize", content=body).status_code == 422


def test_recognize_file_is_the_whole_file_fallback(worker, monkeypatch):
    whisper = Whisper()
    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: whisper)

    answer = worker.post("/worker/recognize/file", json={"path": "/tmp/take.wav", "previous_text": "and"}).json()

    assert answer["text"] == "The whole take."
    path, _, _, options = whisper.calls[0]
    assert path == "/tmp/take.wav"
    assert options == {"previous_text": "and", "check_speech": False, "vocabulary": ()}
    assert "duration" not in answer


def test_recognize_file_decodes_an_upload_and_answers_its_length(worker, monkeypatch, tmp_path):
    from backend.utils import audio as audio_utils

    whisper = Whisper()
    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: whisper)
    monkeypatch.setattr(audio_utils, "load_audio", lambda path: (np.zeros(36000, dtype=np.float32), 24000))
    written = []

    def save(audio, path, rate):
        written.append((len(audio), rate))
        open(path, "wb").close()

    monkeypatch.setattr(audio_utils, "save_audio", save)
    upload = tmp_path / "take.webm"
    upload.write_bytes(b"webm")

    body = {"path": str(upload), "stt_model": "base", "check_speech": True, "decode": True}
    answer = worker.post("/worker/recognize/file", json=body).json()

    assert (answer["text"], answer["duration"]) == ("The whole take.", 1.5)
    path, _, model, options = whisper.calls[0]
    # Whisper reads the WAV written from what librosa decoded, then it's gone.
    assert (path, model, written) == (f"{upload}.stt.wav", "base", [(36000, 24000)])
    assert options["check_speech"] is True
    assert not (tmp_path / "take.webm.stt.wav").exists()


def test_recognize_file_reads_a_wav_upload_as_it_is(worker, monkeypatch):
    from backend.utils import audio as audio_utils

    whisper = Whisper()
    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: whisper)
    monkeypatch.setattr(audio_utils, "load_audio", lambda path: (np.zeros(12000, dtype=np.float32), 24000))

    answer = worker.post("/worker/recognize/file", json={"path": "/tmp/up.WAV", "decode": True}).json()

    assert answer["duration"] == 0.5
    assert whisper.calls[0][0] == "/tmp/up.WAV"


def test_recognize_file_answers_what_failed(worker, monkeypatch):
    from backend.utils import audio as audio_utils

    def broken(path):
        raise RuntimeError("unsupported file format")

    monkeypatch.setattr(stream_models, "get_whisper_model", lambda: Whisper())
    monkeypatch.setattr(audio_utils, "load_audio", broken)

    response = worker.post("/worker/recognize/file", json={"path": "/tmp/x.ogg", "decode": True})

    assert (response.status_code, response.json()) == (500, {"detail": "unsupported file format"})


class Qwen:
    supports_adapters = True

    def __init__(self, tokens=("Hello", " there", "."), fail_with_adapter=False):
        self.tokens = tokens
        self.fail_with_adapter = fail_with_adapter
        self.calls = []
        self.started = asyncio.Event()
        self.release = None

    async def generate(self, **arguments):
        self.calls.append(
            dict(arguments, hint=generation_hint.get(), cache_key=prompt_cache_key.get(), stop=generation_stop.get())
        )
        if self.fail_with_adapter and arguments.get("adapter_path"):
            raise RuntimeError("adapter broke")
        listener, stop = generation_listener.get(), generation_stop.get()
        text = ""
        for token in self.tokens:
            if stop is not None and stop.is_set():
                break
            text += token
            if listener is not None:
                listener(text)
            await asyncio.sleep(0.01)
        return text

    async def prepare(self, model_size, adapter_path):
        self.calls.append(dict(prepare=model_size, adapter_path=adapter_path))


GENERATE = dict(
    id="phrase-1",
    prompt="hello there",
    system="Clean this up.",
    examples=[["um hi", "Hi."]],
    model_size="1.7B",
    cache_key="cleanup:Chat",
    hint="",
    flags={"style": "Chat"},
)


def streamed(worker, body) -> list[dict]:
    with worker.stream("POST", "/worker/cleanup/generate", json=body) as response:
        assert response.headers["content-type"] == "application/x-ndjson"
        return [json.loads(line) for line in response.iter_lines() if line]


def test_cleanup_streams_partials_then_the_text(worker, monkeypatch):
    qwen = Qwen()
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)
    monkeypatch.setattr(manager, "active_adapter", lambda size, flags: f"adapters/{size}/{flags['style']}")

    lines = streamed(worker, GENERATE)

    assert [line["partial"] for line in lines[:-1]] == ["Hello", "Hello there", "Hello there."]
    assert lines[-1]["text"] == "Hello there."
    assert lines[-1]["model"] == "1.7B"
    assert lines[-1]["stopped"] is False
    call = qwen.calls[0]
    assert call["prompt"] == "hello there"
    assert call["system"] == "Clean this up."
    assert call["examples"] == [("um hi", "Hi.")]
    assert (call["max_tokens"], call["temperature"]) == (2048, 0.2)
    assert call["adapter_path"] == "adapters/1.7B/Chat"
    assert (call["hint"], call["cache_key"]) == ("", "cleanup:Chat")


def test_cleanup_without_flags_runs_the_base_model(worker, monkeypatch):
    qwen = Qwen()
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)

    streamed(worker, dict(GENERATE, flags=None, hint=None))

    assert "adapter_path" not in qwen.calls[0]
    assert qwen.calls[0]["hint"] is None


def test_a_failing_adapter_is_quarantined_and_the_base_model_cleans(worker, monkeypatch):
    qwen = Qwen(fail_with_adapter=True)
    quarantined = []
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)
    monkeypatch.setattr(manager, "active_adapter", lambda size, flags: "adapters/personal")
    monkeypatch.setattr(manager, "quarantine_adapter", lambda message, path: quarantined.append(path))

    lines = streamed(worker, GENERATE)

    assert lines[-1]["text"] == "Hello there."
    assert quarantined == ["adapters/personal"]
    assert "adapter_path" not in qwen.calls[1]


def test_a_failed_cleanup_ends_with_its_error(worker, monkeypatch):
    class Broken(Qwen):
        async def generate(self, **arguments):
            raise RuntimeError("out of memory")

    monkeypatch.setattr(llm_service, "get_llm_model", lambda: Broken())

    assert streamed(worker, dict(GENERATE, flags=None)) == [{"error": "out of memory"}]


@pytest.mark.asyncio
async def test_stop_ends_a_cleanup_before_its_next_token(monkeypatch):
    qwen = Qwen(tokens=tuple(f" {n}" for n in range(200)))
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)
    response = await stream_models.cleanup_generate(stream_models.Generate(**dict(GENERATE, flags=None)))
    lines = response.body_iterator

    assert "partial" in json.loads(await anext(lines))
    assert await stream_models.cleanup_stop("phrase-1") == {"stopped": True}
    rest = [json.loads(line) async for line in lines]

    assert rest[-1]["stopped"] is True
    assert len(rest[-1]["text"].split()) < 200
    # Gone once it ended.
    assert await stream_models.cleanup_stop("phrase-1") == {"stopped": False}


@pytest.mark.asyncio
async def test_a_dropped_response_stops_its_cleanup(monkeypatch):
    qwen = Qwen(tokens=tuple(f" {n}" for n in range(200)))
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)
    response = await stream_models.cleanup_generate(stream_models.Generate(**dict(GENERATE, flags=None)))

    await anext(response.body_iterator)
    stop = qwen.calls[0]["stop"]
    await response.body_iterator.aclose()

    assert stop.is_set()


def test_cleanup_prefill_and_load(worker, monkeypatch):
    qwen = Qwen()
    monkeypatch.setattr(llm_service, "get_llm_model", lambda: qwen)
    monkeypatch.setattr(manager, "active_adapter", lambda size, flags: None)
    body = {key: GENERATE[key] for key in ("system", "examples", "model_size", "cache_key", "flags")}

    assert "seconds" in worker.post("/worker/cleanup/prefill", json=body).json()
    assert worker.post("/worker/cleanup/load", json={"model_size": "1.7B", "flags": None}).json() == {"ok": True}

    prefill, load = qwen.calls
    assert (prefill["prompt"], prefill["max_tokens"], prefill["temperature"]) == ("", 1, 0)
    assert prefill["cache_key"] == "cleanup:Chat"
    assert "adapter_path" not in prefill
    assert load == {"prepare": "1.7B", "adapter_path": None}


def test_commands_prefill_and_rewrite(worker, monkeypatch):
    seen = []

    async def prefill(selection, model_size):
        seen.append(("prefill", selection, model_size))

    async def rewrite(selection, instruction, model_size):
        seen.append(("rewrite", selection, instruction, model_size))
        return "Dear team,", model_size

    monkeypatch.setattr(commands, "ensure_model_ready", lambda size: None)
    monkeypatch.setattr(commands, "prefill", prefill)
    monkeypatch.setattr(commands, "rewrite", rewrite)

    assert worker.post("/worker/command/prefill", json={"selection": "hey all", "model_size": "4B"}).json() == {
        "ok": True
    }
    body = {"selection": "hey all", "instruction": "make it formal", "model_size": "4B"}
    assert worker.post("/worker/command/rewrite", json=body).json() == {"text": "Dear team,", "model": "4B"}
    assert seen == [("prefill", "hey all", "4B"), ("rewrite", "hey all", "make it formal", "4B")]


def test_a_command_that_cant_run_says_why(worker, monkeypatch):
    def missing(size):
        raise ValueError("Download Qwen3 4B in Models to use Command Mode")

    monkeypatch.setattr(commands, "ensure_model_ready", missing)
    body = {"selection": "hey", "instruction": "formal", "model_size": "4B"}

    answer = worker.post("/worker/command/rewrite", json=body)

    assert answer.status_code == 422
    assert answer.json() == {"detail": "Download Qwen3 4B in Models to use Command Mode"}


def test_keep_resident_and_foreground(worker, monkeypatch):
    calls = []

    async def keep_resident(reason, llm, stt):
        calls.append((reason, llm, stt))

    monkeypatch.setattr(stream_models, "keep_resident", keep_resident)
    monkeypatch.setattr(manager, "_foreground_count", 0)

    worker.post("/worker/keep-resident", json={"reason": "dictation started", "stt": False})
    assert calls == [("dictation started", True, False)]

    worker.post("/worker/foreground/begin")
    assert manager._foreground_count == 1
    worker.post("/worker/foreground/end")
    assert manager._foreground_count == 0


def test_speech_model_is_the_trained_one_while_active(worker, monkeypatch):
    monkeypatch.setattr(manager, "_active", {})
    assert worker.post("/worker/speech-model", json={"configured": "turbo"}).json() == {"model": "turbo"}

    monkeypatch.setattr(manager, "_active", {"speech": {"configured": "turbo", "model": "/models/trained"}})
    assert worker.post("/worker/speech-model", json={"configured": "turbo"}).json() == {"model": "/models/trained"}
    assert worker.post("/worker/speech-model", json={"configured": "small"}).json() == {"model": "small"}


def test_alignments_are_kept_for_a_while(monkeypatch):
    monkeypatch.setattr(stream_models, "_alignments", type(stream_models._alignments)())
    keys = [stream_models._keep_alignment(SimpleNamespace()) for _ in range(stream_models.MAX_ALIGNMENTS + 1)]
    assert keys[0] not in stream_models._alignments
    assert keys[-1] in stream_models._alignments
