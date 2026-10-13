"""Which Mac a request came from, and stamping it on captures (beta ``network``)."""

from contextlib import contextmanager

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import Session
from sqlalchemy.pool import StaticPool
from starlette.applications import Starlette
from starlette.responses import JSONResponse
from starlette.routing import Route, WebSocketRoute
from starlette.testclient import TestClient

from backend.database.models import Base, Capture
from backend.utils import macs
from backend.utils.macs import Mac, MacMiddleware, current_mac, mac_of


def scope(headers=(), query=b""):
    return {"headers": [(k.encode(), v.encode()) for k, v in headers], "query_string": query}


@contextmanager
def requested_by(mac):
    reset = current_mac.set(mac)
    try:
        yield
    finally:
        current_mac.reset(reset)


def test_reads_headers_with_a_percent_encoded_name():
    found = mac_of(scope([("X-Kass-Mac", "a1-b2"), ("X-Kass-Mac-Name", "Morgan%E2%80%99s%20MacBook")]))
    assert found == Mac("a1-b2", "Morgan’s MacBook")


def test_reads_query_parameters_where_headers_cant_be_set():
    found = mac_of(scope(query=b"mac=a1-b2&mac_name=Morgan%E2%80%99s%20MacBook"))
    assert found == Mac("a1-b2", "Morgan’s MacBook")


@pytest.mark.parametrize("mac_id", ["", "has space", "x" * 65, "semi;colon"])
def test_ignores_an_id_that_isnt_one(mac_id):
    assert mac_of(scope([("X-Kass-Mac", mac_id)])) is None


def test_a_blank_name_is_none():
    assert mac_of(scope([("X-Kass-Mac", "a1"), ("X-Kass-Mac-Name", "  ")])) == Mac("a1", None)


def test_middleware_sets_the_mac_for_requests_and_websockets():
    async def http(_request):
        mac = current_mac.get()
        return JSONResponse(mac and {"id": mac.id, "name": mac.name})

    async def ws(websocket):
        await websocket.accept()
        mac = current_mac.get()
        await websocket.send_json(mac and {"id": mac.id, "name": mac.name})
        await websocket.close()

    app = Starlette(routes=[Route("/", http), WebSocketRoute("/ws", ws)])
    app.add_middleware(MacMiddleware)
    client = TestClient(app)

    response = client.get("/", headers={"X-Kass-Mac": "a1", "X-Kass-Mac-Name": "Studio"})
    assert response.json() == {"id": "a1", "name": "Studio"}
    assert client.get("/").json() is None
    with client.websocket_connect("/ws?mac=b2&mac_name=Laptop") as socket:
        assert socket.receive_json() == {"id": "b2", "name": "Laptop"}


@pytest.fixture
def db():
    macs.install()
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    with Session(engine) as session:
        yield session
    engine.dispose()


def capture():
    return Capture(audio_path="", source="dictation", transcript_raw="hi")


def test_new_captures_get_the_requesting_mac(db):
    row = capture()
    with requested_by(Mac("a1", "Studio")):
        db.add(row)
        db.commit()
    assert (row.mac_id, row.mac_name) == ("a1", "Studio")


def test_outside_a_request_and_on_edits_nothing_is_stamped(db):
    row = capture()
    db.add(row)
    db.commit()
    assert row.mac_id is None

    with requested_by(Mac("a1", "Studio")):
        row.transcript_refined = "Hi."
        db.commit()
    assert row.mac_id is None
