"""Capture change notices for apps on other Macs (beta ``network``)."""

import asyncio
import json
import threading

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool

from backend.database.models import Base, Capture, CaptureFeedback, WritingStyle
from backend.services import capture_events
from backend.utils.macs import Mac


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


async def notices(stream, kind, wait_s=0.2) -> int:
    """How many ``kind`` events arrive within ``wait_s``."""
    count = 0
    while True:
        try:
            event = await asyncio.wait_for(anext(stream), timeout=wait_s)
        except TimeoutError:
            return count
        if event.startswith(f"event: {kind}"):
            count += 1


def listen(change, kind="changed"):
    """Subscribe, read the hello, run ``change``, then count the ``kind`` notices it caused."""

    async def run():
        stream = capture_events.subscribe()
        assert (await anext(stream)).startswith("event: changed")
        assert (await anext(stream)).startswith("event: learned")
        await asyncio.to_thread(change)
        try:
            return await notices(stream, kind)
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


def test_other_tables_say_learned_not_changed(db):
    def style():
        db.add(WritingStyle(name="Mine"))
        db.commit()

    assert listen(style) == 0
    assert listen(style, "learned") == 1


def test_a_correction_from_another_mac_says_learned(db):
    row = capture()
    db.add(row)
    db.commit()

    def correct():
        db.add(CaptureFeedback(capture_id=row.id, target="refined", expected_text="Or are we?", snapshot="{}"))
        db.commit()

    assert listen(correct, "learned") == 1


def test_learning_saved_outside_the_database_says_learned():
    assert listen(lambda: capture_events.publish("learned"), "learned") == 1


def test_rollbacks_stay_quiet(db):
    def rolled_back():
        db.add(capture())
        db.add(WritingStyle(name="Mine"))
        db.flush()
        db.rollback()

    assert listen(rolled_back) == 0
    assert listen(rolled_back, "learned") == 0


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


def test_peers_lists_each_mac_listening_and_drops_it_when_it_leaves():
    async def next_peers(stream):
        while True:
            event = await asyncio.wait_for(anext(stream), timeout=1)
            if event.startswith("event: peers"):
                return json.loads(event.split("data: ", 1)[1])

    async def run():
        studio = capture_events.subscribe(Mac("studio", "Studio"))
        assert (await anext(studio)).startswith("event: changed")
        assert await next_peers(studio) == [{"id": "studio", "name": "Studio"}]

        laptop = capture_events.subscribe(Mac("laptop", "Morgan’s MacBook"))
        await anext(laptop)
        both = [{"id": "laptop", "name": "Morgan’s MacBook"}, {"id": "studio", "name": "Studio"}]
        assert await next_peers(laptop) == both
        assert await next_peers(studio) == both

        await laptop.aclose()
        assert await next_peers(studio) == [{"id": "studio", "name": "Studio"}]
        await studio.aclose()

    asyncio.run(run())
