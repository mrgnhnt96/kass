"""Punctuation said as a word is written, kept through cleanup, and learned."""

from unittest.mock import AsyncMock

import pytest

from backend import config
from backend.services import correction_learning as learning
from backend.services.correction_rules import Example
from backend.services.refinement import RefinementFlags, prepare_refinement, refine_transcript
from backend.services.spoken_punctuation import (
    apply_spoken_punctuation,
    keep_spoken_punctuation,
    learn,
    period_after_closers,
)
from backend.services.writing_style import apply_style


@pytest.mark.parametrize(
    ("said", "written"),
    [
        ("I'm home comma see you soon period", "I'm home, see you soon."),
        # Whisper's own pauses around the word go with it.
        ("I'm home, comma, see you soon. Period.", "I'm home, see you soon."),
        ("That's great exclamation point", "That's great!"),
        # Whisper mishears the long names.
        ("But it sounds pretty cool, explanation point", "But it sounds pretty cool!"),
        ("We did it exclamatory mark", "We did it!"),
        ("That's great, exclamation mark, we did it", "That's great! We did it"),
        ("Is it done question mark", "Is it done?"),
        ("Where are you at question mark", "Where are you at?"),
        ("Bring three things colon a pen and paper", "Bring three things: a pen and paper"),
        ("Meet at 3 colon 30", "Meet at 3:30"),
        ("Go to https colon//example.com", "Go to https://example.com"),
        ('He said comma "hi"', 'He said, "hi"'),
        ("It works semicolon ship it", "It works; ship it"),
        ("It works semi-colon ship it", "It works; ship it"),
        ("I'm done full stop", "I'm done."),
        ("lowercase start period then more", "lowercase start. then more"),
    ],
)
def test_said_marks_are_written(said, written):
    assert apply_spoken_punctuation(said) == written


@pytest.mark.parametrize(
    "said",
    [
        "Add a comma after hello",
        "It took a period of time",
        "End it with an exclamation point",
        "Too many commas and periods",
        "Period.",
        "comma is the word",
        "Make your presentation point by point",
        "Give an explanation point",
        "First line\nperiod at the start",
    ],
)
def test_talked_about_marks_stay_words(said):
    assert apply_spoken_punctuation(said) == said


def test_learned_contexts_keep_the_word():
    # A said mark wins until the speaker corrects one back to a word.
    assert apply_spoken_punctuation("The trial period ends Friday") == "The trial. Ends Friday"
    assert apply_spoken_punctuation("Use the Oxford comma here") == "Use the Oxford, here"
    learned = ({}, frozenset({"trial period", "oxford comma"}))
    assert apply_spoken_punctuation("The trial period ends Friday", learned) == "The trial period ends Friday"
    assert apply_spoken_punctuation("Use the Oxford comma here", learned) == "Use the Oxford comma here"


def test_learned_words_for_marks():
    learned = ({"bang": "!"}, frozenset())
    assert apply_spoken_punctuation("That's great bang", learned) == "That's great!"
    assert apply_spoken_punctuation("That's great bang") == "That's great bang"


@pytest.mark.parametrize(
    ("said", "cleaned", "kept"),
    [
        # The model's period goes back to the comma said, lowering what follows.
        ("I'm home comma see you soon period", "I'm home. See you soon", "I'm home, see you soon."),
        ("It works comma ship it", "It works, ship it.", "It works, ship it."),
        # A mark the cleanup dropped comes back.
        ("Stop exclamation point now", "Stop now.", "Stop! Now."),
        # Whisper heard "!" in the voice; a cleanup that flattened it to a period gives it back.
        ("That's great!", "That's great.", "That's great!"),
        ("That's great! We won.", "That's great. We won.", "That's great! We won."),
        # A cleanup that reshaped the sentence around Whisper's "!" keeps its own.
        ("That's great! We won.", "That's great, we won.", "That's great, we won."),
        # A word kept a different number of times can't be matched: left alone.
        ("Done period done", "Done", "Done"),
    ],
)
def test_said_marks_survive_cleanup(said, cleaned, kept):
    assert keep_spoken_punctuation(said, cleaned) == kept


def test_said_marks_win_over_the_learned_style():
    habits = {
        "boundary": "comma",
        "lowercase_start": True,
        "drop_intro_comma": False,
        "drop_conjunction_comma": False,
        "drop_final_period": True,
    }
    said = "it's done period we ship tomorrow period"
    styled = apply_style("It's done. We ship tomorrow.", habits)
    assert styled == "it's done, we ship tomorrow"
    assert keep_spoken_punctuation(said, styled) == "it's done. we ship tomorrow."


@pytest.mark.asyncio
async def test_cleanup_gets_the_mark_and_keeps_it(monkeypatch):
    seen = []

    async def generate(**arguments):
        seen.append(arguments["prompt"])
        return "I'm home. See you soon."

    backend = type("Backend", (), {"model_size": "0.6B", "generate": AsyncMock(side_effect=generate)})()
    monkeypatch.setattr("backend.services.refinement.llm_service.get_llm_model", lambda: backend)
    refined, _ = await refine_transcript("um I'm home comma see you soon", RefinementFlags())
    assert seen == ["um I'm home, see you soon"]
    assert refined == "I'm home, see you soon."


def test_marks_are_left_as_words_without_cleanup():
    flags = RefinementFlags(smart_cleanup=False)
    assert prepare_refinement("I'm home comma see you", flags)[0] == "I'm home comma see you"


def _example(index, original, expected, source="manual"):
    return Example(str(index), str(index), original, expected, None, source)


def test_a_word_for_a_mark_is_learned_from_two_dictations():
    one = _example(1, "That's great bang.", "That's great!")
    two = _example(2, "We won, bang, finally.", "We won! Finally.")
    assert learn([one]) == {"aliases": {}, "kept": []}
    assert learn([one, two]) == {"aliases": {"bang": "!"}, "kept": []}
    # A redictation only backs up an explicit correction.
    redictated = [_example(i, e.original, e.expected, "redictation") for i, e in enumerate((one, two))]
    assert learn(redictated)["aliases"] == {}
    # Kept as a word elsewhere, it is a word.
    word = _example(3, "Big bang theory.", "The big bang theory.")
    assert learn([one, two, word])["aliases"] == {}


def test_a_mark_word_goes_back_to_a_word_where_the_speaker_says_so():
    corrected = _example(1, "The trial. Ends Friday.", "The trial period ends Friday.")
    assert learn([corrected]) == {"aliases": {}, "kept": ["trial period"]}
    wanted = [_example(i, "The trial period ends.", "The trial. Ends.") for i in (2, 3)]
    assert learn([corrected, *wanted])["kept"] == []


@pytest.fixture
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(learning, "_state", None)
    monkeypatch.setattr(learning, "_path", None)
    monkeypatch.setattr(learning, "_compiled", ())
    monkeypatch.setattr(learning, "_punctuation", ({}, frozenset()))
    return tmp_path


def test_learned_marks_reload_and_apply_to_the_next_dictation(storage):
    import json

    # The server learns them from the reports and saves them for the worker.
    marks = learn([_example(1, "That's great bang.", "That's great!"), _example(2, "We won bang.", "We won!")])
    state = {**learning._empty(), "punctuation": marks}
    (storage / "correction-learning.json").write_text(json.dumps(state))
    learning.reload()
    assert learning.learned_punctuation() == ({"bang": "!"}, frozenset())
    assert prepare_refinement("Nice work bang", RefinementFlags())[0] == "Nice work!"


@pytest.mark.parametrize(
    ("written", "placed"),
    [
        ('He said "stop." Then he left.', 'He said "stop". Then he left.'),
        ("It's fine (I checked.)", "It's fine (I checked)."),
        ("Call it \u201cdone.\u201d", "Call it \u201cdone\u201d."),
        ('She wrote ("see above.")', 'She wrote ("see above").'),
        ('He said "stop.".', 'He said "stop".'),
        ("It's 'final.'\nNext line", "It's 'final'.\nNext line"),
        # Only a period moves; the rest stay inside.
        ('He asked "why?"', 'He asked "why?"'),
        ('She yelled "go!"', 'She yelled "go!"'),
        ('He said "wait..."', 'He said "wait..."'),
        ('Already "right".', 'Already "right".'),
        ("A plain sentence.", "A plain sentence."),
        # An abbreviation keeps its own period, and the sentence still gets one.
        ("Bring snacks (chips, soda, etc.)", "Bring snacks (chips, soda, etc.)."),
        ('He said "made in the U.S."', 'He said "made in the U.S.".'),
        ("We start at (9 a.m.)", "We start at (9 a.m.)."),
        ("Bring snacks (chips, etc.).", "Bring snacks (chips, etc.)."),
        ('Go to "example.com."', 'Go to "example.com".'),
    ],
)
def test_a_period_goes_after_closing_quotes_and_parentheses(written, placed):
    assert period_after_closers(written) == placed
