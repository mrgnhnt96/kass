"""How dictation phrases cut at pauses are joined."""

import pytest

from backend.services.phrase_seams import close_phrase, join_phrases, keep_raw_capitals, open_phrase


@pytest.mark.parametrize(
    ("previous", "phrase", "raw", "style", "expected"),
    [
        # Whisper continued the sentence in lowercase.
        ("How", "Much is it?", "much is it?", "standard", "How much is it?"),
        ("How", "Much is it?", "much is it?", "casual", "How much is it?"),
        # A common opener in capitals starts a new sentence.
        ("It might", "But we'll see", "But we'll see", "standard", "It might. But we'll see"),
        ("It might", "But we'll see", "But we'll see", "casual", "It might, but we'll see"),
        # A capitalized name continues the sentence in either style.
        ("I talked to", "Morgan about it", "Morgan about it", "standard", "I talked to Morgan about it"),
        ("I talked to", "Morgan about it", "Morgan about it", "casual", "I talked to Morgan about it"),
        # "I" keeps its capital when lowercasing a continuation.
        ("It works", "I think", "I think", "casual", "It works, I think"),
        # A sentence Whisper already ended stays ended in standard, joins in casual.
        ("It's a 401k.", "So what's left?", "So what's left?", "standard", "It's a 401k. So what's left?"),
        ("It's a 401k.", "So what's left?", "So what's left?", "casual", "It's a 401k, so what's left?"),
        # Questions keep their mark.
        ("Is it done?", "Yes it is", "Yes it is", "casual", "Is it done? Yes it is"),
        # Spoken line breaks are kept as they are.
        ("First item\n\n", "Second item", "Second item", "casual", "First item\n\nSecond item"),
        ("", "Hello", "Hello", "standard", "Hello"),
        ("Hello", "", "", "standard", "Hello"),
    ],
)
def test_join_phrases(previous, phrase, raw, style, expected):
    assert join_phrases(previous, phrase, raw, style) == expected


def test_open_phrase_drops_only_punctuation_the_pause_added():
    assert open_phrase("It might.", "it might") == "It might"
    assert open_phrase("Wait...", "wait...") == "Wait..."
    # Whisper heard a question, so it stays one.
    assert open_phrase("Is it?", "is it?") == "Is it?"
    # A lone "how" became a question only because it was cut off.
    assert open_phrase("How?", "how") == "How"
    # Ended with a spoken line break: finished, not cut off.
    assert open_phrase("It might.\n", "it might new line") == "It might.\n"


def test_close_phrase_ends_an_open_dictation():
    assert close_phrase("It might") == "It might."
    assert close_phrase("Is it?") == "Is it?"
    assert close_phrase("") == ""
    # A spoken line break at the end stays, after the period.
    assert close_phrase("It might\n") == "It might.\n"
    assert close_phrase("It might.\n") == "It might.\n"
    assert close_phrase("\n") == "\n"


def test_a_phrase_that_starts_with_a_line_break_joins_at_it():
    assert join_phrases("Hello there.", "\nHow are you", "new line how are you", "standard") == (
        "Hello there.\nHow are you"
    )
    assert join_phrases("Hello there", "\n\nHow", "new paragraph how", "casual") == "Hello there\n\nHow"


@pytest.mark.parametrize(
    ("heard", "expected"),
    [
        # Whisper's endings for a phrase cut off at a pause.
        ("Wait, but if I pause-", "Wait, but if I pause"),
        ("for a-", "for a"),
        ("second.", "second"),
        ("How...", "How"),
        ("How\u2026", "How"),
        ("so the \u2014", "so the \u2014"),
        # Marks the speaker may have meant stay for cleanup to judge.
        ("that my pauses cannot be cleaned?", "that my pauses cannot be cleaned?"),
        ("wow!", "wow!"),
        # An abbreviation's period isn't an ending.
        ("we moved to the U.S.", "we moved to the U.S."),
        ("it costs $5 (maybe).", "it costs $5 (maybe)"),
        ("", ""),
    ],
)
def test_strip_pause_mark(heard, expected):
    from backend.services.phrase_seams import strip_pause_mark

    assert strip_pause_mark(heard) == expected


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
    from backend.services.phrase_seams import continue_phrase

    assert continue_phrase(phrase, earlier) == expected


@pytest.mark.parametrize(
    ("before", "phrase", "earlier", "expected"),
    [
        # The pause put the question mark inside the question. It goes, and the
        # capital Whisper gave the next phrase stays as the weaker hint.
        ("Wait, what if I pause?", "For a second.", "x", ("Wait, what if I pause", "For a second.")),
        ("does it still add dots?", "Not sure.", "does it still add dots?", ("does it still add dots", "Not sure.")),
        ("Wow!", "That was fast.", "Wow!", ("Wow", "That was fast.")),
        # Whisper itself continued the sentence.
        ("Is it going to show text?", "as it's written?", "x", ("Is it going to show text", "as it's written?")),
        # "I" and names are always capitalized, so their capital can't be the
        # hint: the mark stays.
        ("What's the other one?", "I have a reminder.", "x", ("What's the other one?", "I have a reminder.")),
        ("Did you ask Sagar?", "Sagar said no.", "I use Sagar daily.", ("Did you ask Sagar?", "Sagar said no.")),
        # Without a mark at the seam the phrase continues as usual.
        ("Wait, but if I pause", "For a second.", "Wait, but if I pause-", ("Wait, but if I pause", "for a second.")),
        ("It works.", "So what's left?", "It works.", ("It works.", "so what's left?")),
    ],
)
def test_continue_after_seam(before, phrase, earlier, expected):
    from backend.services.phrase_seams import continue_after_seam

    assert continue_after_seam(before, phrase, earlier) == expected


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
    from backend.services.phrase_seams import continue_phrase

    assert continue_phrase(phrase, earlier, names) == expected


@pytest.mark.parametrize(
    ("before", "expected"),
    [
        ("I think we should", True),
        ("I think we should ", True),
        ("Well, ", True),
        ('He said "no" ', True),
        ("We could — ", True),
        ("", False),
        ("   ", False),
        ("Done. ", False),
        ("Really?", False),
        ("Note: ", False),
        ("Hi,\n", False),
        ("Intro\n- ", False),
    ],
)
def test_continues_sentence(before, expected):
    from backend.services.phrase_seams import continues_sentence

    assert continues_sentence(before) == expected


@pytest.mark.parametrize(
    ("text", "raw", "expected"),
    [
        ("Move the meeting to Friday.", "move the meeting to friday", "move the meeting to Friday."),
        # Cleanup dropped a filler; the word is still found.
        ("Move it.", "um, move it", "move it."),
        ("Morgan agreed.", "Morgan agreed", "Morgan agreed."),
        ("I agree.", "i agree", "I agree."),
        # A word raw doesn't start with is left alone.
        ("Totally different.", "something else", "Totally different."),
        ("already lower", "already lower", "already lower"),
    ],
)
def test_match_raw_start(text, raw, expected):
    from backend.services.phrase_seams import match_raw_start

    assert match_raw_start(text, raw) == expected


def test_mid_sentence_capitals_treats_each_line_as_a_start():
    from backend.services.phrase_seams import mid_sentence_capitals

    assert mid_sentence_capitals("Hi,\nThanks for asking Morgan") == {"Morgan"}


# Capture c5f17de8: a run-on past the settle bound, cleaned as a tail that
# continued the sentence, came back in lowercase after a period it added.
_RUN_ON_RAW = (
    "There are A lot of dictations on Kass now from my work machine and this machine, there has been a couple of "
    "times on my work machine where I was using my AirPods and the output came out really, really poor and then "
    "there are some times where I pause and add a correction and the correction is there, but it didn't actually "
    "remove the previous text that I wanted to remove will you go take a look at the dictations and the outputs "
    "and figure out what else we can do to improve Kass?"
)


def test_keep_raw_capitals_restores_what_cleanup_lowercased():
    cleaned = (
        "There has been a couple of times on my work machine where I was using my AirPods and the output came out "
        "really, really poor. and then there are some times where i pause and add a correction and the correction "
        "is there, but it didn't actually remove the previous text that i wanted to remove will you go take a look "
        "at the dictations and the outputs and figure out what else we can do to improve kass?"
    )
    assert keep_raw_capitals(cleaned, _RUN_ON_RAW) == (
        "There has been a couple of times on my work machine where I was using my AirPods and the output came out "
        "really, really poor. And then there are some times where I pause and add a correction and the correction "
        "is there, but it didn't actually remove the previous text that I wanted to remove will you go take a look "
        "at the dictations and the outputs and figure out what else we can do to improve Kass?"
    )


@pytest.mark.parametrize(
    ("cleaned", "raw", "expected"),
    [
        # Contractions of "I" too.
        ("so i'm in and i'll see", "So I'm in and I'll see", "so I'm in and I'll see"),
        # Abbreviations, ellipses and links are not sentence ends or names.
        ("i.e. the U.S. and etc. and so... and then", "I said", "i.e. the U.S. and etc. and so... and then"),
        ("go to kass.mrgnhnt.com now", "go to Kass now", "go to kass.mrgnhnt.com now"),
        # A quote or a capital inside the word keeps its shape.
        ('Done. "ok" she said. iPhone works.', "I", 'Done. "Ok" she said. iPhone works.'),
        # A capital Whisper gave a common word after a pause isn't a name.
        ("it broke and the fix works", "it broke And the fix works", "it broke and the fix works"),
        # A transcript that writes "i" in lowercase (code) keeps it.
        ("for i in range", "for i in range", "for i in range"),
    ],
)
def test_keep_raw_capitals(cleaned, raw, expected):
    assert keep_raw_capitals(cleaned, raw) == expected
