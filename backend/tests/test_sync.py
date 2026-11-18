"""A stand-in for a shared Kass records what it changes, to send to the
shared Kass (services/sync/changes.py). Sending, importing and copying are
the Rust server's (kass-server's sync module, tested there)."""

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base, CaptureSettings, DictionaryEntry, SyncChange, WritingStyle
from backend.services import sync
from backend.services.sync import changes


def database():
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    return sessionmaker(bind=engine, autoflush=False)


@pytest.fixture(autouse=True)
def data_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    return tmp_path


@pytest.fixture
def standing_in(monkeypatch):
    monkeypatch.setenv(sync.SHARED_SERVER_ENV, "http://studio.local:17493")
    changes.install()


@pytest.fixture
def stand_in(standing_in, monkeypatch):
    """The stand-in's database, as the server's own."""
    factory = database()
    monkeypatch.setattr(database_session, "SessionLocal", factory)
    with changes.not_recorded(), factory() as db:
        db.add(CaptureSettings(id=1))
        db.add(WritingStyle(id="default", name="Default", is_default=True))
        db.commit()
    return factory


def recorded(factory) -> dict:
    with factory() as db:
        return {(row.kind, row.ref_id): row.deleted for row in db.query(SyncChange).all()}


def entry(entry_id, written, at=None):
    return DictionaryEntry(
        id=entry_id, scope="global", scope_id="", written=written, key=written.lower(), updated_at=at
    )


def test_records_nothing_unless_standing_in(monkeypatch):
    monkeypatch.delenv(sync.SHARED_SERVER_ENV, raising=False)
    changes.install()
    factory = database()
    with factory() as db:
        db.add(entry("a", "Kass"))
        db.commit()
    assert recorded(factory) == {}


def test_records_new_edited_and_deleted_rows(stand_in):
    with stand_in() as db:
        db.add(entry("a", "Kass"))
        db.add(entry("b", "Midna"))
        db.commit()
        db.get(DictionaryEntry, "a").written = "KASS"
        db.delete(db.get(DictionaryEntry, "b"))
        db.commit()
    assert recorded(stand_in) == {("dictionary", "a"): False, ("dictionary", "b"): True}


def test_a_copy_from_the_shared_kass_isnt_recorded(stand_in):
    with changes.not_recorded(), stand_in() as db:
        db.add(entry("a", "Kass"))
        db.commit()
    assert recorded(stand_in) == {}


def test_settings_made_with_defaults_or_only_this_macs_own_arent_changes(standing_in):
    factory = database()
    with factory() as db:
        db.add(CaptureSettings(id=1))
        db.commit()
        db.get(CaptureSettings, 1).input_device_id = "laptop-mic"
        db.commit()
    assert recorded(factory) == {}
    with factory() as db:
        db.get(CaptureSettings, 1).live_text = True
        db.commit()
    assert recorded(factory) == {("settings", "1"): False}


def test_the_worker_says_which_trained_models_are_in_use_and_takes_the_shared_kass_s(monkeypatch):
    """The Rust server asks the worker for its trained models to serve a
    snapshot, and hands a stand-in's worker the copies it made."""
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes.sync import router
    from backend.services import dictionary, styles, writing_style
    from backend.services.model_improvement import manager

    in_use = {"llm": {"id": "run-1", "path": "/data/model-improvement/runs/run-1"}}
    used = []
    monkeypatch.setattr(manager, "in_use", lambda: in_use)
    monkeypatch.setattr(manager, "use_mirrored", used.append)
    forgot = []
    monkeypatch.setattr(dictionary, "invalidate", lambda: forgot.append("dictionary"))
    monkeypatch.setattr(styles, "invalidate", lambda: forgot.append("styles"))
    monkeypatch.setattr(writing_style, "_state", {"stale": True})
    app = FastAPI()
    app.include_router(router)
    client = TestClient(app)

    assert client.post("/worker/sync/adapters").json() == {"in_use": in_use}
    active = {"llm": {"id": "run-1", "path": "/stand-in/model-improvement/mirrored/run-1/llm"}}
    assert client.post("/worker/sync/mirrored", json={"active": active}).status_code == 200
    assert used == [active]
    assert forgot == ["styles", "dictionary"]
    assert writing_style._state is None


def test_waking_a_stand_in_loads_its_models(monkeypatch):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes.sync import router
    from backend.services import model_startup

    loads = []

    async def load():
        loads.append(True)

    monkeypatch.setattr(model_startup, "load_startup_models", load)
    app = FastAPI()
    app.include_router(router)
    assert TestClient(app).post("/worker/models/startup").status_code == 200
    assert loads == [True]
