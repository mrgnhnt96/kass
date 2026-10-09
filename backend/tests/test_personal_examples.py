"""The user's own examples: collected, matched to a transcript, and used by cleanup."""

import json
from unittest.mock import AsyncMock

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base, Capture, CaptureFeedback
from backend.services import personal_examples, writing_style
from backend.services.refinement import (
    REFINEMENT_EXAMPLES,
    RefinementFlags,
    build_refinement_prompt,
    refine_transcript,
)


@pytest.fixture(autouse=True)
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(writing_style, "_state", None)
    monkeypatch.setattr(personal_examples, "_cache", None)
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    session = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", session)
    return session


def _correct(storage, said, meant, capture_id=None, target="refined", source="manual"):
    with storage() as db:
        capture = Capture(id=capture_id or said[:20], audio_path="a.wav", transcript_raw=said)
        db.add(capture)
        db.add(
            CaptureFeedback(
                capture_id=capture.id,
                target=target,
                expected_text=meant,
                snapshot=json.dumps({"transcript_raw": said, "transcript_refined": said}),
                source=source,
            )
        )
        db.commit()
    personal_examples.invalidate()


def test_refined_corrections_become_examples(storage):
    _correct(storage, "so the release we need to push the release to friday", "We need to push the release to Friday.")
    _correct(storage, "teh cat", "the cat", target="raw")
    examples = personal_examples.all_examples()
    assert [(e["source"], e["said"], e["meant"]) for e in examples] == [
        ("correction", "so the release we need to push the release to friday", "We need to push the release to Friday.")
    ]


def test_only_explicit_reports_teach_the_style(storage):
    from backend.services import known_names

    _correct(storage, "meet megan at noon", "Meet Morgan at noon.", "voice", source="voice_fix")
    _correct(storage, "call megan at noon", "Call Rosalind at noon.", "again", source="redictation")
    assert [e["meant"] for e in personal_examples.all_examples()] == ["Meet Morgan at noon."]
    known_names.invalidate()
    names = known_names.known_names()
    assert "Morgan" in names
    assert "Rosalind" not in names


def test_withdrawn_correction_stops_being_an_example(storage):
    from backend.services.capture_feedback import withdraw_feedback

    _correct(storage, "meet megan at noon", "Meet Morgan at noon.", "voice", source="voice_fix")
    assert personal_examples.all_examples()
    with storage() as db:
        report = db.query(CaptureFeedback).one()
        assert withdraw_feedback("voice", report.id, db)
    assert personal_examples.all_examples() == []


def test_every_dictation_gets_the_same_examples_oldest_first(storage):
    _correct(storage, "lunch plans for thursday with the team", "Lunch with the team on Thursday.", "a")
    _correct(storage, "push the release to friday the release", "Push the release to Friday.", "b")
    first = personal_examples.for_prompt()
    # A different dictation sees exactly the same examples, so the cleanup
    # model's cached prompt still matches.
    assert personal_examples.for_prompt() == first
    assert [said for said, _ in first] == [
        "lunch plans for thursday with the team",
        "push the release to friday the release",
    ]


def test_a_new_correction_is_added_after_the_existing_ones(storage):
    _correct(storage, "lunch plans for thursday with the team", "Lunch with the team on Thursday.", "a")
    before = personal_examples.for_prompt()
    _correct(storage, "the dog needs a walk", "The dog needs a walk.", "c")
    after = personal_examples.for_prompt()
    assert after[: len(before)] == before
    assert after[-1] == ("the dog needs a walk", "The dog needs a walk.")


def test_only_the_most_recent_examples_fit(storage, monkeypatch):
    monkeypatch.setattr(personal_examples, "MAX_PROMPT_EXAMPLES", 2)
    for index in range(3):
        _correct(storage, f"example {index} said", f"Example {index} meant.", f"id{index}")
    assert [said for said, _ in personal_examples.for_prompt()] == ["example 1 said", "example 2 said"]


def test_unsaved_calibration_examples_come_last(storage):
    _correct(storage, "lunch plans for thursday with the team", "Lunch with the team on Thursday.", "a")
    examples = personal_examples.for_prompt(extra=[("draft said", "Draft meant.")])
    assert examples[-1] == ("draft said", "Draft meant.")


def test_hidden_examples_are_left_out(storage):
    _correct(storage, "so we should we should ship it", "We should ship it.")
    example_id = personal_examples.all_examples()[0]["id"]
    assert personal_examples.hide(example_id)
    assert personal_examples.all_examples() == []
    assert not personal_examples.hide("correction:missing")


def test_taught_replies_that_differ_from_the_cleanup_are_examples():
    writing_style.save_run(
        None,
        [
            {"said": "kept as shown", "shown": "Kept as shown.", "written": "Kept as shown."},
            {"said": "voicebox cleanup", "shown": "Voicebox cleanup.", "written": "What I would send."},
            {"said": None, "shown": "typed", "written": "typed"},
        ],
    )
    examples = personal_examples.all_examples()
    assert [(e["source"], e["said"], e["meant"]) for e in examples] == [
        ("calibration", "voicebox cleanup", "What I would send.")
    ]


def test_prompt_allows_restructuring_only_with_examples():
    assert "Restarts:" not in build_refinement_prompt(RefinementFlags())
    assert "Keep their vocabulary." in build_refinement_prompt(RefinementFlags())
    personal = build_refinement_prompt(RefinementFlags(), personal=True)
    assert "- Changed answers: after" in personal
    assert "- Things said late:" in personal
    assert "Keep their vocabulary." not in personal


@pytest.mark.asyncio
async def test_refinement_sends_the_users_examples_after_the_defaults(storage):
    _correct(storage, "so the release we need to push the release to friday", "We need to push the release to Friday.")
    backend = type("Backend", (), {"model_size": "0.6B", "generate": AsyncMock(return_value="Push it to Friday.")})()
    await refine_transcript("push it to friday", RefinementFlags(), backend_override=backend, use_personal_model=False)
    arguments = backend.generate.await_args.kwargs
    assert arguments["examples"][: len(REFINEMENT_EXAMPLES)] == REFINEMENT_EXAMPLES
    assert arguments["examples"][-1] == (
        "so the release we need to push the release to friday",
        "We need to push the release to Friday.",
    )
    assert "Restarts:" in arguments["system"]
    await refine_transcript(
        "push it to friday",
        RefinementFlags(),
        backend_override=backend,
        use_personal_model=False,
        use_personal_examples=False,
    )
    assert backend.generate.await_args.kwargs["examples"] == REFINEMENT_EXAMPLES


@pytest.mark.asyncio
async def test_refining_a_capture_keeps_and_flags_possible_content_changes(storage, monkeypatch):
    from backend.services import captures

    monkeypatch.setattr(
        captures, "refine_transcript", AsyncMock(return_value=("Send the notes and the slides.", "0.6B"))
    )
    with storage() as db:
        db.add(Capture(id="c1", audio_path="a.wav", transcript_raw="send the notes"))
        db.commit()
        output = await captures.refine_capture("c1", RefinementFlags(), None, db)
    assert output.transcript_refined == "Send the notes and the slides."
    assert output.refinement_review.model_dump() == {
        "outcome": "review",
        "added": ["slides"],
        "missing": [],
        "reasons": [],
    }


def test_examples_api_lists_and_removes(storage):
    from fastapi import FastAPI
    from fastapi.testclient import TestClient

    from backend.routes.writing_style import router

    _correct(storage, "so we should we should ship it", "We should ship it.")
    app = FastAPI()
    app.include_router(router)
    client = TestClient(app)
    examples = client.get("/writing-style/examples").json()
    assert [e["meant"] for e in examples] == ["We should ship it."]
    assert client.delete(f"/writing-style/examples/{examples[0]['id']}").status_code == 204
    assert client.get("/writing-style/examples").json() == []
    assert client.delete("/writing-style/examples/missing").status_code == 404
