"""A stand-in for a shared Kass records what it changes, sends it, and copies the shared Kass (services/sync)."""

import json
from datetime import datetime, timedelta

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import (
    Base,
    Capture,
    CaptureFeedback,
    CaptureSettings,
    DictionaryEntry,
    SyncChange,
    TakeReport,
    WritingStyle,
)
from backend.routes.sync import router
from backend.services import sync
from backend.services.sync import changes, importer, job, mirror


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


@pytest.fixture
def shared(monkeypatch):
    """The shared Kass's database, applying what the stand-in sends."""
    factory = database()
    with factory() as db:
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


def dictate(db, data_dir, capture_id="take", text="hello there"):
    (data_dir / "captures").mkdir(exist_ok=True)
    (data_dir / "captures" / f"{capture_id}.wav").write_bytes(b"RIFF-audio")
    db.add(
        Capture(
            id=capture_id,
            audio_path=f"captures/{capture_id}.wav",
            source="dictation",
            transcript_raw=text,
            transcript_refined=text.capitalize() + ".",
            mac_id="laptop",
        )
    )
    db.commit()


def send(shared_factory):
    """One batch from the stand-in, applied by the shared Kass as /sync/import would."""
    payload, files, keys = job._batch()
    audio = {name.rsplit(".", 1)[0]: (name, data) for _field, (name, data, _type) in files}
    with shared_factory() as db:
        importer.apply(db, json.loads(json.dumps(payload)), audio)
    with database_session.SessionLocal() as db:
        changes.sent(db, keys)
    return payload


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


def test_a_dictation_reaches_the_shared_kass_with_its_recording_and_report(stand_in, shared, data_dir):
    with stand_in() as db:
        dictate(db, data_dir)
        db.add(
            CaptureFeedback(
                id="report",
                capture_id="take",
                target="refined",
                expected_text="Hello there!",
                snapshot="{}",
                source="manual",
            )
        )
        db.add(TakeReport(mode="dictation", outcome="delivered", latency_ms=420))
        db.commit()

    send(shared)

    with shared() as db:
        capture = db.get(Capture, "take")
        assert capture.transcript_refined == "Hello there."
        assert capture.mac_id == "laptop"
        assert config.resolve_storage_path(capture.audio_path).read_bytes() == b"RIFF-audio"
        assert db.get(CaptureFeedback, "report").expected_text == "Hello there!"
        assert db.query(TakeReport).one().latency_ms == 420
    assert recorded(stand_in) == {}


def test_sending_a_batch_twice_changes_nothing_more(stand_in, shared, data_dir):
    with stand_in() as db:
        dictate(db, data_dir)
    payload, files, _keys = job._batch()
    audio = {name.rsplit(".", 1)[0]: (name, data) for _field, (name, data, _type) in files}
    for _ in range(2):
        with shared() as db:
            importer.apply(db, json.loads(json.dumps(payload)), audio)
    with shared() as db:
        assert db.query(Capture).count() == 1


def test_a_dictation_deleted_on_the_stand_in_goes_from_the_shared_kass(stand_in, shared, data_dir):
    with stand_in() as db:
        dictate(db, data_dir)
    send(shared)
    with stand_in() as db:
        db.delete(db.get(Capture, "take"))
        db.commit()
    send(shared)
    with shared() as db:
        assert db.get(Capture, "take") is None


def test_the_later_edit_wins(stand_in, shared):
    with shared() as db:
        db.add(entry("a", "Kass", at=datetime.utcnow() + timedelta(hours=1)))
        db.add(entry("b", "Midna", at=datetime.utcnow() - timedelta(hours=1)))
        db.commit()
    with stand_in() as db:
        db.add(entry("a", "KASS"))
        db.add(entry("b", "MIDNA"))
        db.commit()
    send(shared)
    with shared() as db:
        assert db.get(DictionaryEntry, "a").written == "Kass"
        assert db.get(DictionaryEntry, "b").written == "MIDNA"


def test_the_same_word_added_on_both_macs_is_kept_once(stand_in, shared):
    with shared() as db:
        db.add(entry("theirs", "Kass", at=datetime.utcnow() - timedelta(hours=1)))
        db.commit()
    with stand_in() as db:
        db.add(entry("ours", "Kass"))
        db.commit()
    send(shared)
    with shared() as db:
        assert [row.id for row in db.query(DictionaryEntry).all()] == ["ours"]


def test_settings_this_mac_owns_stay_put(stand_in, shared):
    with shared() as db:
        db.get(CaptureSettings, 1).input_device_id = "studio-mic"
        db.get(CaptureSettings, 1).updated_at = datetime.utcnow() - timedelta(hours=1)
        db.commit()
    with stand_in() as db:
        settings = db.get(CaptureSettings, 1)
        settings.input_device_id = "laptop-mic"
        settings.live_text = True
        db.commit()
    send(shared)
    with shared() as db:
        settings = db.get(CaptureSettings, 1)
        assert settings.live_text is True
        assert settings.input_device_id == "studio-mic"


def test_a_change_made_while_sending_is_sent_next_time(stand_in, shared):
    with stand_in() as db:
        db.add(entry("a", "Kass"))
        db.commit()
    _payload, _files, keys = job._batch()
    with stand_in() as db:
        db.get(DictionaryEntry, "a").written = "KASS"
        db.commit()
    with database_session.SessionLocal() as db:
        changes.sent(db, keys)
    assert recorded(stand_in) == {("dictionary", "a"): False}


def test_the_stand_in_copies_the_shared_kass(stand_in, shared, data_dir, monkeypatch):
    monkeypatch.setattr(mirror, "_forget_cached", lambda: None)
    with shared() as db:
        db.add(entry("theirs", "Kass"))
        db.add(WritingStyle(id="work", name="Work"))
        db.get(CaptureSettings, 1).llm_model = "4B"
        db.commit()
        (data_dir / "writing-style.json").write_text(json.dumps({"version": 2, "styles": {}}))
        data = json.loads(json.dumps(mirror.snapshot(db), default=str))
    (data_dir / "writing-style.json").unlink()
    with stand_in() as db:
        db.add(entry("stale", "Herga"))
        db.commit()
        db.query(SyncChange).delete()
        db.commit()
        mirror.apply(db, data, lambda *_: None)
    with stand_in() as db:
        assert [row.id for row in db.query(DictionaryEntry).all()] == ["theirs"]
        assert {row.id for row in db.query(WritingStyle).all()} == {"default", "work"}
        assert db.get(CaptureSettings, 1).llm_model == "4B"
    assert json.loads((data_dir / "writing-style.json").read_text())["version"] == 2
    assert recorded(stand_in) == {}


def test_a_snapshot_says_when_anything_changed(shared):
    with shared() as db:
        before = mirror.snapshot(db)["revision"]
        assert mirror.snapshot(db)["revision"] == before
        db.add(entry("a", "Kass"))
        db.commit()
        assert mirror.snapshot(db)["revision"] != before


def test_a_stand_in_turns_down_changes_meant_for_the_shared_kass(stand_in):
    app = FastAPI()
    app.include_router(router)
    response = TestClient(app).post("/sync/import", data={"changes": "[]"})
    assert response.status_code == 409


def test_the_shared_kass_imports_over_http(shared, monkeypatch, data_dir):
    monkeypatch.delenv(sync.SHARED_SERVER_ENV, raising=False)
    app = FastAPI()
    app.include_router(router)

    def get_db():
        with shared() as db:
            yield db

    from backend.database import get_db as real_get_db

    app.dependency_overrides[real_get_db] = get_db
    (data_dir / "captures").mkdir()
    row = {
        "id": "take",
        "audio_path": "captures/take.wav",
        "source": "dictation",
        "transcript_raw": "hi",
        "created_at": datetime.utcnow().isoformat(),
    }
    change = {
        "kind": "capture",
        "id": "take",
        "deleted": False,
        "changed_at": datetime.utcnow().isoformat(),
        "row": row,
    }
    response = TestClient(app).post(
        "/sync/import",
        data={"changes": json.dumps([change])},
        files=[("audio", ("take.wav", b"RIFF", "application/octet-stream"))],
    )
    assert response.status_code == 204
    with shared() as db:
        assert db.get(Capture, "take").transcript_raw == "hi"
    assert (data_dir / "captures" / "take.wav").read_bytes() == b"RIFF"


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


def test_one_sync_sends_what_changed_then_copies_the_shared_kass(stand_in, shared, data_dir, monkeypatch):
    import asyncio

    import httpx

    monkeypatch.setattr(mirror, "_forget_cached", lambda: None)

    async def no_downloads(_settings):
        return None

    monkeypatch.setattr(job, "_fetch_models", no_downloads)
    monkeypatch.setattr(job, "_state", {**job._state, "revision": None, "copied_at": 0.0})
    app = FastAPI()
    app.include_router(router)
    app.add_api_route("/health", lambda: {"status": "healthy"})

    def get_db():
        with shared() as db:
            yield db

    from backend.database import get_db as real_get_db

    app.dependency_overrides[real_get_db] = get_db
    with shared() as db:
        db.add(entry("theirs", "Studio"))
        db.commit()
    with stand_in() as db:
        dictate(db, data_dir)

    async def run():
        # The shared Kass reads the stand-in's environment in-process: it isn't standing in.
        monkeypatch.delenv(sync.SHARED_SERVER_ENV)
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(transport=transport, base_url="http://studio") as client:
            await job.sync_once(client, "http://studio")

    asyncio.run(run())

    with shared() as db:
        assert db.get(Capture, "take") is not None
    with stand_in() as db:
        assert db.query(SyncChange).count() == 0
        assert [row.id for row in db.query(DictionaryEntry).all()] == ["theirs"]
    assert job._state["reachable"] is True
