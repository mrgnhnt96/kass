"""The capital Whisper gives a phrase cut at a pause, and names said mid-sentence."""

import pytest

from backend.services.phrase_seams import continue_phrase, mid_sentence_capitals


@pytest.mark.parametrize(
    ("phrase", "earlier", "expected"),
    [
        ("Does that mean-", "Wait, but if I pause for a second.", "does that mean-"),
        ("Pauses.", "that my pauses cannot be cleaned from in between.", "pauses."),
        # "I" and acronyms keep their capitals.
        ("I think so", "It works.", "I think so"),
        ("VS Code is open", "I was using it.", "VS Code is open"),
        # A word capitalized mid-sentence anywhere in the dictation is a name.
        ("Sagar is not focused", "I have to hit enter twice in Sagar.", "Sagar is not focused"),
        ("Sagar said so, ask Sagar", "Hello.", "Sagar said so, ask Sagar"),
        ("already lowercase", "fine.", "already lowercase"),
        ("", "", ""),
    ],
)
def test_continue_phrase_drops_the_capital_a_pause_added(phrase, earlier, expected):
    assert continue_phrase(phrase, earlier) == expected


@pytest.mark.parametrize(
    ("phrase", "earlier", "names", "expected"),
    [
        # Common words lose the capital Whisper gave them.
        ("Move the meeting to Friday", "I think we should", frozenset(), "move the meeting to Friday"),
        ("The tests pass", "Let's ship it once", frozenset(), "the tests pass"),
        ("Unfortunately not", "and", frozenset(), "unfortunately not"),
        # Names keep theirs.
        ("Morgan about the budget", "Please ask", frozenset(), "Morgan about the budget"),
        ("Sarah and she agreed", "I talked to", frozenset(), "Sarah and she agreed"),
        ("Kubernetes broke", "and then", frozenset(), "Kubernetes broke"),
        ("GitHub is down", "and", frozenset(), "GitHub is down"),
        # A word the user capitalizes mid-sentence is a name to them.
        ("Slack is down", "and", frozenset({"Slack"}), "Slack is down"),
        ("Slack is down", "we use Slack daily and", frozenset(), "Slack is down"),
    ],
)
def test_continue_phrase_keeps_names(phrase, earlier, names, expected):
    assert continue_phrase(phrase, earlier, names) == expected


def test_mid_sentence_capitals_treats_each_line_as_a_start():
    assert mid_sentence_capitals("Hi,\nThanks for asking Morgan") == {"Morgan"}
