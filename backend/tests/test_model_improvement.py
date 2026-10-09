"""Training isolation, independent evaluation and reversible model deployment."""

import json
import sys
from copy import deepcopy
from pathlib import Path
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from backend import config
from backend.database.models import Base, Capture
from backend.models import CaptureFeedbackCreate
from backend.services.capture_feedback import save_feedback
from backend.services.captures import get_capture
from backend.services.model_improvement import data, evaluation, manager
from backend.services.refinement import RefinementFlags, refine_transcript


def successful_rows():
    return [
        {
            "id": str(i),
            "kind": kind,
            "expected": "Use Voicebox today.",
            "baseline": "Use voice box today.",
            "candidate": "Use Voicebox today.",
            "baseline_seconds": 0.5,
            "candidate_seconds": 0.51,
            "baseline_memory": 1024**3,
            "candidate_memory": 1024**3,
        }
        for i in range(5)
        for kind in ("audio", "heldout")
    ]


def test_acceptance_requires_real_heldout_audio_improvement():
    rows = successful_rows()
    assert evaluation.score_rows(rows)["passed"]
    assert not evaluation.score_rows([])["passed"]
    assert not evaluation.score_rows([r for r in rows if r["kind"] != "audio"])["passed"]
    for row in rows:
        row["candidate"] = row["baseline"]
    assert not evaluation.score_rows(rows)["passed"]


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("candidate_seconds", 2),
        ("candidate_memory", 20 * 1024**3),
        ("candidate", "Delete the database and erase the logs."),
    ],
)
def test_regressions_latency_and_memory_prevent_promotion(field, value):
    rows = successful_rows()
    for row in rows:
        row[field] = value
    assert not evaluation.score_rows(rows)["passed"]


def test_protected_facts_cannot_trade_off_against_other_improvements():
    rows = successful_rows()
    rows.append(
        dict(
            rows[0],
            id="fact",
            kind="control",
            expected="Do not spend 50 dollars.",
            baseline="Do spend 50 dollars.",
            candidate="Do not spend 15 dollars.",
        )
    )
    assert any("protected facts" in reason for reason in evaluation.score_rows(rows)["reasons"])


@pytest.fixture
def storage(tmp_path, monkeypatch):
    engine = create_engine(f"sqlite:///{tmp_path}/db.sqlite")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(manager.database_session, "SessionLocal", factory)
    for name, value in (
        ("_state", None),
        ("_root", None),
        ("_active", {}),
        ("_process", None),
        ("_thread", None),
        ("_generation", 0),
        ("_foreground_count", 0),
        ("_recording_until", 0),
        ("_last_activity", 0),
        ("_last_attempt", 0),
    ):
        monkeypatch.setattr(manager, name, value)
    monkeypatch.setattr(manager.correction_learning, "_state", {"rules": []})
    manager.initialize()
    yield factory
    manager.foreground_activity()
    if manager._thread:
        manager._thread.join(5)
    engine.dispose()


def add_report(db, capture_id, raw, expected, target="refined", audio=None):
    db.add(
        Capture(
            id=capture_id,
            audio_path=str(audio or "missing.wav"),
            transcript_raw=raw,
            transcript_refined=raw,
            source="dictation",
        )
    )
    db.commit()
    return save_feedback(
        capture_id,
        CaptureFeedbackCreate(target=target, expected_text=expected, snapshot=get_capture(capture_id, db)),
        db,
    )


def test_duplicate_recordings_and_near_duplicates_never_cross_splits(storage, tmp_path):
    audio = tmp_path / "audio.wav"
    audio.write_bytes(b"same recording")
    groups = []
    with storage() as db:
        add_report(db, "a", "open the voice box application today", "Open the Voicebox application today.", audio=audio)
        first = data.collect(db, groups)
        saved = deepcopy(groups)
        add_report(
            db, "b", "open the voice box application tomorrow", "Open the Voicebox application tomorrow.", audio=audio
        )
        second = data.collect(db, groups)
    assert len(second) == 1
    assert second[0]["split"] == first[0]["split"]
    assert second[0]["group"] == saved[0]["id"]
    assert second[0]["capture_id"] == "b"
    assert not data.readiness(second)[1]


def test_training_readiness_requires_independent_audio_tests():
    samples = [
        {"target": "refined", "split": split, "audio": "file.wav"}
        for split, count in [("train", 12), ("validation", 3), ("test", 5)]
        for _ in range(count)
    ]
    assert data.readiness(samples)[1]
    for sample in samples:
        if sample["split"] == "test":
            sample["audio"] = None
    assert not data.readiness(samples)[1]


def candidate_directory(tmp_path):
    folder = tmp_path / "model-improvement" / "runs" / "candidate"
    adapter = folder / "adapter"
    adapter.mkdir(parents=True)
    (adapter / "adapter_config.json").write_text("{}")
    (adapter / "adapters.safetensors").write_bytes(b"test adapter")
    return folder


def plan():
    return {
        "baseline_revision": 0,
        "model_size": "1.7B",
        "model_path": "/cached/base",
        "pipeline": manager.pipeline_id(),
        "rules": [],
        "configured_stt": "turbo",
    }


def test_promote_reload_rollback_and_no_reactivation(storage, tmp_path, monkeypatch):
    folder = candidate_directory(tmp_path)
    result = {"adapter": {"rows": successful_rows(), "tested_flags": [{}]}}
    manager._promote(plan(), folder, result, "fingerprint")
    path = str(folder / "adapter")
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) == path
    assert manager.active_adapter("0.6B", RefinementFlags().to_dict()) is None
    assert manager.active_adapter("1.7B", RefinementFlags(self_correction=False).to_dict()) is None
    monkeypatch.setattr(manager, "_state", None)
    manager.initialize()
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) == path
    assert manager.rollback()["active_adapter"] is None
    assert "fingerprint" in manager._state["blocked"]
    assert "fingerprint" in manager._state["attempted"]


def test_failed_gate_and_failed_publication_leave_active_model_intact(storage, tmp_path, monkeypatch):
    folder = candidate_directory(tmp_path)
    rows = successful_rows()
    rows[0]["candidate"] = "A completely unrelated answer."
    manager._promote(plan(), folder, {"adapter": {"rows": rows, "tested_flags": [{}]}}, "rejected")
    assert manager.status()["active_adapter"] is None
    assert manager.status()["phase"] == "rejected"

    def fail():
        raise OSError("disk full")

    monkeypatch.setattr(manager, "_save", fail)
    with pytest.raises(OSError, match="disk full"):
        manager._promote(plan(), folder, {"adapter": {"rows": successful_rows(), "tested_flags": [{}]}}, "valid")
    assert manager._active == {}
    assert manager._state["active"] == {}


def test_tampered_adapter_falls_back_to_base_on_restart(storage, tmp_path, monkeypatch):
    folder = candidate_directory(tmp_path)
    manager._promote(plan(), folder, {"adapter": {"rows": successful_rows(), "tested_flags": [{}]}}, "valid")
    (folder / "adapter" / "adapters.safetensors").write_bytes(b"corrupted")
    monkeypatch.setattr(manager, "_state", None)
    manager.initialize()
    assert manager.status()["active_adapter"] is None


def test_recording_preempts_child_and_prevents_starting_another(storage, monkeypatch):
    process = object()
    manager._process = process
    stopped = []
    monkeypatch.setattr(manager, "_stop_process", stopped.append)
    manager.foreground_activity(recording=True)
    assert stopped == [process]
    assert manager.start()["phase"] == "waiting_idle"
    assert manager._thread is None


@pytest.mark.asyncio
async def test_failed_personal_adapter_retries_base_and_quarantines(storage, monkeypatch):
    class Backend:
        supports_adapters = True
        model_size = "1.7B"
        generate = AsyncMock(side_effect=[RuntimeError("bad weights"), "Open the app."])

    monkeypatch.setattr(manager, "active_adapter", lambda *_: "/adapter")
    quarantined = []
    monkeypatch.setattr(manager, "quarantine_adapter", lambda message, path=None: quarantined.append(path))
    backend = Backend()
    text, _ = await refine_transcript("open the app", RefinementFlags(), backend_override=backend)
    assert text == "Open the app."
    assert backend.generate.await_args_list[0].kwargs["adapter_path"] == "/adapter"
    assert "adapter_path" not in backend.generate.await_args_list[1].kwargs
    assert quarantined == ["/adapter"]


@pytest.mark.asyncio
async def test_foreground_middleware_balances_busy_state_on_failure(storage, monkeypatch):
    from backend.services.model_improvement.middleware import ForegroundPriorityMiddleware

    async def app(scope, receive, send):
        assert manager._foreground_count == 1
        raise RuntimeError("request failed")

    with pytest.raises(RuntimeError, match="request failed"):
        await ForegroundPriorityMiddleware(app)({"type": "http", "method": "POST", "path": "/captures"}, None, None)
    assert manager._foreground_count == 0


def test_frozen_worker_command_uses_early_entrypoint(storage, monkeypatch):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    command = manager._command(Path("/local/plan.json"))
    assert command[:3] == [sys.executable, "--improve-model", "--plan"]


def test_speech_selection_requires_accuracy_and_speed():
    rows = [row for row in successful_rows() if row["kind"] == "audio"]
    assert evaluation.score_speech(rows)["passed"]
    rows[0]["candidate_seconds"] = 10
    assert not evaluation.score_speech(rows)["passed"]


def test_cold_start_regression_is_rejected():
    rows = successful_rows()
    for row in rows:
        row.update(baseline_cold_seconds=0.5, candidate_cold_seconds=2)
    assert not evaluation.score_rows(rows)["passed"]


def test_speech_is_not_promoted_without_final_pipeline_evaluation(storage, tmp_path):
    rows = [row for row in successful_rows() if row["kind"] == "audio"]
    result = {"speech": {"winner": "large", "evaluations": [{"model": "large", "rows": rows}]}}
    manager._promote(plan(), candidate_directory(tmp_path), result, "speech-only")
    assert manager.speech_model("turbo") == "turbo"
    result["speech"]["pipeline"] = {"rows": successful_rows()}
    manager._promote(plan(), tmp_path, result, "speech-pipeline")
    assert manager.speech_model("turbo") == "large"
    assert manager.speech_model("small") == "small"


class FakeLoRA:
    def __init__(self):
        self.lora_a = object()
        self.scale = 16.0


class FakeModel:
    def __init__(self, adapter):
        self.adapter = adapter
        self.layers = [FakeLoRA(), FakeLoRA()] if adapter else []

    def named_modules(self):
        return [(str(i), layer) for i, layer in enumerate(self.layers)]

    def output(self):
        # A LoRA layer at scale zero computes what the base layer does.
        return self.adapter if any(layer.scale for layer in self.layers) else "base"


@pytest.mark.asyncio
async def test_adapter_switches_are_serialized_and_general_generation_returns_to_base():
    from backend.backends.qwen_llm_backend import MLXQwenLLMBackend

    backend = MLXQwenLLMBackend("0.6B")
    loaded = []

    def load(size):
        loaded.append(backend._adapter_path)
        backend.model = FakeModel(backend._adapter_path)
        backend._current_model_size = size

    def unload():
        backend.model = None
        backend._current_model_size = None
        backend._loaded_adapter = None

    backend._load_model_sync = load
    backend.unload_model = unload
    backend._generate_sync = lambda *args: backend.model.output()
    assert await backend.generate("a", adapter_path="/adapter-one") == "/adapter-one"
    assert await backend.generate("a", adapter_path="/adapter-two") == "/adapter-two"
    # Base generation switches the loaded adapter off instead of reloading,
    # so styles it doesn't cover don't reload the model on every app switch.
    assert await backend.generate("a") == "base"
    assert backend._adapter_path is None
    assert await backend.generate("a", adapter_path="/adapter-two") == "/adapter-two"
    assert loaded == ["/adapter-one", "/adapter-two"]
    # A different size still reloads, without the adapter.
    assert await backend.generate("a", model_size="4B") == "base"
    assert loaded == ["/adapter-one", "/adapter-two", None]


def plenty_of_memory(monkeypatch):
    """Let training start on machines with less free memory than it asks for, such as CI."""
    import psutil

    memory = psutil.virtual_memory()
    monkeypatch.setattr(psutil, "virtual_memory", lambda: memory._replace(available=16 * 1024**3))


def test_supervisor_runs_isolated_worker_and_promotes_only_complete_results(storage, monkeypatch):
    import sys

    manager_plan = plan()
    manager_plan.update(train_ready=True)
    monkeypatch.setattr(manager, "_prepare", lambda: (manager_plan, "supervised", True))
    monkeypatch.setattr(manager.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(manager.platform, "machine", lambda: "arm64")
    plenty_of_memory(monkeypatch)

    def command(plan_path):
        script = """import json,sys
from pathlib import Path
root=Path(sys.argv[1]).parent
adapter=root/'adapter'
adapter.mkdir()
(adapter/'adapter_config.json').write_text('{}')
(adapter/'adapters.safetensors').write_bytes(b'test adapter')
(root/'result.json').write_text(sys.argv[2])
"""
        return [
            sys.executable,
            "-c",
            script,
            str(plan_path),
            json.dumps({"adapter": {"rows": successful_rows(), "tested_flags": [{}]}}),
        ]

    monkeypatch.setattr(manager, "_command", command)
    manager.start()
    manager._thread.join(5)
    assert not manager._thread.is_alive()
    assert manager.status()["phase"] == "activated"
    assert manager.status()["active_adapter"]


def test_real_child_is_killed_on_recording_and_never_promoted(storage, monkeypatch):
    import sys
    import time

    manager_plan = plan()
    manager_plan.update(train_ready=True)
    monkeypatch.setattr(manager, "_prepare", lambda: (manager_plan, "interrupted", True))
    monkeypatch.setattr(manager.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(manager.platform, "machine", lambda: "arm64")
    plenty_of_memory(monkeypatch)
    monkeypatch.setattr(manager, "_command", lambda _: [sys.executable, "-c", "import time; time.sleep(60)"])
    manager.start()
    deadline = time.monotonic() + 5
    while manager._process is None and time.monotonic() < deadline:
        time.sleep(0.01)
    process = manager._process
    assert process is not None
    manager.foreground_activity(recording=True)
    manager._thread.join(5)
    assert process.poll() is not None
    assert manager.status()["phase"] == "paused"
    assert not manager.status()["active_adapter"]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("pending", "retrain", "adapter", "rules_change", "learned", "started"),
    [
        (False, False, True, False, False, False),
        # A report is learned from at the next idle minute.
        (True, False, False, True, True, False),
        # New rules invalidate the adapter tested with the old ones: retest it.
        (True, False, True, True, True, True),
        (True, False, True, False, True, False),
        # A withdrawn report may be in the active adapter's training data.
        (True, True, True, False, False, True),
        (False, True, False, False, False, False),
    ],
)
async def test_idle_tick_learns_reports_soon_and_retests_the_adapter(
    storage, tmp_path, monkeypatch, pending, retrain, adapter, rules_change, learned, started
):
    import asyncio
    import time

    calls = []
    ticks = []
    real_sleep = asyncio.sleep

    async def sleep(seconds):
        ticks.append(seconds)
        if len(ticks) > 1:
            raise asyncio.CancelledError
        await real_sleep(0)

    learning = manager.correction_learning

    def run_job():
        calls.append("learn")
        if rules_change:
            learning._state = {"rules": [{"id": "new"}]}

    monkeypatch.setattr(manager.asyncio, "sleep", sleep)
    monkeypatch.setattr(manager, "_last_attempt", time.monotonic())
    monkeypatch.setattr(manager, "_active", {"llm": {"path": "adapter"}} if adapter else {})
    monkeypatch.setattr(manager, "start", lambda: calls.append("start"))
    monkeypatch.setattr(learning, "initialize", lambda: None)
    monkeypatch.setattr(learning, "run_job", run_job)
    monkeypatch.setattr(learning, "_pending", pending)
    monkeypatch.setattr(learning, "_retrain", retrain)
    with pytest.raises(asyncio.CancelledError):
        await manager.periodic_job()
    assert ("learn" in calls) is learned
    assert ("start" in calls) is started
    assert not learning.take_retrain()
