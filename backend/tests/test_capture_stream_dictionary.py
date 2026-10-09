"""Streaming dictation uses the dictionary of the app it goes to (docs/plans/DICTIONARIES.md)."""

import asyncio
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend.database import session as database_session
from backend.database.models import Base
from backend.services import capture_stream, correction_learning, dictionary, phrase_seams, styles
from backend.tests.test_capture_stream import append, make_session

ZED = "dev.zed.Zed"


@pytest.fixture
def database(monkeypatch):
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", make)
    monkeypatch.setattr(phrase_seams, "_common_word", lambda word: False)
    with make() as db:
        styles.ensure_styles(db)
        dictionary.add_group(db, "Kubernetes", None, [{"scope": "global"}])
        dictionary.add_group(db, "Voicebox", "voice box", [{"scope": "app", "scope_id": ZED, "app_name": "Zed"}])
    return make


async def dictate(tmp_path, monkeypatch, app):
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    stt = type("STT", (), {"transcribe_array": AsyncMock(return_value="I deployed voice box on kubernetes")})()
    monkeypatch.setattr(capture_stream, "get_whisper_model", lambda: stt)
    monkeypatch.setattr(
        capture_stream, "refine_transcript", AsyncMock(return_value=("I deployed voice box on kubernetes.", "0.6B"))
    )
    monkeypatch.setattr(capture_stream, "known_names", lambda: frozenset())
    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    if app:
        session.set_app(app, "Zed")
    worker = asyncio.create_task(session.run())
    append(session, 1)
    session.finish()
    await worker
    session.close()
    return session, stt


@pytest.mark.asyncio
async def test_the_apps_dictionary_reaches_whisper_and_the_finished_text(tmp_path, monkeypatch, database):
    session, stt = await dictate(tmp_path, monkeypatch, ZED)

    # Terms first: a replacement fixes its word whatever Whisper hears.
    assert stt.transcribe_array.await_args.kwargs["vocabulary"] == ("Kass", "Kubernetes", "Voicebox")
    assert session.refined == "I deployed Voicebox on Kubernetes."
    assert {"Voicebox", "Kubernetes"} <= session.names


@pytest.mark.asyncio
async def test_without_an_app_only_the_global_entries_apply(tmp_path, monkeypatch, database):
    session, stt = await dictate(tmp_path, monkeypatch, None)

    assert stt.transcribe_array.await_args.kwargs["vocabulary"] == ("Kass", "Kubernetes")
    assert session.refined == "I deployed voice box on Kubernetes."


def test_provisional_text_holds_back_a_dictionary_match(tmp_path, monkeypatch, database):
    monkeypatch.setattr(capture_stream, "_has_corrections", lambda: False)
    session, _ = make_session(tmp_path, monkeypatch)
    # The global term "Kubernetes", which may be heard as two words ("cuber
    # netes"), and the word after them.
    assert session.holdback(1) == 3
    session.set_app(ZED, "Zed")
    # "voice box" is two words, and the one after decides the match.
    assert session.holdback(1) == 3
    session.close()
