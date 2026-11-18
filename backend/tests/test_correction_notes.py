"""Older examples are summarized into rules instead of dropping out of cleanup."""

import json

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base, Capture, CaptureFeedback
from backend.services import correction_notes, llm, personal_examples, writing_style
from backend.services.model_improvement import manager
from backend.services.refinement import RefinementFlags, build_refinement_prompt

RULE = "Write Voicebox as one word."


@pytest.fixture(autouse=True)
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(writing_style, "_state", None)
    monkeypatch.setattr(personal_examples, "_cache", None)
    monkeypatch.setattr(correction_notes, "_state", None)
    monkeypatch.setattr(personal_examples, "MAX_PROMPT_EXAMPLES", 2)
    monkeypatch.setattr(correction_notes, "MIN_BATCH", 3)
    monkeypatch.setattr(manager, "interrupted", lambda generation: False)
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    session = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", session)
    return session


class FakeModel:
    """Summarizes to ``reply``; cleans up "voice box" only when told to by a rule."""

    model_size = "0.6B"

    def __init__(self, reply):
        self.reply = reply
        self.summaries = []

    async def generate(self, prompt, system=None, **_):
        if system.startswith("You keep a short list of rules"):
            self.summaries.append(prompt)
            return self.reply
        if "Never say Voicebox." in system:
            prompt = prompt.replace("voice box", "the desktop dictation app")
        if RULE in system:
            prompt = prompt.replace("voice box", "Voicebox")
        return prompt


def _correct(storage, index, said, meant):
    with storage() as db:
        db.add(Capture(id=f"c{index}", audio_path="a.wav", transcript_raw=said))
        db.add(
            CaptureFeedback(
                id=f"f{index}",
                capture_id=f"c{index}",
                target="refined",
                expected_text=meant,
                snapshot=json.dumps({"transcript_raw": said, "transcript_refined": said}),
            )
        )
        db.commit()
    personal_examples.invalidate()


def _voicebox_corrections(storage, count):
    for index in range(count):
        _correct(storage, index, f"open voice box number {index}", f"open Voicebox number {index}")


def _use(monkeypatch, model):
    monkeypatch.setattr(llm, "get_llm_model", lambda: model)


def test_only_examples_that_left_the_prompt_are_pending(storage):
    _voicebox_corrections(storage, 4)
    shown = {example["id"] for example in personal_examples.in_prompt()}
    assert len(shown) == 2
    assert [example["id"] for example in correction_notes.pending()] == ["correction:f0", "correction:f1"]


@pytest.mark.asyncio
async def test_waits_for_a_full_batch(storage, monkeypatch):
    model = FakeModel(f"- {RULE}")
    _use(monkeypatch, model)
    _voicebox_corrections(storage, 4)
    assert await correction_notes.summarize(RefinementFlags(), "0.6B", 0) is None
    assert model.summaries == []


@pytest.mark.asyncio
async def test_a_rule_that_helps_is_kept_and_used_by_cleanup(storage, monkeypatch):
    _use(monkeypatch, FakeModel(f"Here are the rules:\n- {RULE}\n- {RULE}"))
    _voicebox_corrections(storage, 5)
    status = await correction_notes.summarize(RefinementFlags(), "0.6B", 0)
    assert [note["text"] for note in status["notes"]] == [RULE]
    assert status["outcome"] == "updated"
    assert status["pending"] == 0
    # The summarized examples are not summarized again.
    assert await correction_notes.summarize(RefinementFlags(), "0.6B", 0) is None

    from backend.services.refinement import refine_transcript

    text, _ = await refine_transcript("open voice box later", RefinementFlags(), use_personal_model=False)
    assert text == "open Voicebox later"


@pytest.mark.asyncio
async def test_a_rule_that_makes_cleanup_worse_is_rejected(storage, monkeypatch):
    _use(monkeypatch, FakeModel("- Never say Voicebox."))
    _voicebox_corrections(storage, 5)
    status = await correction_notes.summarize(RefinementFlags(), "0.6B", 0)
    assert status["notes"] == []
    assert status["outcome"] == "no_change"
    # Considered either way, so a rejected batch is not retried forever.
    assert status["pending"] == 0


@pytest.mark.asyncio
async def test_a_bad_rule_does_not_sink_a_good_one(storage, monkeypatch):
    _use(monkeypatch, FakeModel(f"- Never say Voicebox.\n- {RULE}"))
    _voicebox_corrections(storage, 5)
    status = await correction_notes.summarize(RefinementFlags(), "0.6B", 0)
    assert [note["text"] for note in status["notes"]] == [RULE]
    assert status["outcome"] == "updated"


@pytest.mark.asyncio
async def test_older_lessons_are_checked_when_the_rules_change(storage, monkeypatch):
    _use(monkeypatch, FakeModel(f"- {RULE}"))
    _voicebox_corrections(storage, 5)
    await correction_notes.summarize(RefinementFlags(), "0.6B", 0)
    for index in range(5, 8):
        _correct(storage, index, f"the build is broken {index}", f"The build is broken {index}")
    # Dropping the old rule would undo what earlier corrections taught.
    _use(monkeypatch, FakeModel("- Capitalize the first word."))
    status = await correction_notes.summarize(RefinementFlags(), "0.6B", 0)
    assert [note["text"] for note in status["notes"]] == [RULE]
    assert status["outcome"] == "no_change"


@pytest.mark.asyncio
async def test_a_dictation_interrupts_the_summary_without_saving(storage, monkeypatch):
    _use(monkeypatch, FakeModel(f"- {RULE}"))
    monkeypatch.setattr(manager, "interrupted", lambda generation: True)
    _voicebox_corrections(storage, 5)
    assert await correction_notes.summarize(RefinementFlags(), "0.6B", 0) is None
    assert correction_notes.status()["pending"] == 3


def test_parse_keeps_short_rules_only():
    reply = "Rules:\n- Drop 'basically'.\n* Use numbered lists.\n1. Keep it short.\n- " + "x" * 200 + "\nprose"
    assert correction_notes.parse(reply) == ["Drop 'basically'.", "Use numbered lists.", "Keep it short."]


def test_rules_that_restate_an_example_are_dropped():
    examples = [{"said": "here is a list of groceries bananas peppers", "meant": "Groceries:\n- Bananas\n- Peppers"}]
    reply = (
        "- If the speaker says 'here is a list of groceries', format it as a list.\n"
        "- Turn items said in a row into a list, one item per line."
    )
    assert correction_notes.parse(reply, examples) == ["Turn items said in a row into a list, one item per line."]


def test_notes_go_in_the_prompt(storage):
    # Removing one is the Rust server's (tests/learned.rs).
    correction_notes._save({**correction_notes._empty(), "notes": [RULE, "Drop 'basically'."]})
    prompt = build_refinement_prompt(RefinementFlags(), personal=True, notes=correction_notes.prompt_section())
    assert f"- {RULE}" in prompt
