"""The Captures app list: per-app counts and the app filter."""

from datetime import datetime, timedelta

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool

from backend.database.models import Base, Capture
from backend.services.captures import list_capture_apps, list_captures

START = datetime(2026, 9, 1, 9, 0)


@pytest.fixture
def db():
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        yield session
    engine.dispose()


def add(db, n, bundle_id, name, minutes=0):
    for i in range(n):
        db.add(
            Capture(
                audio_path="captures/x.wav",
                source="dictation",
                transcript_raw="hi",
                app_bundle_id=bundle_id,
                app_name=name,
                created_at=START + timedelta(minutes=minutes + i),
            )
        )
    db.commit()


def test_counts_every_row_most_first(db):
    add(db, 3, "com.tinyspeck.slackmacgap", "Slack")
    add(db, 205, "com.apple.mail", "Mail")
    add(db, 2, None, None)

    result = list_capture_apps(db)

    assert result.total == 210
    assert result.unknown_count == 2
    assert [(a.app_bundle_id, a.count) for a in result.apps] == [
        ("com.apple.mail", 205),
        ("com.tinyspeck.slackmacgap", 3),
    ]


def test_app_takes_its_newest_name(db):
    add(db, 1, "com.example.editor", "Old Name", minutes=0)
    add(db, 1, "com.example.editor", "New Name", minutes=10)

    (app,) = list_capture_apps(db).apps

    assert app.app_name == "New Name"
    assert app.count == 2
    assert app.last_captured_at == START + timedelta(minutes=10)


def test_filters_to_one_app_or_to_no_app(db):
    add(db, 2, "com.apple.mail", "Mail")
    add(db, 1, "com.tinyspeck.slackmacgap", "Slack")
    add(db, 1, None, None)

    mail, total = list_captures(db, app_bundle_id="com.apple.mail")
    assert total == 2
    assert {c.app_bundle_id for c in mail} == {"com.apple.mail"}

    unknown, total = list_captures(db, unknown_app=True)
    assert total == 1
    assert unknown[0].app_bundle_id is None

    _, total = list_captures(db)
    assert total == 4


def test_the_app_list_counts_captures(db):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.database import get_db
    from backend.routes.captures import router

    add(db, 2, "com.apple.mail", "Mail")
    add(db, 1, None, None)
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_db] = lambda: db
    client = TestClient(app)

    apps = client.get("/captures/apps").json()
    assert apps["total"] == 3
    assert apps["unknown_count"] == 1
    assert apps["apps"][0]["app_name"] == "Mail"
