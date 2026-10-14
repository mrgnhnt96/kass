"""Explicit corrections replace the local clause without dropping other facts."""

from unittest.mock import AsyncMock

import pytest

from backend.services.refinement import RefinementFlags, refine_transcript
from backend.services.spoken_corrections import apply_spoken_corrections, retracted_by


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (
            "I'm Morgan Hunt and I like to rock climb. No, actually, I like to code",
            "I'm Morgan Hunt and I like to code",
        ),
        ("I'm Morgan Hunt and I like to rock climb no actually I like to code", "I'm Morgan Hunt and I like to code"),
        ("I'm Morgan Hunt, and I like climbing. No wait, I like coding.", "I'm Morgan Hunt, and I like coding."),
        ("We meet on Tuesday. No, actually, we meet on Thursday.", "We meet on Thursday."),
        ("Keep the notes. We meet on Tuesday. No wait, we meet on Thursday.", "Keep the notes. We meet on Thursday."),
        ("I want coffee. I mean, I want tea. Bring two cups.", "I want tea. Bring two cups."),
        ("I like climbing. No actually I like coding. No wait I like sailing.", "I like sailing."),
        ("The meeting is on Monday. No actually the meeting is on Friday.", "The meeting is on Friday."),
        ("The meeting is on Monday. I meant the meeting is on Friday.", "The meeting is on Friday."),
        ("The meeting is on Monday, or rather the meeting is on Friday.", "The meeting is on Friday."),
    ],
)
def test_repeated_clause_corrections(raw, expected):
    assert apply_spoken_corrections(raw) == expected


@pytest.mark.parametrize(
    "raw",
    [
        "I like rock climbing and I like coding.",
        "I like climbing and Sarah likes swimming. No actually I like coding.",
        "I like climbing while my wife likes swimming. No actually I like coding.",
        "I like climbing, he likes swimming. No actually I like coding.",
        "I want mac and cheese. No actually I want pasta.",
        "I actually like rock climbing.",
        "I like climbing. No, actually, the weather is bad.",
        "I like climbing. She likes swimming. No actually I like coding.",
        "I like climbing and I work here. No actually I like coding.",
        "She said I like climbing. No actually I like coding.",
        'He said "I like climbing. No actually I like coding."',
        "I like climbing. No actually",
    ],
)
def test_ambiguous_or_literal_corrections_are_not_guessed(raw):
    assert apply_spoken_corrections(raw) is None


@pytest.mark.asyncio
async def test_repeated_clause_correction_preserves_name_without_rewriting(monkeypatch):
    backend = type("Backend", (), {"model_size": "0.6B", "generate": AsyncMock()})()
    monkeypatch.setattr("backend.services.refinement.llm_service.get_llm_model", lambda: backend)
    text, _ = await refine_transcript(
        "I'm Morgan Hunt and I like to rock climb. No, actually, I like to code",
        RefinementFlags(),
    )
    assert text == "I'm Morgan Hunt and I like to code"
    backend.generate.assert_not_awaited()


@pytest.mark.asyncio
async def test_disabled_corrections_skip_deterministic_edit(monkeypatch):
    raw = "I like climbing. No actually I like coding."
    backend = type("Backend", (), {"model_size": "0.6B", "generate": AsyncMock(return_value=raw)})()
    monkeypatch.setattr("backend.services.refinement.llm_service.get_llm_model", lambda: backend)
    assert (await refine_transcript(raw, RefinementFlags(self_correction=False)))[0] == raw
    # Only Whisper's final period is hidden; the retraction reaches the model as said.
    assert backend.generate.call_args.kwargs["prompt"] == raw.removesuffix(".")


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (
            "Alright, my favorite candy is Bubblegum. No, no, no, it's Reese's Pieces.",
            "Alright, my favorite candy is Reese's Pieces.",
        ),
        (
            "My favorite candy is Bubblegum no no no it\u2019s Reese\u2019s Pieces.",
            "My favorite candy is Reese\u2019s Pieces.",
        ),
        (
            "Keep the receipt. My favorite candy is Bubblegum. No, no, it's Reese's Pieces. Buy two bags.",
            "Keep the receipt. My favorite candy is Reese's Pieces. Buy two bags.",
        ),
        ("The appointment was Monday. No wait, it was Tuesday.", "The appointment was Tuesday."),
        ("The tickets are blue. No, no, they're green.", "The tickets are green."),
        (
            "My favorite candy is Bubblegum. No, no, it's caramel. No actually it's Reese's Pieces.",
            "My favorite candy is Reese's Pieces.",
        ),
        ("I like climbing. No, no, no, I like coding.", "I like coding."),
    ],
)
def test_repeated_no_and_pronoun_value_corrections(raw, expected):
    assert apply_spoken_corrections(raw) == expected


@pytest.mark.parametrize(
    "raw",
    [
        "My favorite candy is Bubblegum. No, no, it's not Reese's Pieces.",
        "My favorite candy is Bubblegum. No, no, it was Reese's Pieces.",
        "My favorite candy is Bubblegum and the shop is closed. No, no, it's Reese's Pieces.",
        "My favorite candy is Bubblegum. No, no, it's",
        "She said \"My favorite candy is Bubblegum. No, no, it's Reese's Pieces.\"",
    ],
)
def test_unclear_pronoun_correction_stays_with_model(raw):
    assert apply_spoken_corrections(raw) is None


@pytest.mark.asyncio
async def test_candy_correction_never_reaches_model_that_reverses_it(monkeypatch):
    backend = type(
        "Backend",
        (),
        {
            "model_size": "0.6B",
            "generate": AsyncMock(
                return_value="Alright, my favorite candy is Reese's Pieces. No, no, no, it's bubblegum."
            ),
        },
    )()
    monkeypatch.setattr("backend.services.refinement.llm_service.get_llm_model", lambda: backend)
    actual, _ = await refine_transcript(
        "Alright, my favorite candy is Bubblegum. No, no, no, it's Reese's Pieces.",
        RefinementFlags(),
    )
    assert actual == "Alright, my favorite candy is Reese's Pieces."
    backend.generate.assert_not_awaited()


@pytest.mark.asyncio
async def test_refinement_does_not_invent_a_name_from_a_misheard_phrase(monkeypatch):
    backend = type("Backend", (), {"model_size": "0.6B", "generate": AsyncMock()})()
    monkeypatch.setattr("backend.services.refinement.llm_service.get_llm_model", lambda: backend)
    actual, _ = await refine_transcript(
        "I'm going to hunt and I like climbing. No actually I like coding.",
        RefinementFlags(),
    )
    assert actual == "I'm going to hunt and I like coding."
    backend.generate.assert_not_awaited()


EARLIER = "The meeting is on Tuesday. Bring the slides."


@pytest.mark.parametrize(
    "phrase",
    [
        "No, the meeting is on Wednesday.",
        "Wait, the meeting is on Wednesday.",
        "Sorry, Wednesday.",
        "I meant the meeting is on Wednesday.",
        "Or rather on Wednesday.",
        "Scratch that, bring the laptop.",
        "The meeting is on Wednesday.",
    ],
)
def test_a_phrase_that_takes_back_earlier_words(phrase):
    retracted = retracted_by(phrase, EARLIER)
    assert retracted is not None
    assert retracted.startswith(EARLIER)


@pytest.mark.parametrize(
    ("earlier", "phrase"),
    [
        ("Can I use your key? I need it today.", "No, unless you replace that key, that key is my personal one."),
        ("We need to update the docs. Then ship it.", "Then update the docs again."),
        ("I want to go home. It is late.", "I want to eat first."),
        ("The fix is merged. We are waiting on QA.", "We ship Thursday."),
        ("Do you need the slides?", "No, I already have a copy of them from last week."),
        ("Is it ready?", "Sorry I'm late, the train was slow this morning."),
    ],
)
def test_an_answer_or_new_thought_takes_nothing_back(earlier, phrase):
    assert retracted_by(phrase, earlier) is None
