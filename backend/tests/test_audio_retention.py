"""Recording retention: keep a capture's text without its audio."""

import io
from unittest.mock import AsyncMock

import numpy as np
import pytest
import soundfile as sf
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base
from backend.services import captures, known_names, personal_examples, styles
from backend.services.settings import get_capture_settings
from backend.tests.settings_rows import save_settings


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
    return make


def choose(storage, discard):
    with storage() as db:
        save_settings(db, discard_audio=discard)


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
