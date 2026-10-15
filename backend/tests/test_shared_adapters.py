"""The adapters Kass ships: used until a personal one is active, and personal
training continues from them (docs/plans/SHARED_ADAPTERS.md)."""

import json

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from backend import config
from backend.database.models import Base
from backend.services import shared_adapters
from backend.services.model_improvement import manager
from backend.services.model_improvement.data import digest
from backend.services.refinement import RefinementFlags
from backend.services.voice_training import lora

DEFAULT = RefinementFlags().to_dict()


@pytest.fixture
def learning(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    engine = create_engine(f"sqlite:///{tmp_path}/learning.sqlite")
    Base.metadata.create_all(engine)
    monkeypatch.setattr(manager.database_session, "SessionLocal", sessionmaker(bind=engine))
    for name, value in (("_state", None), ("_root", None), ("_active", {}), ("_process", None), ("_thread", None)):
        monkeypatch.setattr(manager, name, value)
    monkeypatch.setattr(manager.correction_learning, "_state", {"rules": []})
    monkeypatch.setattr(manager, "_reload_speech", lambda: None)
    manager.initialize()
    return tmp_path


@pytest.fixture
def shipped(tmp_path, monkeypatch):
    """A build with a shared 1.7B cleanup adapter and a shared voice adapter."""
    root = tmp_path / "shared-adapters"
    for name, item in (
        ("cleanup-1.7B", {"id": "cleanup-one", "pipeline": manager.pipeline_id(), "tested_flags": [DEFAULT]}),
        ("voice-turbo", {"id": "voice-one"}),
    ):
        (root / name).mkdir(parents=True)
        (root / name / shared_adapters.MANIFEST).write_text(json.dumps(item))
    monkeypatch.setattr(shared_adapters, "ROOT", root)
    monkeypatch.setattr(shared_adapters, "_enabled", True)
    monkeypatch.setattr(shared_adapters, "_broken", set())
    shared_adapters.manifest.cache_clear()
    yield root
    shared_adapters.manifest.cache_clear()


def personal_llm(path, shared=None):
    manager._active["llm"] = {
        "id": "personal",
        "path": str(path),
        "model_size": "1.7B",
        "rules_digest": digest([]),
        "tested_flags": [DEFAULT],
        "shared": shared,
    }


def test_cleanup_uses_the_shared_adapter_until_a_personal_one_is_active(learning, shipped):
    shared = str(shipped / "cleanup-1.7B")
    assert manager.active_adapter("1.7B", DEFAULT) == shared
    # Only for the model size and flags it was tested with.
    assert manager.active_adapter("4B", DEFAULT) is None
    assert manager.active_adapter("1.7B", RefinementFlags(self_correction=False).to_dict()) is None
    personal_llm(learning / "personal")
    assert manager.active_adapter("1.7B", DEFAULT) == str(learning / "personal")


def test_a_shared_adapter_tested_with_other_cleanup_code_is_never_used(learning, shipped):
    (shipped / "cleanup-1.7B" / shared_adapters.MANIFEST).write_text(
        json.dumps({"id": "cleanup-one", "pipeline": "older", "tested_flags": [DEFAULT]})
    )
    shared_adapters.manifest.cache_clear()
    assert manager.active_adapter("1.7B", DEFAULT) is None


def test_turning_it_off_uses_stock_models_and_stock_built_personal_ones(learning, shipped):
    shared_adapters._enabled = False
    assert manager.active_adapter("1.7B", DEFAULT) is None
    assert manager.voice_adapter("turbo") is None
    personal_llm(learning / "built-on-shared", shared="cleanup-one")
    assert manager.active_adapter("1.7B", DEFAULT) is None
    personal_llm(learning / "built-on-stock")
    assert manager.active_adapter("1.7B", DEFAULT) == str(learning / "built-on-stock")


def test_the_setting_switches_adapters_right_away(learning, shipped, monkeypatch):
    from backend.services.settings import update_capture_settings

    reloads = []
    monkeypatch.setattr(manager, "_reload_speech", lambda: reloads.append(True))
    with manager.database_session.SessionLocal() as db:
        update_capture_settings(db, {"shared_adapters": False})
    assert manager.voice_adapter("turbo") is None
    assert reloads
    with manager.database_session.SessionLocal() as db:
        update_capture_settings(db, {"shared_adapters": True})
    assert manager.voice_adapter("turbo") == str(shipped / "voice-turbo")


def test_a_shared_voice_adapter_that_fails_to_load_is_set_aside(learning, shipped):
    path = str(shipped / "voice-turbo")
    assert manager.voice_adapter("turbo") == path
    manager.quarantine_voice("failed", path)
    assert manager.voice_adapter("turbo") is None


def voice_plan(monkeypatch):
    from backend.services.voice_training import bank, sounds

    monkeypatch.setattr(bank, "takes", list)
    monkeypatch.setattr(bank, "room_paths", list)
    monkeypatch.setattr(bank, "digest", lambda: "bank")
    monkeypatch.setattr(sounds, "ready", lambda: True)
    monkeypatch.setattr(manager, "_cached", lambda repo: True)
    return manager._voice_plan({"train": 200, "test": 20}, manager._active, [], None)


def trained_voice(folder, shared=None):
    (folder / "voice").mkdir(parents=True)
    (folder / "voice" / lora.ADAPTER_FILE).write_bytes(b"weights")
    (folder / "voice" / lora.CONFIG_FILE).write_text(json.dumps({"model_size": "turbo", "shared": shared}))
    return str(folder / "voice")


def test_voice_training_starts_from_the_shared_adapter(learning, shipped, monkeypatch):
    shared = str(shipped / "voice-turbo")
    plan = voice_plan(monkeypatch)
    assert (plan["resume"], plan["production"], plan["shared"]) == (shared, shared, "voice-one")

    # A voice model trained on plain turbo stays in use, but training moves to the shared one.
    runs = learning / "model-improvement" / "runs"
    stock = trained_voice(runs / "stock")
    manager._active["voice"] = {"path": stock, "model_size": "turbo", "shared": None}
    plan = voice_plan(monkeypatch)
    assert (plan["resume"], plan["production"], plan["shared"]) == (shared, stock, "voice-one")

    # A candidate already built on the shared one is continued.
    candidate = trained_voice(runs / "candidate", shared="voice-one")
    manager._state["voice_resume"] = candidate
    monkeypatch.setattr(manager, "_still_worth_continuing", lambda path: True)
    plan = voice_plan(monkeypatch)
    assert (plan["resume"], plan["production"], plan["shared"]) == (candidate, stock, "voice-one")


def test_voice_training_drops_a_resume_point_that_no_longer_passes(learning, shipped, monkeypatch):
    shared = str(shipped / "voice-turbo")
    runs = learning / "model-improvement" / "runs"
    candidate = trained_voice(runs / "candidate", shared="voice-one")
    manager._state["voice_resume"] = candidate
    monkeypatch.setattr(manager, "_still_worth_continuing", lambda path: False)
    assert voice_plan(monkeypatch)["resume"] == shared

    monkeypatch.setattr(manager, "_still_worth_continuing", lambda path: path == candidate)
    assert voice_plan(monkeypatch)["resume"] == candidate


def test_voice_training_without_shared_adapters_starts_from_plain_turbo(learning, shipped, monkeypatch):
    shared_adapters._enabled = False
    runs = learning / "model-improvement" / "runs"
    manager._state["voice_resume"] = trained_voice(runs / "candidate", shared="voice-one")
    plan = voice_plan(monkeypatch)
    assert (plan["resume"], plan["production"], plan["shared"]) == (None, None, None)


def prepare(monkeypatch):
    from types import SimpleNamespace

    from backend.services import settings as settings_service, styles

    monkeypatch.setattr(manager.correction_learning, "run_job", lambda: None)
    monkeypatch.setattr(manager, "collect", lambda db, groups: [])
    monkeypatch.setattr(manager, "readiness", lambda samples: ({"audio_test": 0}, True))
    monkeypatch.setattr(manager, "_cached", lambda repo: "/cached")
    monkeypatch.setattr(
        settings_service,
        "get_capture_settings",
        lambda db: SimpleNamespace(llm_model="1.7B", stt_model="base", language="auto", discard_audio=False),
    )
    monkeypatch.setattr(styles, "load", lambda db: SimpleNamespace(styles=[]))
    monkeypatch.setattr(manager, "_scan_bank", lambda db, settings: None)
    plan, _, _ = manager._prepare()
    return plan["start_adapter"], plan["baseline_adapter"], plan["shared"]


def test_cleanup_training_starts_from_the_shared_adapter(learning, shipped, monkeypatch):
    shared = str(shipped / "cleanup-1.7B")
    assert prepare(monkeypatch) == (shared, shared, "cleanup-one")
    # A personal adapter trained on the stock model stays the one to beat.
    personal_llm(learning / "stock")
    assert prepare(monkeypatch) == (shared, str(learning / "stock"), "cleanup-one")
    # One already built on this shared adapter is continued.
    personal_llm(learning / "built-on-shared", shared="cleanup-one")
    assert prepare(monkeypatch) == (str(learning / "built-on-shared"),) * 2 + ("cleanup-one",)
    # Off: training starts from the stock model.
    shared_adapters._enabled = False
    assert prepare(monkeypatch) == (None, None, None)
