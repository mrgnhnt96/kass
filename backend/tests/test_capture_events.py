"""The worker's own change notices, which the Rust server passes on to the apps."""

import asyncio
import threading

from backend.services import capture_events


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


def listen(change, kind="learned"):
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


def test_learning_saved_says_learned():
    assert listen(lambda: capture_events.publish("learned")) == 1
    assert listen(lambda: capture_events.publish("learned"), "changed") == 0


def test_pending_notices_collapse():
    def twice():
        capture_events.publish("learned")
        capture_events.publish("learned")

    assert listen(twice) == 1


def test_a_publish_from_another_thread_reaches_the_stream():
    def publish():
        worker = threading.Thread(target=capture_events.publish, args=("learned",))
        worker.start()
        worker.join()

    assert listen(publish) == 1


def test_listener_leaves_when_the_stream_closes():
    async def run():
        stream = capture_events.subscribe()
        await anext(stream)
        assert len(capture_events._listeners) == 1
        await stream.aclose()
        return len(capture_events._listeners)

    assert asyncio.run(run()) == 0
