"""Training isolation, independent evaluation and using the trained models
the server keeps (server/src/model_improvement.rs owns state, schedule and
rollback)."""

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


def test_a_fact_the_model_in_use_also_misses_is_a_mistake_not_a_changed_fact():
    # The user wrote "second item" as "2."; neither model does. That counts as
    # an error, but it doesn't block every candidate as a changed fact.
    rows = successful_rows()
    rows.append(
        dict(
            rows[0],
            id="numbered",
            kind="control",
            expected="2. Record the video.",
            baseline="Second item. Record the video.",
            candidate="Second item, record the video.",
        )
    )
    assert not any("protected facts" in reason for reason in evaluation.score_rows(rows)["reasons"])


@pytest.fixture
def storage(tmp_path, monkeypatch):
    engine = create_engine(f"sqlite:///{tmp_path}/db.sqlite")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(manager.database_session, "SessionLocal", factory)
    for name, value in (
        ("_root", None),
        ("_active", {}),
        ("_process", None),
        ("_generation", 0),
        ("_foreground_count", 0),
        ("_recording_until", 0),
        ("_last_activity", 0),
        ("_runs", 0),
        ("_reports", []),
        ("_prepared", None),
    ):
        monkeypatch.setattr(manager, name, value)
    monkeypatch.setattr(manager.correction_learning, "_state", {"rules": []})
    manager.initialize()
    yield factory
    manager.foreground_activity()
    engine.dispose()


def save_state(tmp_path, active):
    """The server saved ``active`` as the trained models in use."""
    folder = tmp_path / "model-improvement"
    folder.mkdir(exist_ok=True)
    (folder / "state.json").write_text(json.dumps({"version": 1, "active": active}))


def restart(monkeypatch):
    monkeypatch.setattr(manager, "_root", None)
    manager.initialize()


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
        "model_size": "1.7B",
        "model_path": "/cached/base",
        "pipeline": manager.pipeline_id(),
        "rules": [],
        "configured_stt": "turbo",
    }


def adapter_result(rows=None):
    return {"adapter": {"rows": rows or successful_rows(), "tested_flags": [{}]}}


def test_a_passing_adapter_is_judged_used_and_kept_across_restarts(storage, tmp_path, monkeypatch):
    folder = candidate_directory(tmp_path)
    judged = manager._judge(plan(), folder, adapter_result(), None)
    assert judged["metrics"]["adapter"]["passed"]
    # Judging uses nothing yet: the server saves it, then says to use it.
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) is None
    assert manager.use(judged["active"]) == judged["active"]
    path = str(folder / "adapter")
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) == path
    assert manager.active_adapter("0.6B", RefinementFlags().to_dict()) is None
    assert manager.active_adapter("1.7B", RefinementFlags(self_correction=False).to_dict()) is None
    save_state(tmp_path, judged["active"])
    restart(monkeypatch)
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) == path
    assert manager.in_use()["llm"]["id"] == "candidate"
    # The server rolled back.
    manager.use({})
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) is None


def test_a_failed_gate_keeps_the_models_in_use(storage, tmp_path):
    folder = candidate_directory(tmp_path)
    rows = successful_rows()
    rows[0]["candidate"] = "A completely unrelated answer."
    judged = manager._judge(plan(), folder, adapter_result(rows), None)
    assert judged["active"] == {}
    assert not judged["metrics"]["adapter"]["passed"]


def test_new_rules_during_evaluation_refuse_the_run(storage, tmp_path, monkeypatch):
    monkeypatch.setattr(manager.correction_learning, "_state", {"rules": [{"id": "new"}]})
    with pytest.raises(ValueError, match="pipeline changed"):
        manager._judge(plan(), candidate_directory(tmp_path), adapter_result(), None)


def test_tampered_adapter_falls_back_to_base_on_restart_and_is_reported(storage, tmp_path, monkeypatch):
    folder = candidate_directory(tmp_path)
    judged = manager._judge(plan(), folder, adapter_result(), None)
    save_state(tmp_path, judged["active"])
    (folder / "adapter" / "adapters.safetensors").write_bytes(b"corrupted")
    restart(monkeypatch)
    assert manager.active_adapter("1.7B", RefinementFlags().to_dict()) is None
    assert manager.check(judged["active"]) == {}
    [report] = manager.reports()
    assert (report["kind"], report["id"], report["block"]) == ("llm", "candidate", False)
    assert manager.reports() == []


def test_an_adapter_that_fails_in_use_is_dropped_and_reported_to_block(storage, tmp_path):
    judged = manager._judge(plan(), candidate_directory(tmp_path), adapter_result(), None)
    manager.use(judged["active"])
    manager.quarantine_adapter("bad weights", judged["active"]["llm"]["path"])
    assert manager.in_use() == {}
    assert manager.reports() == [{"kind": "llm", "id": "candidate", "error": "bad weights", "block": True}]


def test_recording_preempts_child_and_says_a_dictation_is_on(storage, monkeypatch):
    process = object()
    manager._process = process
    stopped = []
    monkeypatch.setattr(manager, "_stop_process", stopped.append)
    manager.foreground_activity(recording=True)
    assert stopped == [process]
    assert manager.dictating()
    assert manager.idle_generation() is None


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


def test_asking_whether_the_models_are_idle_keeps_them_idle(storage, monkeypatch):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes.learning import router
    from backend.services.model_improvement.middleware import ForegroundPriorityMiddleware

    app = FastAPI()
    app.add_middleware(ForegroundPriorityMiddleware)
    app.include_router(router)
    monkeypatch.setattr(manager, "_last_activity", manager.time.monotonic() - manager.IDLE_SECONDS - 1)
    monkeypatch.setattr(manager, "_recording_until", 0.0)
    client = TestClient(app)
    first = client.post("/worker/learning/idle").json()
    assert first["idle"]
    # Asking again, or for the styles' state, is no dictation.
    assert client.post("/worker/learning/idle").json() == first
    manager.begin_foreground()
    manager.end_foreground()
    second = client.post("/worker/learning/idle").json()
    assert not second["idle"]
    assert second["generation"] != first["generation"]


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
    judged = manager._judge(plan(), candidate_directory(tmp_path), result, None)
    assert "speech" not in judged["active"]
    result["speech"]["pipeline"] = {"rows": successful_rows()}
    manager.use(manager._judge(plan(), tmp_path, result, None)["active"])
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


def ready_to_train(monkeypatch, fingerprint):
    manager_plan = plan()
    manager_plan.update(train_ready=True, voice=None)
    kept = {"counts": {}, "evaluated_report_ids": []}
    monkeypatch.setattr(manager, "_prepare", lambda *_: (manager_plan, fingerprint, True, kept))
    monkeypatch.setattr(manager.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(manager.platform, "machine", lambda: "arm64")
    plenty_of_memory(monkeypatch)
    prepared = manager.prepare([], None, [])
    assert (prepared["ready"], prepared["fingerprint"], prepared["trains"]) == (True, fingerprint, True)
    return prepared


def test_supervisor_runs_isolated_worker_and_judges_only_complete_results(storage, monkeypatch):
    prepared = ready_to_train(monkeypatch, "supervised")

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
        return [sys.executable, "-c", script, str(plan_path), json.dumps(adapter_result())]

    monkeypatch.setattr(manager, "_command", command)
    judged = manager.train(prepared["run"], prepared["generation"], None)
    assert judged["active"]["llm"]["id"] == prepared["run"]
    assert judged["metrics"]["adapter"]["passed"]
    # Only the run it prepared trains, once.
    with pytest.raises(ValueError, match="wasn't prepared"):
        manager.train(prepared["run"], prepared["generation"], None)


def test_real_child_is_killed_on_recording_and_never_judged(storage, monkeypatch):
    import threading
    import time

    prepared = ready_to_train(monkeypatch, "interrupted")
    monkeypatch.setattr(manager, "_command", lambda _: [sys.executable, "-c", "import time; time.sleep(60)"])
    answers = []
    thread = threading.Thread(
        target=lambda: answers.append(manager.train(prepared["run"], prepared["generation"], None))
    )
    thread.start()
    deadline = time.monotonic() + 5
    while manager._process is None and time.monotonic() < deadline:
        time.sleep(0.01)
    process = manager._process
    assert process is not None
    manager.foreground_activity(recording=True)
    thread.join(5)
    assert process.poll() is not None
    assert answers == [{"interrupted": True}]
    assert manager.in_use() == {}


def test_a_run_is_never_prepared_off_apple_silicon(storage, monkeypatch):
    monkeypatch.setattr(manager.platform, "machine", lambda: "x86_64")
    assert manager.prepare([], None, []) == {"unsupported": True}
