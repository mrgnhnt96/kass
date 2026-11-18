"""Spoken commands: "paste from clipboard" becomes a marker the client fills."""

import pytest

from backend.services.refinement import RefinementFlags, refine_transcript
from backend.services.voice_commands import CLIPBOARD, commands_alone, mark_commands


@pytest.mark.parametrize(
    ("said", "marked"),
    [
        ("Paste from clipboard.", f"{CLIPBOARD}."),
        ("here's the link paste from the clipboard let me know", f"here's the link {CLIPBOARD} let me know"),
        ("Paste from my clipboard, then paste from clipboard", f"{CLIPBOARD}, then {CLIPBOARD}"),
        # How Whisper has spelled it.
        ("This is my PR branch, Pace from Clipboard. Thanks.", f"This is my PR branch, {CLIPBOARD}. Thanks."),
        ("pays from the clipboard", CLIPBOARD),
        ("Paced, from clipboard.", f"{CLIPBOARD}."),
        ("copy it to the clipboard", "copy it to the clipboard"),
        ("I read it from the clipboard", "I read it from the clipboard"),
        ("I copied it from my clipboard", "I copied it from my clipboard"),
        ("the paste from clipboards", "the paste from clipboards"),
    ],
)
def test_the_phrase_becomes_a_marker_wherever_it_is_said(said, marked):
    assert mark_commands(said) == marked


def test_a_dictation_of_only_commands_has_nothing_to_clean():
    assert commands_alone(f"{CLIPBOARD}.") == CLIPBOARD
    assert commands_alone("[Clipboard]") == CLIPBOARD
    assert commands_alone(f"{CLIPBOARD} {CLIPBOARD}") == f"{CLIPBOARD} {CLIPBOARD}"
    assert commands_alone(f"Here {CLIPBOARD}.") is None
    assert commands_alone("") is None


@pytest.mark.asyncio
async def test_only_a_command_skips_the_model():
    class NoModel:
        model_size = "4B"

        async def generate(self, **_):
            raise AssertionError("the model ran")

    flags = RefinementFlags(True, True, True, "standard")
    assert await refine_transcript(f"{CLIPBOARD}.", flags, backend_override=NoModel()) == (CLIPBOARD, "4B")
