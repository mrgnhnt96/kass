"""Capture change notices for apps on other Macs (beta ``network``)."""

import asyncio
import threading

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool

from backend.database.models import Base, Capture, WritingStyle
from backend.services import capture_events


@pytest.fixture
def db():
    capture_events.install()
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        yield session
    engine.dispose()


def capture(**fields):
    return Capture(audio_path="captures/x.wav", source="dictation", transcript_raw="hi", **fields)


async def notices(stream, wait_s=0.2) -> int:
    """How many ``changed`` events arrive within ``wait_s``."""
    count = 0
    while True:
        try:
            event = await asyncio.wait_for(anext(stream), timeout=wait_s)
        except TimeoutError:
            return count
        if event.startswith("event: changed"):
            count += 1


def listen(change):
    """Subscribe, read the hello, run ``change``, then count the notices it caused."""

    async def run():
        stream = capture_events.subscribe()
        assert (await anext(stream)).startswith("event: changed")
        await asyncio.to_thread(change)
        try:
            return await notices(stream)
        finally:
            await stream.aclose()

    return asyncio.run(run())


def test_insert_update_and_delete_each_notify(db):
    row = capture()

    def add():
        db.add(row)
        db.commit()

    assert listen(add) == 1

    def edit():
        row.transcript_refined = "Hi."
        db.commit()

    assert listen(edit) == 1

    def remove():
        db.delete(row)
        db.commit()

    assert listen(remove) == 1


def test_bulk_delete_notifies(db):
    db.add_all([capture(), capture()])
    db.commit()

    def purge():
        db.query(Capture).delete(synchronize_session=False)
        db.commit()

    assert listen(purge) == 1


def test_other_tables_and_rollbacks_stay_quiet(db):
    def style():
        db.add(WritingStyle(name="Mine"))
        db.commit()

    assert listen(style) == 0

    def rolled_back():
        db.add(capture())
        db.flush()
        db.rollback()

    assert listen(rolled_back) == 0


def test_commit_from_another_thread_reaches_the_stream(db):
    def add():
        worker = threading.Thread(target=lambda: (db.add(capture()), db.commit()))
        worker.start()
        worker.join()

    assert listen(add) == 1


def test_listener_leaves_when_the_stream_closes():
    async def run():
        stream = capture_events.subscribe()
        await anext(stream)
        assert len(capture_events._listeners) == 1
        await stream.aclose()
        return len(capture_events._listeners)

    assert asyncio.run(run()) == 0
