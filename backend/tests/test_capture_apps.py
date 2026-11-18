"""The Captures app filter. The app list itself is the Rust server's
(``tauri/src-tauri/server/src/styles.rs``, ``capture_apps``)."""

from datetime import datetime, timedelta

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool

from backend.database.models import Base, Capture
from backend.services.captures import list_captures

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
