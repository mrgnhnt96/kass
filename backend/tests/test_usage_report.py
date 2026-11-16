"""Daily usage reports: which days go, catching up missed days, opting out and back in."""

import time
from datetime import date, datetime, timedelta

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool

from backend.database.models import Base, Capture, TakeReport
from backend.services import settings as settings_service, usage_report
from backend.tests.settings_rows import save_settings

# Thursday, Oct 1 2026, in Denver (MDT, UTC-6).
TODAY = date(2026, 10, 1)


@pytest.fixture(autouse=True)
def denver(monkeypatch):
    monkeypatch.setenv("TZ", "America/Denver")
    time.tzset()
    monkeypatch.setattr(usage_report, "machine", lambda: {"chip": "Apple M2", "memory_gb": 16, "os_version": "26.0"})
    monkeypatch.setattr(usage_report, "_voice_training_on", lambda: False)
    yield
    monkeypatch.undo()
    time.tzset()


@pytest.fixture
def db():
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        save_settings(session, onboarding_completed=True)
        yield session
    engine.dispose()


class Sink:
    def __init__(self, ok=True):
        self.ok = ok
        self.batches: list[list[dict]] = []

    def __call__(self, events):
        self.batches.append(events)
        return self.ok

    @property
    def events(self):
        return [e for batch in self.batches for e in batch]


def dictate(db, utc: datetime, words=10, duration_ms=6000, text="word"):
    db.add(
        Capture(
            audio_path="captures/x.wav",
            source="dictation",
            transcript_raw=" ".join([text] * words),
            transcript_refined=" ".join([text] * words),
            duration_ms=duration_ms,
            app_bundle_id="com.apple.mail",
            app_name="Mail",
            created_at=utc,
        )
    )
    db.commit()


def take(db, utc: datetime, outcome="delivered", latency_ms=400, mode="dictation"):
    db.add(TakeReport(created_at=utc, mode=mode, outcome=outcome, latency_ms=latency_ms))
    db.commit()


def test_sends_each_finished_day_with_its_numbers(db):
    # 9 pm Sep 29 local is 3 am Sep 30 UTC: it belongs to Sep 29.
    dictate(db, datetime(2026, 9, 30, 3, 0), words=12)
    dictate(db, datetime(2026, 9, 30, 16, 0), words=40)
    take(db, datetime(2026, 9, 30, 16, 0), latency_ms=300)
    take(db, datetime(2026, 9, 30, 16, 5), latency_ms=500)
    take(db, datetime(2026, 9, 30, 16, 6), outcome="failed", latency_ms=None)
    # Today isn't finished: it waits for tomorrow.
    dictate(db, datetime(2026, 10, 1, 16, 0), words=99)
    sink = Sink()

    assert usage_report.report(db, today=TODAY, post=sink) == 2

    by_day = {e["event_properties"]["day"]: e for e in sink.events}
    assert sorted(by_day) == ["2026-09-29", "2026-09-30"]
    sep30 = by_day["2026-09-30"]
    assert sep30["event_type"] == "daily_usage"
    assert sep30["event_properties"]["words"] == 40
    assert sep30["event_properties"]["dictations"] == 1
    assert sep30["event_properties"]["speaking_seconds"] == 6
    assert sep30["event_properties"]["pace_wpm"] == 400
    assert sep30["event_properties"]["delivered_takes"] == 2
    assert sep30["event_properties"]["failed_takes"] == 1
    assert sep30["event_properties"]["latency_p50_ms"] == 400
    assert sep30["insert_id"] == f"{sep30['device_id']}-2026-09-30"
    assert sep30["user_properties"]["chip"] == "Apple M2"
    assert "ip" not in sep30
    assert settings_service.get_capture_settings(db).usage_sent_through == "2026-09-30"


def test_never_sends_text_or_app_names(db):
    dictate(db, datetime(2026, 9, 30, 16, 0), text="secretword")
    sink = Sink()
    usage_report.report(db, today=TODAY, post=sink)
    sent = repr(sink.events)
    assert "secretword" not in sent
    assert "Mail" not in sent
    assert "com.apple.mail" not in sent


def test_catches_up_days_missed_while_away(db):
    dictate(db, datetime(2026, 9, 20, 16, 0))
    usage_report.report(db, today=date(2026, 9, 21), post=Sink())
    # Kass wasn't running from the 22nd to the 30th; dictation on two of those days
    # (made through the batch path or an import) still counts.
    dictate(db, datetime(2026, 9, 23, 16, 0))
    dictate(db, datetime(2026, 9, 27, 16, 0))
    sink = Sink()

    usage_report.report(db, today=TODAY, post=sink)

    assert [e["event_properties"]["day"] for e in sink.events] == ["2026-09-23", "2026-09-27"]
    assert settings_service.get_capture_settings(db).usage_sent_through == "2026-09-30"


def test_a_failed_upload_is_retried_next_time(db):
    dictate(db, datetime(2026, 9, 30, 16, 0))
    usage_report.report(db, today=TODAY, post=Sink(ok=False))
    assert settings_service.get_capture_settings(db).usage_sent_through is None

    sink = Sink()
    usage_report.report(db, today=TODAY, post=sink)
    assert len(sink.events) == 1


def test_waits_for_onboarding(db):
    save_settings(db, onboarding_completed=False)
    dictate(db, datetime(2026, 9, 30, 16, 0))
    sink = Sink()
    assert usage_report.report(db, today=TODAY, post=sink) == 0
    assert sink.batches == []


def test_days_with_sharing_off_are_never_sent(db):
    save_settings(db, share_usage=False)
    dictate(db, datetime(2026, 9, 29, 16, 0))
    take(db, datetime(2026, 9, 29, 16, 0))
    sink = Sink()
    usage_report.report(db, today=TODAY, post=sink)
    assert sink.batches == []
    # Take reports from days that will never be sent aren't kept.
    assert db.query(TakeReport).count() == 0

    # Turned back on today: the server marks yesterday sent, so only today onward goes.
    save_settings(db, share_usage=True, usage_sent_through=(TODAY - timedelta(days=1)).isoformat())
    dictate(db, datetime(2026, 10, 1, 16, 0))
    usage_report.report(db, today=date(2026, 10, 2), post=sink)
    assert [e["event_properties"]["day"] for e in sink.events] == ["2026-10-01"]


def test_long_catch_up_goes_in_batches(db, monkeypatch):
    monkeypatch.setattr(usage_report, "BATCH", 2)
    for day in (25, 26, 27, 28, 29):
        dictate(db, datetime(2026, 9, day, 16, 0))
    sink = Sink()
    assert usage_report.report(db, today=TODAY, post=sink) == 5
    assert len(sink.batches) == 3


def test_device_id_is_random_and_kept(db):
    dictate(db, datetime(2026, 9, 29, 16, 0))
    dictate(db, datetime(2026, 9, 30, 16, 0))
    sink = Sink()
    usage_report.report(db, today=date(2026, 9, 30), post=sink)
    usage_report.report(db, today=TODAY, post=sink)
    ids = {e["device_id"] for e in sink.events}
    assert len(ids) == 1
    assert len(ids.pop()) == 36


def test_sent_take_reports_are_deleted(db):
    take(db, datetime(2026, 9, 30, 16, 0))
    take(db, datetime(2026, 10, 1, 16, 0))
    usage_report.report(db, today=TODAY, post=Sink())
    assert [t.created_at.day for t in db.query(TakeReport)] == [1]


def test_dictionary_size_is_a_range():
    assert usage_report._dictionary_bucket(0) == "0"
    assert usage_report._dictionary_bucket(7) == "1-10"
    assert usage_report._dictionary_bucket(500) == "200+"


def test_dev_servers_do_not_send(monkeypatch):
    monkeypatch.delenv("KASS_USAGE_REPORTS", raising=False)
    monkeypatch.delattr("sys.frozen", raising=False)
    assert not usage_report.sending_allowed()
    monkeypatch.setenv("KASS_USAGE_REPORTS", "1")
    assert usage_report.sending_allowed()
    monkeypatch.setattr("sys.frozen", True, raising=False)
    monkeypatch.setenv("KASS_USAGE_REPORTS", "0")
    assert not usage_report.sending_allowed()


def test_the_exact_payload_is_logged_before_sending(monkeypatch, caplog):
    import json
    import logging

    posted = []

    class Response:
        status_code = 200
        text = ""

    def fake_post(url, json, timeout):
        posted.append(json)
        return Response()

    monkeypatch.setattr(usage_report.httpx, "post", fake_post)
    events = [{"event_type": "daily_usage", "event_properties": {"words": 10}}]
    with caplog.at_level(logging.INFO, logger=usage_report.__name__):
        assert usage_report._post(events)

    logged = next(r.getMessage() for r in caplog.records if r.getMessage().startswith("Sending usage"))
    assert json.loads(logged.split(": ", 1)[1]) == posted[0]["events"]
    assert usage_report.API_KEY not in logged


def test_run_once_opens_the_session_init_db_bound_after_import(monkeypatch):
    """init_db rebinds SessionLocal after this module imports it (it starts as None)."""
    from sqlalchemy.orm import sessionmaker

    from backend.database import session as database_session

    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    monkeypatch.setattr(database_session, "SessionLocal", sessionmaker(bind=engine))
    monkeypatch.setattr(usage_report, "sending_allowed", lambda: True)
    opened = []
    monkeypatch.setattr(usage_report, "report", lambda db: opened.append(db) or 0)

    usage_report.run_once()

    assert len(opened) == 1
    engine.dispose()
