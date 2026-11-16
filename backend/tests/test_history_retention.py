"""History retention: old captures go, what they taught and counted stays."""

import json
import os
from datetime import UTC, datetime, timedelta

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine, text
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.migrations import run_migrations
from backend.database.models import Base, Capture, CaptureFeedback, KnownName, RetiredCapture
from backend.services import (
    correction_notes,
    history_retention,
    known_names,
    personal_examples,
    styles,
    usage_stats,
    writing_style,
)
from backend.services.settings import get_capture_settings
from backend.tests.settings_rows import save_settings

SLACK, MAIL = "com.tinyspeck.slackmacgap", "com.apple.mail"
NOW = datetime.utcnow()


@pytest.fixture
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(writing_style, "_state", None)
    monkeypatch.setattr(correction_notes, "_state", None)
    monkeypatch.setattr(personal_examples, "_cache", None)
    monkeypatch.setattr(known_names, "_cache", None)
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", make)
    with make() as db:
        styles.ensure_styles(db)
    return make


def keep(storage, days):
    with storage() as db:
        save_settings(db, history_retention_days=days)


def add(storage, capture_id, days_ago, app=MAIL, words=5, source="dictation", said=None, meant=None):
    """A capture ``days_ago`` days old with an audio file, optionally corrected."""
    audio = config.get_captures_dir() / f"{capture_id}.wav"
    audio.write_bytes(b"RIFF")
    written = (NOW - timedelta(days=days_ago)).replace(tzinfo=UTC).timestamp()
    os.utime(audio, (written, written))
    refined = " ".join(["Tell", "Priya"] + ["word"] * (words - 2))
    with storage() as db:
        db.add(
            Capture(
                id=capture_id,
                audio_path=config.to_storage_path(audio),
                source=source,
                transcript_raw=said or " ".join(["um"] * (words + 1)),
                transcript_refined=refined,
                duration_ms=4000 + words * 100,
                app_bundle_id=app,
                app_name=app,
                created_at=NOW - timedelta(days=days_ago),
            )
        )
        if meant is not None:
            snapshot = {
                "transcript_raw": said,
                "transcript_refined": refined,
                "app_bundle_id": app,
                "audio_path": config.to_storage_path(audio),
            }
            db.add(
                CaptureFeedback(
                    capture_id=capture_id,
                    target="refined",
                    expected_text=meant,
                    snapshot=json.dumps(snapshot),
                    created_at=NOW - timedelta(days=days_ago),
                )
            )
        db.commit()
    personal_examples.invalidate()
    return audio


def ids(storage):
    with storage() as db:
        return {capture_id for (capture_id,) in db.query(Capture.id)}


def stats(storage):
    with storage() as db:
        return {period: usage_stats.usage_stats(db, period).model_dump() for period in usage_stats.PERIODS}


def test_retention_defaults_to_30_days(storage):
    with storage() as db:
        assert get_capture_settings(db).history_retention_days == 30


def test_upgrading_keeps_30_days_unconfirmed_and_detaches_nothing(tmp_path):
    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
        connection.execute(text("CREATE TABLE capture_feedback (id VARCHAR PRIMARY KEY, capture_id VARCHAR)"))
    run_migrations(engine)
    run_migrations(engine)
    with engine.connect() as connection:
        row = connection.execute(
            text("SELECT history_retention_days, history_retention_confirmed FROM capture_settings")
        ).one()
        assert tuple(row) == (30, 0)
        columns = {row[1] for row in connection.execute(text("PRAGMA table_info(capture_feedback)"))}
    assert {"app_bundle_id", "teaches_style_id"} <= columns


def test_old_captures_and_their_audio_go_and_their_corrections_keep_teaching(storage):
    with storage() as db:
        work = styles.create_style(db, "Work").id
        styles.assign_app(db, SLACK, "Slack", work)
    old_audio = add(storage, "old-fixed", 40, app=SLACK, said="ship it to voice box", meant="Ship it to Voicebox.")
    plain_audio = add(storage, "old-plain", 35)
    add(storage, "recent", 2)
    before_examples = personal_examples.all_examples(work)
    with storage() as db:
        before_counts = styles.app_corrections(db)
    keep(storage, 30)

    assert history_retention.sweep() == 2

    assert ids(storage) == {"recent"}
    assert not old_audio.exists()
    assert not plain_audio.exists()
    # The correction stays, pointing at its app, its style and its moved audio.
    with storage() as db:
        report = db.query(CaptureFeedback).one()
        assert (report.capture_id, report.app_bundle_id) == ("old-fixed", SLACK)
        moved = config.resolve_storage_path(json.loads(report.snapshot)["audio_path"])
        assert moved.parent.name == "correction-audio"
        assert moved.is_file()
        assert styles.app_corrections(db) == before_counts == {SLACK: 1}
    assert personal_examples.all_examples(work) == before_examples
    assert [e["said"] for e in before_examples] == ["ship it to voice box"]
    # Names they wrote are still known.
    assert "Priya" in known_names.known_names()


def test_a_left_correction_keeps_its_style_after_its_capture_goes(storage):
    with storage() as db:
        work = styles.create_style(db, "Work").id
        styles.assign_app(db, SLACK, "Slack", work)
    add(storage, "old", 40, app=SLACK, said="send the deck", meant="Send the deck.")
    with storage() as db:
        styles.assign_app(db, SLACK, "Slack", styles.default_id(), corrections="leave")
    keep(storage, 30)
    history_retention.sweep()
    personal_examples.invalidate()
    assert [e["said"] for e in personal_examples.all_examples(work)] == ["send the deck"]
    # Moving the app back and bringing corrections still reaches deleted captures' corrections.
    with storage() as db:
        styles.assign_app(db, SLACK, "Slack", work)
        styles.assign_app(db, SLACK, "Slack", styles.default_id(), corrections="bring")
    personal_examples.invalidate()
    assert personal_examples.all_examples(work) == []
    assert [e["said"] for e in personal_examples.all_examples(styles.default_id())] == ["send the deck"]


def test_stats_totals_stay_the_same_after_a_sweep(storage):
    keep(storage, 7)
    for days_ago, app, words in [(20, MAIL, 12), (10, SLACK, 7), (9, MAIL, 3), (3, SLACK, 9), (0, MAIL, 4)]:
        add(storage, f"c{days_ago}", days_ago, app=app, words=words)
    add(storage, "fixed", 8, said="a b", meant="A b.")
    add(storage, "command", 12, source="command")
    before = stats(storage)

    assert history_retention.sweep() == 5

    assert stats(storage) == before
    assert before["all"]["current"]["captures"] == 6
    assert before["all"]["current"]["fixed_captures"] == 1


def test_sweeping_again_changes_nothing(storage):
    keep(storage, 7)
    for days_ago in (30, 20, 10, 1):
        add(storage, f"c{days_ago}", days_ago)
    before = stats(storage)
    assert history_retention.sweep() == 3
    assert history_retention.sweep() == 0
    with storage() as db:
        assert db.query(RetiredCapture).count() == 3
        seen = db.get(KnownName, "Priya").last_seen_at
    assert seen == NOW - timedelta(days=10)
    assert stats(storage) == before


def test_a_crash_midway_loses_nothing_and_counts_nothing_twice(storage, monkeypatch):
    keep(storage, 7)
    audio = add(storage, "old", 30, said="the quick fox", meant="The quick fox.")
    add(storage, "new", 1)
    before = stats(storage)

    real = history_retention._fold

    def fold_then_crash(db, rows, *args):
        real(db, rows, *args)
        raise RuntimeError("power cut")

    monkeypatch.setattr(history_retention, "_fold", fold_then_crash)
    with pytest.raises(RuntimeError):
        history_retention.sweep()
    assert ids(storage) == {"old", "new"}
    assert audio.exists()
    with storage() as db:
        assert db.query(RetiredCapture).count() == 0
        assert db.query(KnownName).count() == 0
        assert db.query(CaptureFeedback).one().app_bundle_id is None
    assert stats(storage) == before

    monkeypatch.setattr(history_retention, "_fold", real)
    # A crash after the commit but before the audio went leaves the file behind.
    remove_audio = history_retention._remove_audio

    def crash(paths):
        raise RuntimeError("power cut")

    monkeypatch.setattr(history_retention, "_remove_audio", crash)
    with pytest.raises(RuntimeError):
        history_retention.sweep()
    assert ids(storage) == {"new"}
    assert audio.exists()
    monkeypatch.setattr(history_retention, "_remove_audio", remove_audio)
    assert history_retention.sweep() == 0
    assert not audio.exists()
    with storage() as db:
        assert db.query(RetiredCapture).count() == 1
    assert stats(storage) == before


def test_orphan_audio_of_a_dictation_in_progress_stays(storage):
    keep(storage, 7)
    add(storage, "old", 30)
    in_progress = config.get_captures_dir() / "streaming.wav"
    in_progress.write_bytes(b"RIFF")
    history_retention.sweep()
    assert in_progress.exists()


def test_keep_forever_deletes_nothing(storage):
    keep(storage, 0)
    for days_ago in (3000, 400, 1):
        add(storage, f"c{days_ago}", days_ago)
    with storage() as db:
        assert history_retention.count_expiring(db, 0) == 0
    assert history_retention.sweep() == 0
    assert ids(storage) == {"c3000", "c400", "c1"}


def test_the_current_take_and_captures_in_use_stay(storage):
    keep(storage, 7)
    add(storage, "busy", 50)
    add(storage, "old", 40)
    add(storage, "current", 30)
    with storage() as db:
        assert history_retention.count_expiring(db, 7) == 2
    with history_retention.in_use("busy"):
        assert history_retention.sweep() == 1
    assert ids(storage) == {"busy", "current"}
    assert history_retention.sweep() == 1
    assert ids(storage) == {"current"}


def test_the_worker_sweeps_when_the_server_saves_a_shorter_window(storage):
    for days_ago in (100, 60, 20, 5, 0):
        add(storage, f"c{days_ago}", days_ago)
    keep(storage, 30)
    app = FastAPI()
    from backend.routes.settings import router

    app.include_router(router)
    assert TestClient(app).post("/worker/history/sweep").json() == {"deleted": 2}
    assert ids(storage) == {"c20", "c5", "c0"}


def test_an_unconfirmed_window_deletes_nothing(storage):
    audio = add(storage, "old", 40)
    add(storage, "new", 1)
    assert history_retention.sweep() == 0
    assert ids(storage) == {"old", "new"}
    assert audio.exists()
    with storage() as db:
        assert history_retention.status(db) == (30, False, 1)


def test_confirming_the_window_lets_the_sweep_delete(storage):
    add(storage, "old", 40)
    add(storage, "new", 1)
    history_retention.sweep()
    with storage() as db:
        # Keeping the default, as the launch dialog's first choice does.
        assert save_settings(db, history_retention_days=30).history_retention_confirmed
    assert history_retention.sweep() == 1
    assert ids(storage) == {"new"}


def test_keep_forever_when_asked_confirms_and_deletes_nothing(storage):
    add(storage, "old", 400)
    add(storage, "new", 1)
    keep(storage, 0)
    with storage() as db:
        assert history_retention.status(db) == (0, True, 0)
    assert history_retention.sweep() == 0
    assert ids(storage) == {"old", "new"}


def test_nothing_to_delete_confirms_without_asking(storage):
    add(storage, "first", 3)
    with storage() as db:
        assert not get_capture_settings(db).history_retention_confirmed
    assert history_retention.sweep() == 0
    with storage() as db:
        assert get_capture_settings(db).history_retention_confirmed
    # Once confirmed, captures that age past the window go without asking.
    add(storage, "later", -40)
    assert history_retention.sweep(now=NOW + timedelta(days=40)) == 1
    assert ids(storage) == {"later"}
