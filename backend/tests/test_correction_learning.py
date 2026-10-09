"""Learning must earn activation on independent examples and remain reversible."""

from unittest.mock import AsyncMock

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from backend import config
from backend.database.models import Base, Capture
from backend.models import CaptureFeedbackCreate
from backend.services import correction_learning as learning, correction_rules as rules
from backend.services.capture_feedback import save_feedback
from backend.services.captures import get_capture, refine_capture
from backend.services.refinement import RefinementFlags


def samples(language=None, sources=("manual", "manual", "manual")):
    return [
        rules.Example(
            str(i),
            str(i),
            f"Please use voice box today at {place}.",
            f"Please use Voicebox today at {place}.",
            language,
            source,
        )
        for i, (place, source) in enumerate(zip(("home", "work", "school"), sources, strict=True))
    ]


@pytest.mark.parametrize("every_report", [False, True])
def test_activation_requires_two_teaching_takes_and_a_third_that_improves(every_report):
    examples = samples()
    assert rules.evaluate(examples[:2], [], every_report=every_report)[0] == []
    selected, metrics = rules.evaluate(examples, [], every_report=every_report)
    assert len(selected) == 1
    assert metrics["examples"] == 3
    assert metrics["accepted"] == 1
    assert rules.evaluate([examples[0]] * 6, [], every_report=every_report)[0] == []
    same_take = [rules.Example(e.id, "same", e.original, e.expected, e.language) for e in examples]
    assert rules.evaluate(same_take, [], every_report=every_report)[0] == []


def test_newest_reports_are_evidence_at_once():
    # Chronological hold-out kept the newest third of reports from ever teaching.
    # That stays the rule without every_report.
    older = [rules.Example(f"u{i}", f"u{i}", f"Old text {i}.", f"Totally new {i}.", None) for i in range(30)]
    assert rules.evaluate([*older, *samples()], [])[0] == []
    selected, metrics = rules.evaluate([*older, *samples()], [], every_report=True)
    assert len(selected) == 1
    assert metrics["accepted"] == 1


@pytest.mark.parametrize(
    ("sources", "learned"),
    [
        (("redictation", "redictation", "redictation"), False),
        (("manual", "redictation", "redictation"), True),
        (("voice_fix", "redictation", "voice_fix"), True),
        (("voice_fix", "voice_fix", "voice_fix"), True),
    ],
)
def test_redictations_only_back_up_an_explicit_report(sources, learned):
    assert bool(rules.evaluate(samples(sources=sources), [], every_report=True)[0]) is learned


def test_active_rule_without_support_is_dropped():
    selected, _ = rules.evaluate(samples(), [], every_report=True)
    kept, metrics = rules.evaluate(samples()[:2], selected, every_report=True)
    assert kept == []
    assert metrics["withdrawn"] == 1
    assert rules.evaluate(samples(), selected, every_report=True)[0] == selected
    # Without every_report, learned rules stay until contradicted or rolled back.
    assert rules.evaluate(samples()[:2], selected)[0] == selected


def test_only_matching_context_and_language_change():
    selected, _ = rules.evaluate(samples("en"), [])
    compiled = rules.compile_rules(selected)
    assert (
        rules.apply_rules("Please use voice box today at lunch.", compiled, "en")
        == "Please use Voicebox today at lunch."
    )
    for text in (
        "The voice box is part of the larynx.",
        "Please use voice box tomorrow.",
        "Please reuse voice box today.",
        "Please use voice box todayish.",
    ):
        assert rules.apply_rules(text, compiled, "en") == text
    assert rules.apply_rules(samples()[0].original, compiled, "fr") == samples()[0].original


def test_heldout_contradiction_and_known_good_block_activation(monkeypatch):
    examples = samples()
    unchanged = rules.Example(
        "4", "4", "Please use voice box today outside.", "Please use voice box today outside.", None
    )
    assert rules.evaluate([*examples, unchanged], [])[0] == []
    monkeypatch.setattr(rules, "KNOWN_GOOD", (examples[0].original,))
    assert rules.evaluate(examples, [])[0] == []


def test_latency_gate_blocks_activation(monkeypatch):
    ticks = iter(i * 0.01 for i in range(100))
    monkeypatch.setattr(rules.time, "perf_counter", lambda: next(ticks))
    selected, metrics = rules.evaluate(samples(), [])
    assert selected == []
    assert not metrics["latency_passed"]


@pytest.mark.parametrize(
    "expected", ["", "Completely different wording.", "Please use voice box today at home. Extra instructions."]
)
def test_deletions_and_broad_rewrites_are_not_learned(expected):
    assert rules.candidate(rules.Example("1", "1", samples()[0].original, expected, None)) is None


@pytest.fixture
def storage(tmp_path, monkeypatch):
    engine = create_engine(f"sqlite:///{tmp_path}/test.db")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(learning.database_session, "SessionLocal", factory)
    monkeypatch.setattr(learning, "_state", None)
    monkeypatch.setattr(learning, "_path", None)
    monkeypatch.setattr(learning, "_compiled", ())
    with factory() as db:
        for example in samples():
            db.add(
                Capture(
                    id=example.id,
                    audio_path="unused.wav",
                    source="dictation",
                    transcript_raw=example.original,
                    transcript_refined=example.original,
                )
            )
            db.commit()
            save_feedback(
                example.id,
                CaptureFeedbackCreate(
                    target="refined", expected_text=example.expected, snapshot=get_capture(example.id, db)
                ),
                db,
            )
    yield factory
    engine.dispose()


def test_job_persists_reloads_and_rollback_is_not_reactivated(storage, monkeypatch):
    result = learning.run_job()
    assert result["active_rules"] == 1
    assert result["revision"] == 1
    assert learning.run_job()["revision"] == 1
    monkeypatch.setattr(learning, "_state", None)
    monkeypatch.setattr(learning, "_compiled", ())
    learning.initialize()
    assert learning.apply_learned_corrections(samples()[0].original) == samples()[0].expected
    assert learning.rollback()["active_rules"] == 0
    assert learning.run_job()["active_rules"] == 0
    # Re-evaluation after additional feedback still respects the withdrawal.
    learning._state["fingerprint"] = None
    assert learning.run_job()["active_rules"] == 0
    with pytest.raises(ValueError, match="No previous"):
        learning.rollback()


def test_new_report_withdraws_conflicting_rule(storage):
    learning.run_job()
    with storage() as db:
        text = "Please use voice box today outside."
        db.add(
            Capture(
                id="new",
                audio_path="unused.wav",
                transcript_raw=text,
                transcript_refined=text.replace("voice box", "Voicebox"),
            )
        )
        db.commit()
        save_feedback(
            "new", CaptureFeedbackCreate(target="refined", expected_text=text, snapshot=get_capture("new", db)), db
        )
    assert learning.run_job()["active_rules"] == 0


def test_atomic_write_failure_keeps_active_cache(storage, monkeypatch):
    learning.initialize()

    def fail(*args):
        raise OSError("disk full")

    monkeypatch.setattr(learning.os, "fsync", fail)
    with pytest.raises(OSError, match="disk full"):
        learning.run_job()
    assert learning.status()["active_rules"] == 0
    assert learning.status()["evaluated_report_ids"] == []
    assert learning.apply_learned_corrections(samples()[0].original) == samples()[0].original


@pytest.mark.asyncio
async def test_future_refinement_uses_rules_without_changing_prompt_or_raw(storage, monkeypatch):
    from backend.services import captures

    learning.run_job()
    model = AsyncMock(return_value=("Please use voice box today at lunch.", "4B"))
    monkeypatch.setattr(captures, "refine_transcript", model)
    with storage() as db:
        db.add(Capture(id="future", audio_path="unused.wav", transcript_raw="please use voice box today at lunch"))
        db.commit()
        output = await refine_capture("future", RefinementFlags(), None, db)
    assert output.transcript_refined == "Please use Voicebox today at lunch."
    assert output.transcript_raw == "please use voice box today at lunch"
    model.assert_awaited_once_with("please use voice box today at lunch", RefinementFlags(), model_size=None)


def test_raw_reports_also_supply_candidates(storage):
    from backend.database.models import CaptureFeedback

    with storage() as db:
        for row in db.query(CaptureFeedback).all():
            row.target = "raw"
        db.commit()
    assert learning.run_job()["active_rules"] == 1


def test_corrupt_state_falls_back_to_no_rules(storage):
    (config.get_data_dir() / "correction-learning.json").write_text("{broken")
    assert learning.status()["active_rules"] == 0


def test_learning_http_controls(storage, monkeypatch):
    from backend.services.model_improvement import manager

    monkeypatch.setattr(manager, "start", lambda: {"can_rollback": False})
    monkeypatch.setattr(manager, "status", lambda: {"can_rollback": False})
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes.captures import router

    app = FastAPI()
    app.include_router(router)
    with TestClient(app) as client:
        assert client.get("/capture/learning").json()["active_rules"] == 0
        result = client.post("/capture/learning/run")
        assert result.status_code == 200
        assert result.json()["active_rules"] == 1
        assert client.post("/capture/learning/rollback").json()["active_rules"] == 0
        assert client.post("/capture/learning/rollback").status_code == 409


def test_numeric_changes_and_long_live_inputs_are_not_applied(storage):
    assert rules.candidate(rules.Example("1", "1", "Meet at 15 today.", "Meet at 50 today.", None)) is None
    learning.run_job()
    text = samples()[0].original * 200
    assert learning.apply_learned_corrections(text) == text


def test_status_identifies_only_reports_evaluated_by_a_completed_job(storage, monkeypatch):
    from backend.database.models import CaptureFeedback

    assert learning.status()["evaluated_report_ids"] == []
    with storage() as db:
        reports = db.query(CaptureFeedback).all()
        expected_ids = {row.id for row in reports}
    assert set(learning.run_job()["evaluated_report_ids"]) == expected_ids
    with storage() as db:
        pending = save_feedback(
            "0",
            CaptureFeedbackCreate(target="refined", expected_text="Another correction", snapshot=get_capture("0", db)),
            db,
        )
    assert pending.id not in learning.status()["evaluated_report_ids"]
    monkeypatch.setattr(learning, "_state", None)
    assert set(learning.status()["evaluated_report_ids"]) == expected_ids
    assert pending.id in learning.run_job()["evaluated_report_ids"]


def test_legacy_state_waits_for_a_new_evaluation(storage, monkeypatch):
    import json

    learning.run_job()
    path = config.get_data_dir() / "correction-learning.json"
    state = json.loads(path.read_text())
    report_ids = state.pop("evaluated_report_ids")
    path.write_text(json.dumps(state))
    monkeypatch.setattr(learning, "_state", None)
    assert learning.status()["evaluated_report_ids"] == []
    assert learning.run_job()["evaluated_report_ids"] == report_ids


def report(storage, capture_id, original, expected, source="manual"):
    with storage() as db:
        db.add(Capture(id=capture_id, audio_path="unused.wav", transcript_raw=original, transcript_refined=original))
        db.commit()
        return save_feedback(
            capture_id,
            CaptureFeedbackCreate(
                target="refined", expected_text=expected, snapshot=get_capture(capture_id, db), source=source
            ),
            db,
        )


def test_withdrawn_support_drops_the_rule_without_blocking_it(storage):
    from backend.database.models import CaptureFeedback
    from backend.services.capture_feedback import withdraw_feedback

    assert learning.run_job()["active_rules"] == 1
    with storage() as db:
        row = db.query(CaptureFeedback).filter(CaptureFeedback.capture_id == "0").one()
        assert withdraw_feedback("0", row.id, db)
    assert learning.pending()
    assert learning.run_job()["active_rules"] == 0
    assert not learning.pending()
    assert learning.apply_learned_corrections(samples()[1].original) == samples()[1].original
    assert learning._state["blocked"] == []
    # Reported again, it is learned again.
    report(storage, "again", "Please use voice box today at noon.", "Please use Voicebox today at noon.")
    assert learning.run_job()["active_rules"] == 1


def test_withdrawing_a_contradiction_lifts_its_block(storage):
    from backend.services.capture_feedback import withdraw_feedback

    learning.run_job()
    text = "Please use voice box today outside."
    with storage() as db:
        db.add(
            Capture(
                id="new",
                audio_path="unused.wav",
                transcript_raw=text,
                transcript_refined=text.replace("voice box", "Voicebox"),
            )
        )
        db.commit()
        contradiction = save_feedback(
            "new",
            CaptureFeedbackCreate(
                target="refined", expected_text=text, snapshot=get_capture("new", db), source="voice_fix"
            ),
            db,
        )
    assert learning.run_job()["active_rules"] == 0
    assert learning._state["blocked_by"] == {learning._state["blocked"][0]: [contradiction.id]}
    with storage() as db:
        assert withdraw_feedback("new", contradiction.id, db)
    assert learning.run_job()["active_rules"] == 1
    assert learning._state["blocked"] == []


def test_rollback_block_outlives_withdrawals(storage):
    from backend.database.models import CaptureFeedback
    from backend.services.capture_feedback import withdraw_feedback

    learning.run_job()
    learning.rollback()
    with storage() as db:
        row = db.query(CaptureFeedback).filter(CaptureFeedback.capture_id == "0").one()
        withdraw_feedback("0", row.id, db)
    report(storage, "again", "Please use voice box today at noon.", "Please use Voicebox today at noon.")
    assert learning.run_job()["active_rules"] == 0


def test_only_a_withdrawal_asks_for_the_adapter_to_retrain(storage, monkeypatch):
    from backend.services.capture_feedback import withdraw_feedback

    monkeypatch.setattr(learning, "_retrain", False)
    saved = report(storage, "again", "Please use voice box today at noon.", "Please use Voicebox today at noon.")
    assert learning.pending()
    assert not learning.take_retrain()
    with storage() as db:
        assert not withdraw_feedback("other", saved.id, db)
        assert withdraw_feedback("again", saved.id, db)
    assert learning.take_retrain()
    assert not learning.take_retrain()


def test_the_newest_reports_teach_at_once(storage):
    from datetime import datetime

    from backend.database.models import CaptureFeedback

    for i in range(6):
        report(storage, f"old{i}", f"Old text {i}.", f"Totally new {i}.")
    with storage() as db:
        db.query(CaptureFeedback).filter(CaptureFeedback.capture_id.like("old%")).update(
            {"created_at": datetime(2020, 1, 1)}, synchronize_session=False
        )
        db.commit()
    # The three matching reports are the newest third, and still teach.
    assert learning.run_job()["active_rules"] == 1
