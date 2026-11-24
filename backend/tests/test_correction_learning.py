"""The worker reads the learned corrections the server keeps, for model improvement."""

import json

import pytest

from backend import config
from backend.services import correction_learning as learning

RULE = {"id": "r1", "match": "voice box", "replacement": "Voicebox"}


@pytest.fixture
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(learning, "_state", None)
    monkeypatch.setattr(learning, "_path", None)
    return tmp_path


def test_reload_reads_the_servers_rules(storage):
    learning.initialize()
    assert learning._state["rules"] == []
    state = {**learning._empty(), "revision": 1, "rules": [RULE], "outcome": "updated"}
    (storage / "correction-learning.json").write_text(json.dumps(state))
    status = learning.reload()
    assert status["active_rules"] == 1
    assert status["revision"] == 1
    assert learning._state["rules"] == [RULE]


@pytest.mark.parametrize("text", ["{broken", json.dumps({"version": 2, "rules": []}), json.dumps({"version": 1})])
def test_unreadable_state_falls_back_to_no_rules(storage, text):
    (storage / "correction-learning.json").write_text(text)
    assert learning.status()["active_rules"] == 0
    assert learning.reload()["active_rules"] == 0
