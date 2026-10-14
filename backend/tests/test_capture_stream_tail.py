"""Sentence-aware streaming cleanup (docs/plans/SENTENCE_AWARE_CLEANUP.md).

A pause is often the speaker thinking mid-sentence. Cleanup works on the open
tail, the raw words since the last final sentence, so a sentence split by
pauses is cleaned as one.
"""

import asyncio
import re
from unittest.mock import AsyncMock

import pytest

from backend.backends import qwen_llm_backend
from backend.services import capture_stream
from backend.tests.test_capture_stream import append, make_session


def scripted(outputs):
    """A refine_transcript fake answering each prompt from ``outputs`` and recording prompts."""
    prompts = []

    async def refine(prompt, flags, model_size=None):
        prompts.append(prompt)
        return outputs[prompt], "4B"

    return refine, prompts


def refining_session(tmp_path, monkeypatch, refine):
    session, events = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    from backend.services import correction_learning

    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    return session, events


@pytest.mark.asyncio
async def test_a_sentence_split_by_pauses_is_cleaned_as_one(tmp_path, monkeypatch):
    # The user's dictation, as Whisper heard it phrase by phrase.
    refine, prompts = scripted(
        {
            "Wait, but if I pause": "Wait, but if I pause.",
            "Wait, but if I pause for a": "Wait, but if I pause for a.",
            "Wait, but if I pause for a second": "Wait, but if I pause for a second.",
            "Wait, but if I pause for a second does that mean": ("Wait, but if I pause for a second, does that mean."),
            "Wait, but if I pause for a second does that mean that my pauses cannot be cleaned?": (
                "Wait, but if I pause for a second, does that mean my pauses can't be cleaned?"
            ),
            "Wait, but if I pause for a second does that mean that my pauses cannot be cleaned From in between": (
                "Wait, but if I pause for a second, does that mean my pauses can't be cleaned from in between?"
            ),
            (
                "Wait, but if I pause for a second does that mean that my pauses cannot be cleaned "
                "From in between pauses."
            ): "Wait, but if I pause for a second, does that mean my pauses can't be cleaned from in between pauses?",
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    for phrase in [
        "Wait, but if I pause-",
        "for a-",
        "second.",
        "Does that mean-",
        "that my pauses cannot be cleaned?",
    ]:
        await session.accept(phrase, paused=True)
    await session.accept("From in between.", paused=True)
    session.finish()
    await session.accept("Pauses.")
    await session.run()
    session.close()
    assert session.refined == (
        "Wait, but if I pause for a second, does that mean my pauses can't be cleaned from in between pauses?"
    )
    # Saved without the marks the pauses added; the last phrase keeps its ending.
    assert session.raw == (
        "Wait, but if I pause for a second does that mean that my pauses cannot be cleaned From in between pauses."
    )
    assert len(prompts) == 7


@pytest.mark.asyncio
async def test_a_question_mark_at_a_pause_does_not_split_the_question(tmp_path, monkeypatch):
    # The user's dictation: Whisper ended the first phrase with "?" only
    # because the audio paused there, and cleanup then ended the sentence at it.
    refine, prompts = scripted(
        {
            "Wait, what if I pause?": "Wait, what if I pause?",
            "Wait, what if I pause For a second": "Wait, what if I pause for a second?",
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("Wait, what if I pause?", paused=True)
    session.finish()
    await session.accept("For a second.", paused=True)
    await session.run()
    session.close()
    assert prompts[-1] == "Wait, what if I pause For a second"
    assert session.raw == "Wait, what if I pause For a second"
    assert session.refined == "Wait, what if I pause for a second?"


@pytest.mark.asyncio
async def test_a_question_followed_by_i_keeps_its_mark(tmp_path, monkeypatch):
    refine, prompts = scripted(
        {
            "What's the other one?": "What's the other one?",
            "What's the other one? I have a reminder for tomorrow.": (
                "What's the other one? I have a reminder for tomorrow."
            ),
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("What's the other one?", paused=True)
    session.finish()
    await session.accept("I have a reminder for tomorrow.")
    await session.run()
    session.close()
    assert prompts[-1] == "What's the other one? I have a reminder for tomorrow."
    assert session.refined == "What's the other one? I have a reminder for tomorrow."


@pytest.mark.asyncio
async def test_finished_sentences_are_never_cleaned_again(tmp_path, monkeypatch):
    refine, prompts = scripted(
        {
            "the fix is merged we are waiting on": "The fix is merged. We are waiting on.",
            "we are waiting on QA": "We are waiting on QA.",
            "we are waiting on QA if nothing comes up we ship Thursday.": (
                "We are waiting on QA. If nothing comes up, we ship Thursday."
            ),
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("the fix is merged we are waiting on-", paused=True)
    await session.accept("QA.", paused=True)
    session.finish()
    await session.accept("If nothing comes up we ship Thursday.")
    await session.run()
    session.close()
    assert prompts[1] == "we are waiting on QA"
    # After release only the open sentence and the last phrase are cleaned.
    assert prompts[-1] == "we are waiting on QA if nothing comes up we ship Thursday."
    assert session.refined == "The fix is merged. We are waiting on QA. If nothing comes up, we ship Thursday."


@pytest.mark.asyncio
async def test_a_long_tail_without_a_sentence_end_is_settled_anyway(tmp_path, monkeypatch):
    monkeypatch.setattr(capture_stream, "MAX_OPEN_WORDS", 5)
    prompts = []

    async def refine(prompt, flags, model_size=None):
        prompts.append(prompt)
        return prompt[0].upper() + prompt[1:] + ".", "4B"

    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("one two three", paused=True)
    await session.accept("four five six", paused=True)
    session.finish()
    await session.accept("seven eight")
    await session.run()
    session.close()
    # The bound keeps the cleanup after release about one phrase long.
    assert prompts == ["one two three", "one two three four five six", "seven eight"]
    assert session.refined == "One two three four five six seven eight."


@pytest.mark.asyncio
async def test_a_correction_reopens_the_last_finished_sentence(tmp_path, monkeypatch):
    refine, _prompts = scripted(
        {
            "the meeting is on Tuesday. Bring snacks": "The meeting is on Tuesday. Bring snacks.",
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("the meeting is on Tuesday. Bring snacks.", paused=True)
    # Nothing to reopen yet: "Bring snacks" is still open.
    assert session.settled == "The meeting is on Tuesday."
    refine2, prompts2 = scripted(
        {"the meeting is on Tuesday. Bring snacks no actually Wednesday": "The meeting is on Wednesday. Bring snacks."}
    )
    monkeypatch.setattr(capture_stream, "refine_transcript", refine2)
    session.finish()
    await session.accept("no actually Wednesday")
    await session.run()
    session.close()
    assert prompts2 == ["the meeting is on Tuesday. Bring snacks no actually Wednesday"]
    assert "Tuesday" not in session.refined


def blocking_refine(outputs, started):
    """A refine fake that generates until release stops it, as the MLX backend does."""
    stops = []

    async def refine(prompt, flags, model_size=None):
        stop = qwen_llm_backend.generation_stop.get()
        stops.append(stop)
        started.set()
        if stop is not None:
            # Generation checks the flag between tokens.
            await asyncio.to_thread(stop.wait, 0.5)
            if stop.is_set():
                return "Partial", "4B"
        return outputs[prompt], "4B"

    return refine, stops


@pytest.mark.asyncio
async def test_release_stops_a_cleanup_that_more_speech_will_replace(tmp_path, monkeypatch):
    started = asyncio.Event()
    refine, stops = blocking_refine({"first part and the rest.": "First part and the rest."}, started)
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    session.recognize = AsyncMock(side_effect=["first part", "And the rest."])
    worker = asyncio.create_task(session.run())
    append(session, 2)
    append(session, 1, amplitude=0)
    await asyncio.wait_for(started.wait(), 2)
    append(session, 1)
    session.finish()
    await asyncio.wait_for(worker, 2)
    session.close()
    assert stops[0].is_set()
    assert session.refined == "First part and the rest."
    # The cleanup after release doesn't wait for, or keep, the stopped one.
    assert session.after_release["refine"] < 0.4


@pytest.mark.asyncio
async def test_release_keeps_a_cleanup_nothing_follows(tmp_path, monkeypatch):
    started = asyncio.Event()
    refine, stops = blocking_refine({"all of it": "All of it."}, started)
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    session.recognize = AsyncMock(side_effect=["all of it", ""])
    worker = asyncio.create_task(session.run())
    append(session, 2)
    append(session, 1, amplitude=0)
    await asyncio.wait_for(started.wait(), 2)
    session.finish()
    await asyncio.wait_for(worker, 2)
    session.close()
    assert len(stops) == 1
    assert not stops[0].is_set()
    assert session.refined == "All of it."


@pytest.mark.asyncio
async def test_a_stopped_cleanup_is_redone_when_the_rest_was_only_noise(tmp_path, monkeypatch):
    started = asyncio.Event()
    refine, stops = blocking_refine({"first part": "First part."}, started)
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    # The voice detector heard something, but Whisper found no words.
    session.recognize = AsyncMock(side_effect=["first part", ""])
    worker = asyncio.create_task(session.run())
    append(session, 2)
    append(session, 1, amplitude=0)
    await asyncio.wait_for(started.wait(), 2)
    append(session, 1)
    session.finish()
    await asyncio.wait_for(worker, 2)
    session.close()
    assert stops[0].is_set()
    assert session.refined == "First part."


@pytest.mark.asyncio
async def test_each_cleanup_is_told_what_the_tail_cleaned_to_last_time(tmp_path, monkeypatch):
    hints = []
    outputs = {
        "the fix is merged we are waiting on": "The fix is merged. We are waiting on.",
        "we are waiting on QA": "We are waiting on QA.",
        "we are waiting on QA today.": "We are waiting on QA today.",
    }

    async def refine(prompt, flags, model_size=None):
        hints.append(qwen_llm_backend.generation_hint.get())
        return outputs[prompt], "4B"

    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("the fix is merged we are waiting on-", paused=True)
    await session.accept("QA.", paused=True)
    session.finish()
    await session.accept("today.")
    await session.run()
    session.close()
    # Most of each cleanup repeats the last one, which the model checks in
    # large steps instead of generating again.
    assert hints == ["", "We are waiting on.", "We are waiting on QA."]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("heard", "cleaned", "expected"),
    [
        ("New line, hello there.", "\nHello there.", "\nHello there."),
        ("Hello there, new line.", "Hello there.\n", "Hello there.\n"),
        ("New line.", "\n", "\n"),
    ],
)
async def test_a_spoken_line_break_at_either_end_is_kept(tmp_path, monkeypatch, heard, cleaned, expected):
    # The field's sentence goes on, so a common first word is lowercased.
    refine, _ = scripted({heard: cleaned, heard[0].lower() + heard[1:]: cleaned})
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    # Even where the field's sentence goes on, the text starts on a new line.
    session.set_context("We said")
    session.finish()
    await session.accept(heard)
    await session.run()
    session.close()
    assert session.refined == expected


@pytest.mark.asyncio
async def test_a_line_break_settled_at_is_not_doubled(tmp_path, monkeypatch):
    refine, prompts = scripted(
        {
            "Here is a list, new line, bananas": "Here is a list:\nBananas",
            # Settled at the break; the open tail starts with the command.
            "new line, bananas and peppers.": "\nBananas and peppers.",
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("Here is a list, new line, bananas.", paused=True)
    session.finish()
    await session.accept("And peppers.")
    await session.run()
    session.close()
    assert session.refined == "Here is a list:\nBananas and peppers."
    assert prompts[-1] == "new line, bananas and peppers."


def words(text):
    return re.findall(r"[\w']+", text.lower())


def cleaned_or(whole_take=None):
    """A refine fake that capitalizes and closes each prompt, recording them.

    The whole take, once released, cleans up to ``whole_take``.
    """
    prompts = []
    session = None

    async def refine(prompt, flags, model_size=None):
        prompts.append(prompt)
        if whole_take is not None and session.finished and words(prompt) == words(session.raw):
            return whole_take, "4B"
        text = prompt.rstrip(" .") + "."
        return text[0].upper() + text[1:], "4B"

    def attach(value):
        nonlocal session
        session = value

    return refine, prompts, attach


async def replay(tmp_path, monkeypatch, phrases, whole_take=None):
    """Each phrase cut at a pause, released during the last one."""
    refine, prompts, attach = cleaned_or(whole_take)
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    attach(session)
    for phrase in phrases[:-1]:
        await session.accept(phrase, paused=True)
    # The correction's target is final already: cleaning the tail can't reach it.
    assert session.settled
    session.finish()
    await session.accept(phrases[-1])
    await session.run()
    session.close()
    return session, prompts


# From real takes: a correction said after a pause, once the sentence it
# corrects was settled. The second value is what the whole take cleans up to.
CORRECTIONS_AFTER_A_PAUSE = [
    (["The meeting is on Tuesday. Bring the slides.", "No, the meeting is on Wednesday."], "no"),
    (["The meeting is on Tuesday. Bring the slides.", "Wait, the meeting is on Wednesday."], "wait"),
    (["The meeting is on Tuesday. Bring the slides.", "Sorry, Wednesday."], "sorry"),
    (["The meeting is on Tuesday. Bring the slides.", "I meant the meeting is on Wednesday."], "I meant"),
    (["The meeting is on Tuesday. Bring the slides.", "Or rather on Wednesday."], "or rather"),
    # A plain restatement, no cue word.
    (["The meeting is on Tuesday. Bring the slides.", "The meeting is on Wednesday."], "restated"),
    # The correction targets a sentence settled before the last one.
    (
        [
            "The meeting is on Tuesday. Bring the slides.",
            "And the snacks. Ask Sam for cups.",
            "Sorry, the meeting is on Wednesday.",
        ],
        "older",
    ),
]


@pytest.mark.asyncio
@pytest.mark.parametrize(("phrases", "case"), CORRECTIONS_AFTER_A_PAUSE)
async def test_a_correction_after_a_pause_cleans_the_whole_take_again(tmp_path, monkeypatch, phrases, case):
    expected = "The meeting is on Wednesday. Bring the slides."
    session, prompts = await replay(tmp_path, monkeypatch, phrases, expected)
    # Either the reopened tail or, for an older target, the whole take.
    assert words(prompts[-1]) == words(session.raw)
    assert session.refined == expected


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "phrases",
    [
        # "No" answers a question; what follows adds to it and must all stay.
        ["Can I use your token? I need it today.", "No, unless you replace that token, that token is my personal one."],
        ["We need to update the docs. Then ship it.", "Then update the docs again."],
        ["I want to go home. It is late.", "I want to eat first."],
        ["The fix is merged. We are waiting on QA.", "We ship Thursday."],
        ["Actually, the fix is merged. We are waiting on QA.", "If nothing comes up we ship Thursday."],
    ],
)
async def test_a_phrase_that_corrects_nothing_does_not_clean_the_whole_take(tmp_path, monkeypatch, phrases):
    session, prompts = await replay(tmp_path, monkeypatch, phrases, "Lost text.")
    assert words(session.raw) not in [words(prompt) for prompt in prompts]
    assert "Lost" not in session.refined


@pytest.mark.asyncio
async def test_a_rejected_whole_take_cleanup_keeps_the_streamed_text(tmp_path, monkeypatch):
    phrases = ["The meeting is on Tuesday. Bring the slides.", "And the snacks. Ask Sam for cups.", "Sorry, Wednesday."]
    # Answers with something never said: the content check rejects it.
    session, prompts = await replay(
        tmp_path, monkeypatch, phrases, "Here is your text: the party is at the beach on Friday."
    )
    assert words(prompts[-1]) == words(session.raw)
    assert "party" not in session.refined
    assert words(session.refined) == words(session.raw)


@pytest.mark.asyncio
async def test_a_tail_cleaned_in_lowercase_keeps_the_transcripts_capitals(tmp_path, monkeypatch):
    # Capture c5f17de8: told the tail continued a sentence, cleanup wrote it
    # all in lowercase after the period it had put before it.
    refine, _ = scripted(
        {
            "the output came out really poor": "The output came out really poor.",
            "the output came out really poor and then I pause and Kass keeps it": (
                "The output came out really poor. and then i pause and kass keeps it."
            ),
            "and then I pause and Kass keeps it": "and then i pause and kass keeps it.",
        }
    )
    session, _ = refining_session(tmp_path, monkeypatch, refine)
    await session.accept("the output came out really poor", paused=True)
    session.finish()
    await session.accept("and then I pause and Kass keeps it")
    await session.run()
    session.close()
    assert session.refined == "The output came out really poor. And then I pause and Kass keeps it."
