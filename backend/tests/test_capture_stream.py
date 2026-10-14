"""Streaming lifecycle and boundary regressions without model downloads."""

import asyncio
import struct
from unittest.mock import AsyncMock

import numpy as np
import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session

from backend import config
from backend.database.models import Base, Capture
from backend.models import CaptureSettingsResponse
from backend.services import capture_stream


class HeardWhenLoud:
    """Stands in for the voice detector: any non-silent audio is a voice.

    The frames these tests send are flat tones, which the real detector
    rightly ignores (it has its own tests).
    """

    def __init__(self, rate, ignore_before=0):
        self.ignore_before = ignore_before
        self.loud = []
        self.samples = 0

    def feed(self, pcm):
        if len(pcm) and abs(int(pcm.astype("int32").max())) >= 250:
            self.loud.append((self.samples, self.samples + len(pcm)))
        self.samples += len(pcm)

    def heard(self, start, end):
        return any(low < end and start < high for low, high in self.loud)

    def quiet(self):
        return self.samples - (self.loud[-1][1] if self.loud else 0)

    def first_voice(self):
        return self.loud[0][0] if self.loud else None

    def ignore(self, start, end):
        self.loud = [(low, high) for low, high in self.loud if not (low < end and start < high)]


def make_session(tmp_path, monkeypatch, **settings):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(capture_stream, "SpeechDetector", HeardWhenLoud)
    events = []

    async def send(event):
        events.append(event)

    session = capture_stream.StreamingCapture(
        dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le"),
        CaptureSettingsResponse(auto_refine=False, **settings),
        send,
    )
    return session, events


def test_the_start_cue_span_reaches_the_voice_detector(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(capture_stream, "SpeechDetector", HeardWhenLoud)
    start = dict(type="start", protocol_version=1, sample_rate=48000, channels=1, encoding="pcm_s16le")
    session = capture_stream.StreamingCapture({**start, "start_cue_ms": 300}, CaptureSettingsResponse(), None)
    assert session.speech.ignore_before == 14400
    assert capture_stream.StreamingCapture(start, CaptureSettingsResponse(), None).speech.ignore_before == 0
    for bad in (-1, 5000, "300", True, 0.3):
        with pytest.raises(ValueError, match="start_cue_ms"):
            capture_stream.StreamingCapture({**start, "start_cue_ms": bad}, CaptureSettingsResponse(), None)


def append(session, seconds, amplitude=1000):
    pcm = np.full(round(seconds * session.rate), amplitude, dtype="<i2").tobytes()
    for offset in range(0, len(pcm), 32000):
        session.append(struct.pack("<II", session.sequence, session.samples) + pcm[offset : offset + 32000])


@pytest.mark.asyncio
async def test_unpaused_speech_is_recognized_once_at_finish(tmp_path, monkeypatch):
    session, events = make_session(tmp_path, monkeypatch)
    session.recognize = AsyncMock(return_value="hello world")
    worker = asyncio.create_task(session.run())
    append(session, 2)
    await asyncio.sleep(0)
    # No client shows provisional text, and a preview still running at
    # release would delay the final result, so none is made.
    assert not events
    session.recognize.assert_not_awaited()
    append(session, 0.3)
    session.finish()
    await worker
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    with Session(engine) as db:
        result = session.persist(db)
        assert result.transcript_raw == "hello world"
        assert result.duration_ms == 2300
        assert db.query(Capture).count() == 1
    # The whole recording, recognized exactly once.
    session.recognize.assert_awaited_once()
    assert len(session.recognize.await_args.args[0]) == 2.3 * session.rate * 2
    assert events[-1]["accepted_text"] == "hello world"
    session.close()
    assert session.path.exists()


@pytest.mark.asyncio
async def test_no_speculative_work_while_speaking(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    stt = type("STT", (), {"transcribe_array": AsyncMock(return_value="hello world")})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    refine = AsyncMock(return_value=("Hello world.", "0.6B"))
    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    worker = asyncio.create_task(session.run())
    for _ in range(7):
        append(session, 1)
        await asyncio.sleep(0)
    stt.transcribe_array.assert_not_awaited()
    refine.assert_not_awaited()
    session.finish()
    await worker
    stt.transcribe_array.assert_awaited_once()
    refine.assert_awaited_once()
    assert session.refined == "Hello world."
    session.close()


@pytest.mark.asyncio
async def test_release_during_a_phrase_only_adds_the_remaining_audio(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    started = asyncio.Event()
    release = asyncio.Event()
    heard = []

    async def recognize(pcm):
        heard.append(len(pcm) // 2)
        if len(heard) == 1:
            started.set()
            await release.wait()
        return f"phrase {len(heard)}"

    session.recognize = recognize
    worker = asyncio.create_task(session.run())
    append(session, 2)
    append(session, 1, amplitude=0)
    await started.wait()
    # The key comes up while the paused phrase is still being recognized.
    append(session, 1)
    session.finish()
    release.set()
    await worker
    assert heard == [3 * session.rate, 1 * session.rate]
    assert session.raw.startswith("phrase 1")
    assert "phrase 2" in session.raw
    session.close()


@pytest.mark.asyncio
async def test_pause_commits_phrase_before_finish(tmp_path, monkeypatch):
    session, events = make_session(tmp_path, monkeypatch)
    session.recognize = AsyncMock(return_value="One phrase.")
    append(session, 1.5)
    append(session, 0.8, amplitude=0)
    worker = asyncio.create_task(session.run())
    await asyncio.sleep(0)
    # The period Whisper gave the phrase because the audio paused isn't kept.
    assert events[-1]["accepted_text"] == "One phrase"
    session.finish()
    await worker
    assert session.recognize.await_count == 1
    session.close()
    assert not session.path.exists()


def test_frames_reject_gaps_and_bound_memory(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    with pytest.raises(ValueError, match="contiguous"):
        session.append(struct.pack("<IIh", 1, 0, 100))
    session.close()


@pytest.mark.asyncio
async def test_recognition_backlog_falls_back_to_full_audio(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    stt = type("STT", (), {"transcribe": AsyncMock(return_value="the whole recording")})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    # Recognition never runs, so a minute of audio piles up unprocessed.
    append(session, 61)
    assert len(session.pending) <= session.rate * 2 * 60
    assert session.degraded_reason
    session.finish()
    await session.run()
    assert session.raw == "the whole recording"
    assert session.covered == session.samples == 61 * session.rate
    stt.transcribe.assert_awaited_once()
    session.close()


@pytest.mark.asyncio
async def test_cross_phrase_correction_revises_accepted_output(tmp_path, monkeypatch):
    session, events = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    monkeypatch.setattr(
        capture_stream,
        "refine_transcript",
        AsyncMock(side_effect=[("The meeting is on Tuesday.", "0.6B"), ("The meeting is on Wednesday.", "0.6B")]),
    )
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    await session.accept("the meeting is on Tuesday")
    await session.accept("no actually Wednesday")
    assert capture_stream.refine_transcript.await_args.args[0] == "the meeting is on Tuesday no actually Wednesday"
    # The retraction passes the content check, so the revision is accepted
    # without another full-dictation pass.
    assert not session.needs_final_refinement
    assert not session.reviews
    monkeypatch.setattr(
        capture_stream, "refine_transcript", AsyncMock(return_value=("The meeting is on Wednesday.", "0.6B"))
    )
    session.finish()
    await session.run()
    assert "Wednesday" in session.refined
    assert "Tuesday" not in session.refined
    assert "Tuesday" in session.raw
    assert events[-1]["type"] == "refined"
    session.close()


@pytest.mark.asyncio
async def test_refinement_failure_keeps_raw_capture(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    monkeypatch.setattr(capture_stream, "refine_transcript", AsyncMock(side_effect=RuntimeError("model unavailable")))
    await session.accept("Keep this text.")
    assert session.raw == "Keep this text."
    assert session.refinement_error == "model unavailable"
    session.close()


def test_overlap_preserves_nonmatching_text():
    assert capture_stream.join_overlap("hello world", "world again") == "hello world again"
    assert capture_stream.join_overlap("one two", "three four") == "one two three four"


@pytest.mark.asyncio
async def test_forced_boundary_finalizes_with_full_audio(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.recognize = AsyncMock(return_value="provisional repeated word")
    stt = type("STT", (), {"transcribe": AsyncMock(return_value="accurate final transcript")})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    append(session, 21)
    session.finish()
    await session.run()
    assert session.degraded_reason
    assert session.raw == "accurate final transcript"
    stt.transcribe.assert_awaited_once()
    session.close()


@pytest.mark.asyncio
async def test_silence_does_not_invoke_whisper(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    stt = type("STT", (), {"transcribe_array": AsyncMock()})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    append(session, 2, amplitude=0)
    session.finish()
    await session.run()
    assert session.raw == ""
    stt.transcribe_array.assert_not_awaited()
    session.close()


@pytest.mark.asyncio
async def test_a_take_with_noise_but_no_voice_never_reaches_whisper(tmp_path, monkeypatch):
    # Whisper answers room noise with "Thank you."; the real detector hears no voice.
    from backend.services import speech_detect

    session, _ = make_session(tmp_path, monkeypatch)
    monkeypatch.setattr(capture_stream, "SpeechDetector", speech_detect.SpeechDetector)
    session.speech = speech_detect.SpeechDetector(session.rate)
    stt = type("STT", (), {"transcribe_array": AsyncMock(return_value="Thank you.")})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    noise = np.random.default_rng(0).normal(0, 1200, session.rate * 2).astype("<i2").tobytes()
    session.append(struct.pack("<II", session.sequence, session.samples) + noise[:32000])
    session.append(struct.pack("<II", session.sequence, session.samples) + noise[32000:])
    session.finish()
    await session.run()
    assert session.raw == ""
    stt.transcribe_array.assert_not_awaited()
    session.close()


class VoiceBetween(HeardWhenLoud):
    """Hears a voice only between the given sample offsets, however loud the rest is."""

    def __init__(self, rate, spans):
        super().__init__(rate)
        self.spans = spans

    def quiet(self):
        ended = [end for start, end in self.spans if start < self.samples]
        return self.samples - min(max(ended, default=0), self.samples)


def test_a_pause_under_a_loud_hum_still_ends_a_phrase(tmp_path, monkeypatch):
    # A fan's hum measured ~1000 RMS on the user's microphone, as loud as their
    # voice, so the audio never got quiet enough to count as a pause.
    session, _ = make_session(tmp_path, monkeypatch)
    rate = session.rate
    session.speech = VoiceBetween(rate, [(0, 2 * rate), (3 * rate, 5 * rate)])
    for _ in range(50):
        append(session, 0.1, amplitude=1000)
    assert session.cuts == [round(2.7 * rate)]
    session.close()


@pytest.mark.asyncio
async def test_each_phrase_is_recognized_in_context_of_earlier_phrases(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    stt = type("STT", (), {"transcribe_array": AsyncMock(side_effect=["How", "much is it?"])})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    append(session, 2)
    append(session, 1, amplitude=0)
    append(session, 2)
    session.finish()
    await session.run()
    assert [call.kwargs["previous_text"] for call in stt.transcribe_array.await_args_list] == ["", "How"]
    # Phrases go to Whisper in memory, at the stream's own sample rate.
    assert all(call.args[1] == session.rate for call in stt.transcribe_array.await_args_list)
    assert session.raw == "How much is it?"
    session.close()


def socket_app(tmp_path, monkeypatch):
    from fastapi import FastAPI
    from sqlalchemy.orm import sessionmaker

    from backend.routes import capture_stream as route

    monkeypatch.setattr(config, "_data_dir", tmp_path)
    engine = create_engine(f"sqlite:///{tmp_path / 'test.db'}", connect_args={"check_same_thread": False})
    Base.metadata.create_all(engine)
    monkeypatch.setattr(route.database_session, "SessionLocal", sessionmaker(bind=engine))
    monkeypatch.setattr(route, "get_capture_settings", lambda _: CaptureSettingsResponse(auto_refine=False))
    monkeypatch.setattr(route, "is_allowed_websocket_origin", lambda _: True)
    monkeypatch.setattr(capture_stream.StreamingCapture, "recognize", AsyncMock(return_value="Finished text."))
    app = FastAPI()
    app.include_router(route.router)
    return app, engine


def test_websocket_finish_and_recovery_do_not_duplicate_capture(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient

    app, engine = socket_app(tmp_path, monkeypatch)
    with TestClient(app) as client:
        with client.websocket_connect("/captures/stream") as socket:
            socket.send_json(
                dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
            )
            ready = socket.receive_json()
            session_id = ready["session_id"]
            assert client.get(f"/captures/stream/{session_id}/result").status_code == 202
            socket.send_bytes(struct.pack("<II", 0, 0) + np.ones(1600, dtype="<i2").tobytes())
            socket.send_json(dict(type="finish"))
            while True:
                event = socket.receive_json()
                if event["type"] == "final":
                    break
            assert event["capture"]["id"] == session_id
            assert event["refinement_complete"]
        assert client.get(f"/captures/stream/{session_id}/result").json() == event
    with Session(engine) as db:
        assert db.query(Capture).count() == 1


def test_finish_records_the_app_dictated_into(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient

    app, engine = socket_app(tmp_path, monkeypatch)
    with TestClient(app) as client, client.websocket_connect("/captures/stream") as socket:
        socket.send_json(dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le"))
        socket.receive_json()
        socket.send_bytes(struct.pack("<II", 0, 0) + np.ones(1600, dtype="<i2").tobytes())
        socket.send_json(dict(type="finish", app=dict(bundle_id="com.tinyspeck.slackmacapp", name=" Slack ")))
        while (event := socket.receive_json())["type"] != "final":
            pass
    assert event["capture"]["app_bundle_id"] == "com.tinyspeck.slackmacapp"
    assert event["capture"]["app_name"] == "Slack"
    with Session(engine) as db:
        assert db.query(Capture).one().app_name == "Slack"


def test_target_app_drops_blank_and_non_string_parts():
    from backend.services.captures import target_app

    assert target_app("com.apple.mail", "Mail") == ("com.apple.mail", "Mail")
    assert target_app("  ", 7) == (None, None)
    assert len(target_app("x" * 1000, None)[0]) == 255


def test_cancel_discards_audio_without_capture(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient

    app, engine = socket_app(tmp_path, monkeypatch)
    with TestClient(app) as client:
        with client.websocket_connect("/captures/stream") as socket:
            socket.send_json(
                dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
            )
            session_id = socket.receive_json()["session_id"]
            socket.send_bytes(struct.pack("<II", 0, 0) + np.ones(1600, dtype="<i2").tobytes())
            socket.send_json(dict(type="cancel"))
            assert socket.receive()["type"] == "websocket.close"
        assert client.get(f"/captures/stream/{session_id}/result").status_code == 404
    with Session(engine) as db:
        assert db.query(Capture).count() == 0
    assert not list(config.get_captures_dir().glob("*.wav"))


def test_cancel_after_finish_discards_the_take(tmp_path, monkeypatch):
    """Escape while the last phrase is still being recognized or cleaned up."""
    import threading
    import time

    from fastapi.testclient import TestClient

    app, engine = socket_app(tmp_path, monkeypatch)
    finishing = threading.Event()
    sessions = []

    async def slow_run(self):
        sessions.append(self)
        await asyncio.to_thread(finishing.wait, 5)

    monkeypatch.setattr(capture_stream.StreamingCapture, "run", slow_run)
    with TestClient(app) as client:
        with client.websocket_connect("/captures/stream") as socket:
            socket.send_json(
                dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
            )
            session_id = socket.receive_json()["session_id"]
            socket.send_bytes(struct.pack("<II", 0, 0) + np.ones(1600, dtype="<i2").tobytes())
            socket.send_json(dict(type="finish"))
            socket.send_json(dict(type="cancel"))
            deadline = time.monotonic() + 5
            while not sessions[0].abort and time.monotonic() < deadline:
                time.sleep(0.01)
            assert sessions[0].abort
            finishing.set()
            assert socket.receive()["type"] == "websocket.close"
        assert client.get(f"/captures/stream/{session_id}/result").status_code == 404
    with Session(engine) as db:
        assert db.query(Capture).count() == 0
    assert not list(config.get_captures_dir().glob("*.wav"))


def test_origin_rejection_and_session_cap(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient
    from starlette.websockets import WebSocketDisconnect

    from backend.routes import capture_stream as route

    app, _ = socket_app(tmp_path, monkeypatch)
    with TestClient(app) as client:
        monkeypatch.setattr(route, "is_allowed_websocket_origin", lambda _: False)
        with pytest.raises(WebSocketDisconnect), client.websocket_connect("/captures/stream"):
            pass
        monkeypatch.setattr(route, "is_allowed_websocket_origin", lambda _: True)
        monkeypatch.setattr(route, "MAX_SESSIONS", 0)
        with pytest.raises(WebSocketDisconnect), client.websocket_connect("/captures/stream"):
            pass
    assert not route._active_sessions


def test_malformed_start_and_oversized_command_release_slot(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient

    from backend.routes import capture_stream as route

    app, _ = socket_app(tmp_path, monkeypatch)
    with TestClient(app) as client:
        with client.websocket_connect("/captures/stream") as socket:
            socket.send_json({"type": "start"})
            assert socket.receive_json()["type"] == "error"
            assert socket.receive()["type"] == "websocket.close"
        with client.websocket_connect("/captures/stream") as socket:
            socket.send_json(
                dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
            )
            assert socket.receive_json()["type"] == "ready"
            socket.send_text("x" * 5000)
            assert socket.receive_json()["message"] == "Command too large"
            assert socket.receive()["type"] == "websocket.close"
    assert not route._active_sessions
    assert not list(config.get_captures_dir().glob("*.wav"))


@pytest.mark.asyncio
async def test_disconnect_after_finish_can_recover_result(tmp_path, monkeypatch):
    import json

    from backend.routes import capture_stream as route

    _, engine = socket_app(tmp_path, monkeypatch)

    class DisconnectedSocket:
        def __init__(self):
            self.messages = iter(
                [
                    {"type": "websocket.receive", "bytes": struct.pack("<II", 0, 0) + b"\0\0" * 100},
                    {"type": "websocket.receive", "text": '{"type":"finish"}'},
                ]
            )
            self.id = None

        async def accept(self):
            pass

        async def receive_text(self):
            return json.dumps(
                dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
            )

        async def receive(self):
            return next(self.messages)

        async def send_json(self, event):
            if event["type"] == "ready":
                self.id = event["session_id"]
            else:
                raise RuntimeError("connection lost")

        async def close(self):
            pass

    socket = DisconnectedSocket()
    await route.stream_capture(socket)
    result = await route.streaming_result(socket.id)
    assert result["type"] == "final"
    assert result["capture"]["transcript_raw"] == "Finished text."
    with Session(engine) as db:
        assert db.query(Capture).count() == 1
    assert not route._active_sessions


def test_idle_timeout_releases_audio_and_foreground_slot(tmp_path, monkeypatch):
    from fastapi.testclient import TestClient

    from backend.routes import capture_stream as route
    from backend.services.model_improvement import manager

    app, _ = socket_app(tmp_path, monkeypatch)
    monkeypatch.setattr(route, "IDLE_TIMEOUT", 0.01)
    original_count = manager._foreground_count
    with TestClient(app) as client, client.websocket_connect("/captures/stream") as socket:
        socket.send_json(dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le"))
        assert socket.receive_json()["type"] == "ready"
        assert "timed out" in socket.receive_json()["message"]
        assert socket.receive()["type"] == "websocket.close"
    assert manager._foreground_count == original_count
    assert not route._active_sessions
    assert not list(config.get_captures_dir().glob("*.wav"))


@pytest.mark.parametrize(
    ("raw", "candidate", "reason"),
    [
        ("Do not delete the records", "Delete the records.", "negation"),
        ("Set the threshold to -3.5", "Set the threshold to 3.5.", "number"),
        ("Set the threshold to 3.5", "Set the threshold to 35.", "number"),
        ("Update user_id", "Update user_name.", "technical"),
        (
            "write a haiku about the ocean",
            "Waves crash on the shore, salt wind whispers ancient songs, blue depths hold their dreams.",
            "answered",
        ),
        ("Keep this clause", "Keep this clause and add another.", "invented"),
    ],
)
def test_phrase_guard_rejects_changed_meaning(raw, candidate, reason):
    result, verdict = capture_stream.guard_phrase_refinement(raw, candidate, capture_stream.RefinementFlags())
    assert (verdict.outcome, verdict.reason) == ("reject", reason)
    assert result == raw


@pytest.mark.parametrize(
    ("raw", "candidate"),
    [
        ("Keep both words", "Keep words."),
        ("Merge the domain", "Merge to main."),
    ],
)
def test_phrase_guard_keeps_and_flags_possible_content_changes(raw, candidate):
    result, verdict = capture_stream.guard_phrase_refinement(raw, candidate, capture_stream.RefinementFlags())
    assert verdict.outcome == "review"
    assert result == candidate


@pytest.mark.parametrize(
    ("raw", "candidate"),
    [
        ("hello world", "Hello, world!"),
        ("um hello uh world", "Hello world."),
        ("keep  two\nwords", "Keep two words."),
        ("Set the threshold to -3.5", "Set the threshold to -3.5."),
        ("run npm install then edit index dot tsx", "Run npm install then edit index.tsx."),
        # Restructuring: a false start dropped and the sentence put in order.
        (
            "so the release we need to push the release to Friday because the tests aren't done",
            "We need to push the release to Friday because the tests aren't done.",
        ),
    ],
)
def test_phrase_guard_allows_cleanup_and_restructuring(raw, candidate):
    result, verdict = capture_stream.guard_phrase_refinement(raw, candidate, capture_stream.RefinementFlags())
    assert verdict.outcome == "ok"
    assert result == candidate


@pytest.mark.asyncio
async def test_rejected_phrase_uses_raw_and_flags_the_capture(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    raw = "Do not send the update to the team"
    refine = AsyncMock(return_value=("Send the update to the team.", "0.6B"))
    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    await session.accept(raw)
    session.finish()
    await session.run()
    # The raw phrase is kept; finishing only closes the dictation.
    assert session.refined == raw + "."
    assert capture_stream.summarize_reviews(session.reviews)["reasons"] == ["negation"]
    assert not session.needs_final_refinement
    refine.assert_awaited_once()
    session.close()


@pytest.mark.asyncio
async def test_a_correction_after_a_learned_change_is_cleaned_from_what_was_said(tmp_path, monkeypatch):
    from backend.services import correction_learning

    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    monkeypatch.setattr(
        correction_learning,
        "apply_learned_corrections",
        lambda text, _: text.replace("Tuesday", "Friday"),
    )
    refine = AsyncMock(side_effect=[("Meeting Tuesday", "0.6B"), ("Meeting Wednesday.", "0.6B")])
    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    stt = type("STT", (), {"transcribe": AsyncMock()})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    await session.accept("Meeting Tuesday", paused=True)
    assert session.refined == "Meeting Friday"
    await session.accept("actually Wednesday", paused=True)
    # The open sentence is cleaned again from what was said, so a learned
    # substitution in the shown text can't get in the way.
    assert refine.await_args.args[0] == "Meeting Tuesday actually Wednesday"
    assert not session.needs_final_refinement
    session.finish()
    await session.run()
    assert session.refined == "Meeting Wednesday."
    stt.transcribe.assert_not_awaited()
    session.close()


def test_websocket_uses_database_factory_initialized_at_startup(tmp_path, monkeypatch):
    """Importing the route before init_db must not capture a stale None factory."""
    from contextlib import asynccontextmanager

    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.database import session as database_session
    from backend.routes import capture_stream as route

    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(database_session, "engine", None)
    monkeypatch.setattr(database_session, "SessionLocal", None)
    monkeypatch.setattr(database_session, "_db_path", None)
    monkeypatch.setattr(route, "is_allowed_websocket_origin", lambda _: True)
    # The default settings refine, which would load the real cleanup model.
    monkeypatch.setattr(route, "load_cleanup_model", AsyncMock())

    @asynccontextmanager
    async def lifespan(_):
        database_session.init_db()
        try:
            yield
        finally:
            database_session.engine.dispose()

    app = FastAPI(lifespan=lifespan)
    app.include_router(route.router)
    with TestClient(app) as client, client.websocket_connect("/captures/stream") as socket:
        socket.send_json(dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le"))
        assert socket.receive_json()["type"] == "ready"
        socket.send_json({"type": "cancel"})
        assert socket.receive()["type"] == "websocket.close"
    assert not route._active_sessions


@pytest.mark.parametrize(("source", "loads"), [("dictation", True), ("command", False)])
def test_a_dictation_loads_its_cleanup_model_while_the_user_speaks(tmp_path, monkeypatch, source, loads):
    # Loading the model (or swapping its adapter back) must not wait for release.
    from fastapi.testclient import TestClient

    from backend.routes import capture_stream as route

    app, _ = socket_app(tmp_path, monkeypatch)
    monkeypatch.setattr(route, "get_capture_settings", lambda _: CaptureSettingsResponse(auto_refine=True))
    load = AsyncMock()
    monkeypatch.setattr(route, "load_cleanup_model", load)
    start = dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
    with TestClient(app) as client, client.websocket_connect("/captures/stream") as socket:
        socket.send_json({**start, "source": source})
        assert socket.receive_json()["type"] == "ready"
        socket.send_json({"type": "cancel"})
        assert socket.receive()["type"] == "websocket.close"
    assert load.await_count == (1 if loads else 0)
    if loads:
        assert load.await_args.args[1] == "0.6B"


async def _dictate(tmp_path, monkeypatch, style, phrases):
    session, _ = make_session(tmp_path, monkeypatch, punctuation_style=style)
    session.settings.auto_refine = True
    session.flags.punctuation_style = style

    async def refine(text, flags, model_size=None):
        return text[0].upper() + text[1:] + ("" if text.endswith("?") else "."), "0.6B"

    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    for phrase in phrases[:-1]:
        await session.accept(phrase, paused=True)
    session.finish()
    await session.accept(phrases[-1])
    await session.run()
    session.close()
    return session.refined


@pytest.mark.asyncio
async def test_pause_inside_a_sentence_does_not_add_a_period(tmp_path, monkeypatch):
    refined = await _dictate(tmp_path, monkeypatch, "standard", ["If I'm contributing 6%, how", "much is deducted?"])
    assert refined == "If I'm contributing 6%, how much is deducted?"


@pytest.mark.asyncio
async def test_the_cleanup_decides_sentences_across_a_pause(tmp_path, monkeypatch):
    # Whisper's capital after the pause isn't taken as a sentence start; the
    # cleanup sees both phrases and decides.
    refined = await _dictate(tmp_path, monkeypatch, "standard", ["It might", "But we'll see"])
    assert refined == "It might but we'll see."


@pytest.mark.asyncio
async def test_a_long_tail_settled_mid_sentence_joins_the_style_way(tmp_path, monkeypatch):
    monkeypatch.setattr(capture_stream, "MAX_OPEN_WORDS", 1)
    refined = await _dictate(tmp_path, monkeypatch, "casual", ["It might.", "Is it?", "We'll see"])
    assert refined == "It might is it? We'll see."


@pytest.mark.asyncio
async def test_learned_style_finishes_the_way_the_user_writes(tmp_path, monkeypatch):
    # Stand-in habits: sentence breaks become commas and the final period goes.
    monkeypatch.setattr(
        capture_stream,
        "apply_learned",
        lambda text, style=None: text.replace(". B", ", b").removesuffix("."),
    )
    session, _ = make_session(tmp_path, monkeypatch, punctuation_style="learned")
    session.settings.auto_refine = True
    session.flags.punctuation_style = "learned"
    monkeypatch.setattr(capture_stream, "refine_transcript", AsyncMock(return_value=("It might. But we'll see.", "4B")))
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    await session.accept("it might", paused=True)
    session.finish()
    await session.accept("but we'll see")
    await session.run()
    session.close()
    assert session.refined == "It might, but we'll see"


@pytest.mark.asyncio
async def test_finish_keeps_the_ending_the_cleanup_chose(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True

    async def refine(text, flags, model_size=None):
        return "A list of things to do:\n1. Go to school\n2. Come home\n3. Do chores", "4B"

    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    await session.accept("A list of things to do. Go to school, come home, do chores.")
    session.finish()
    await session.run()
    session.close()
    assert session.refined.endswith("3. Do chores")


def expressive_session(tmp_path, monkeypatch, source="dictation", **settings):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    start = dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le", source=source)
    return capture_stream.StreamingCapture(start, CaptureSettingsResponse(auto_refine=True, **settings), None)


def excited_voice():
    from backend.backends.word_timing import Word
    from backend.services.prosody import Frames, Voice

    words = [
        Word(word, index * 0.3, index * 0.3 + 0.3) for index, word in enumerate(["we", "finally", "shipped", "it"])
    ]
    return Voice(Frames(np.full(120, 220.0), np.full(120, -14.0)), words)


async def voice_ready(session):
    async def worked_out():
        voice = excited_voice()
        # "finally" drawn out.
        return voice, [dict(word="finally", at=1, stretch=4.6)]

    from backend.services.prosody import Baseline

    session.baseline = Baseline((7.0, 1.0), (0.0, 1.0), (-20.0, 1.0))
    session.voice = asyncio.create_task(worked_out())
    await session.voice


@pytest.mark.asyncio
async def test_a_sentence_said_with_energy_ends_with_an_exclamation_mark(tmp_path, monkeypatch):
    session = expressive_session(tmp_path, monkeypatch)
    await voice_ready(session)
    assert session.express("We finally shipped it.") == "We finallyyyy shipped it!"
    # Only a period becomes "!"; a word the cleanup changed isn't drawn out.
    assert session.express("Did we eventually ship it?") == "Did we eventually ship it?"
    session.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("source", "settings"),
    [
        # The switch is off: measured, never written.
        ("dictation", {"expressive": False}),
        # Without cleanup the transcript is pasted as heard.
        ("dictation", {"auto_refine": False}),
        # A command's words are an instruction, not the output.
        ("command", {}),
    ],
)
async def test_the_text_keeps_its_period_unless_written_how_it_was_said(tmp_path, monkeypatch, source, settings):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    start = dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le", source=source)
    session = capture_stream.StreamingCapture(start, CaptureSettingsResponse(**{"auto_refine": True, **settings}), None)
    await voice_ready(session)
    assert session.express("We finally shipped it.", final=True) == "We finally shipped it."
    session.close()


@pytest.mark.asyncio
async def test_the_final_text_never_waits_for_how_it_was_said(tmp_path, monkeypatch):
    session = expressive_session(tmp_path, monkeypatch)
    from backend.services.prosody import Baseline

    session.baseline = Baseline((7.0, 1.0), (0.0, 1.0), (-20.0, 1.0))
    never = asyncio.get_running_loop().create_future()
    session.voice = never
    assert session.express("We finally shipped it.", final=True) == "We finally shipped it."
    never.cancel()
    session.close()
