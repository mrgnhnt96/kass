"""Recording retention: keep a capture's text without its audio."""

import io
import json
from datetime import datetime, timedelta
from unittest.mock import AsyncMock

import numpy as np
import pytest
import soundfile as sf
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base, Capture, CaptureFeedback
from backend.services import captures, history_retention, known_names, personal_examples, styles
from backend.services.settings import get_capture_settings
from backend.tests.settings_rows import save_settings

NOW = datetime.utcnow()


@pytest.fixture
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(personal_examples, "_cache", None)
    monkeypatch.setattr(known_names, "_cache", None)
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", make)
    with make() as db:
        styles.ensure_styles(db)
        # Retention confirmed, 30 days: the sweep may delete.
        save_settings(db, history_retention_days=30)
    return make


def choose(storage, discard):
    with storage() as db:
        save_settings(db, discard_audio=discard)


def add(storage, capture_id, days_ago, corrected=False):
    """A dictation ``days_ago`` days old with its recording, optionally corrected."""
    audio = config.get_captures_dir() / f"{capture_id}.wav"
    audio.write_bytes(b"RIFF")
    with storage() as db:
        db.add(
            Capture(
                id=capture_id,
                audio_path=config.to_storage_path(audio),
                source="dictation",
                transcript_raw="teh plan",
                transcript_refined="Teh plan.",
                duration_ms=4000,
                created_at=NOW - timedelta(days=days_ago),
            )
        )
        if corrected:
            snapshot = {"transcript_raw": "teh plan", "audio_path": config.to_storage_path(audio)}
            db.add(
                CaptureFeedback(
                    capture_id=capture_id,
                    target="raw",
                    expected_text="the plan",
                    snapshot=json.dumps(snapshot),
                    created_at=NOW - timedelta(days=days_ago),
                )
            )
        db.commit()
    return audio


def correction_files():
    return sorted(path.name for path in history_retention.correction_audio_dir().iterdir())


def test_off_by_default(storage):
    # Upgrades get it off too (tauri/src-tauri/server/src/db.rs).
    with storage() as db:
        assert get_capture_settings(db).discard_audio is False


@pytest.mark.asyncio
@pytest.mark.parametrize("discard", [True, False])
async def test_an_upload_keeps_its_text_and_loses_its_recording_only_when_asked(storage, monkeypatch, discard):
    choose(storage, discard)
    stt = type("STT", (), {"model_size": "turbo", "transcribe": AsyncMock(return_value="Hello.")})()
    monkeypatch.setattr(captures, "get_whisper_model", lambda: stt)
    audio = io.BytesIO()
    sf.write(audio, np.zeros(16000), 16000, format="WAV")
    with storage() as db:
        result = await captures.create_capture(
            audio_bytes=audio.getvalue(),
            filename="take.wav",
            source="file",
            language=None,
            stt_model="turbo",
            db=db,
        )
    assert result.transcript_raw == "Hello."
    assert result.audio_deleted is discard
    assert bool(result.audio_path) is not discard
    assert any(config.get_captures_dir().iterdir()) is not discard


@pytest.mark.parametrize(("discard", "kept"), [(True, []), (False, ["old.wav"])])
def test_history_retention_keeps_correction_audio_only_while_recordings_are_kept(storage, discard, kept):
    add(storage, "old", 40, corrected=True)
    add(storage, "newest", 0)
    choose(storage, discard)
    assert history_retention.sweep() == 1
    assert correction_files() == kept
    # The correction itself still teaches.
    with storage() as db:
        assert db.query(CaptureFeedback).one().expected_text == "the plan"
