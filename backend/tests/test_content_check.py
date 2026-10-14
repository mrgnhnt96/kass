"""The content check: restructuring passes, possible content changes get a review."""

import pytest

from backend.services.content_check import check, check_refinement, summarize_reviews
from backend.services.refinement import RefinementFlags


@pytest.mark.parametrize(
    ("said", "cleaned"),
    [
        ("so I was thinking we should, we should push it", "I was thinking we should push it."),
        ("the tests the tests aren't done", "The tests aren't done."),
        ("if I am making 180,000 a year", "If I'm making $180,000 a year."),
        ("no wait I meant Friday", "I meant Friday."),
    ],
)
def test_restructuring_passes(said, cleaned):
    assert check(said, cleaned).outcome == "ok"


def test_a_word_in_place_of_a_heard_one_is_listed_for_review():
    verdict = check("commit and merge domain", "Commit and merge to main.")
    assert (verdict.outcome, verdict.added, verdict.missing) == ("review", ["main"], ["domain"])


@pytest.mark.parametrize(
    ("said", "cleaned", "invented"),
    [
        ("send the notes", "Send the notes and the slides.", ["slides"]),
        ("Merge.", "Merge to main", ["main"]),
        ("commit, PR, merge", "Commit, PR, merge to main", ["main"]),
        ("wording", "Would you like me to refine the wording?", ["refine"]),
    ],
)
def test_a_word_added_between_heard_words_is_rejected(said, cleaned, invented):
    verdict = check(said, cleaned)
    assert (verdict.outcome, verdict.reason, verdict.added) == ("reject", "invented", invented)


def test_a_fixed_mishearing_of_a_small_word_is_kept():
    assert check("merge it to me", "Merge it to main.").outcome == "review"


def test_dropping_much_of_what_was_said_is_rejected():
    verdict = check("book flights hotel and a car for the trip", "Book flights for the trip.")
    assert (verdict.outcome, verdict.reason, verdict.missing) == ("reject", "dropped", ["car", "hotel"])


def test_a_dropped_request_in_a_long_dictation_is_rejected():
    # Under MISSING_SHARE, so this used to pass as ok and paste without the second request.
    said = (
        "check the build server and see how far along the release is and then look through the open pull "
        "requests for comments and tell me if anyone is waiting on a reply from us before Friday"
    )
    cleaned = "Check the build server and see how far along the release is before Friday."
    verdict = check(said, cleaned)
    assert (verdict.outcome, verdict.reason) == ("reject", "dropped")
    assert {"comments", "reply", "waiting"} <= set(verdict.missing)


def test_one_dropped_hedge_in_a_short_dictation_is_not_rejected():
    # One word is a third of this take's content; DROPPED_WORDS keeps it a review, as before.
    assert check("I think the draft is ready", "The draft is ready.").outcome == "review"


def test_retracted_words_do_not_count_as_dropped():
    verdict = check("move it to the blue folder actually the green folder", "Move it to the green folder.", True)
    assert verdict.outcome == "ok"


def test_swapping_who_does_what_is_rejected():
    verdict = check(
        "can you find the API keys so that you can pull the report for me",
        "Can you find the API keys so that I can pull the report for you?",
    )
    assert (verdict.outcome, verdict.reason) == ("reject", "person")


def test_rewording_a_pronoun_is_not_a_swap():
    assert check("can you send me the notes", "Send me the notes.").reason is None
    assert check("I want you to send the notes", "Please send the notes.").reason is None


def test_answering_instead_of_cleaning_is_rejected():
    verdict = check("what is the capital of France", "The capital of France is Paris, a city famous for art.")
    assert (verdict.outcome, verdict.reason) == ("reject", "answered")


def test_a_cleanup_mostly_of_new_words_is_rejected():
    verdict = check("Why is it?", "Because I'm pretty happy with how this is working.")
    assert (verdict.outcome, verdict.reason) == ("reject", "answered")


@pytest.mark.parametrize(
    ("said", "cleaned"),
    [
        ("Is this working better?", "Yes, it's working better."),
        ("Can you merge it?", "Yes, I can merge it."),
        ("Why is it?", "Because I'm pretty happy with how this is working."),
    ],
)
def test_answering_a_question_is_rejected(said, cleaned):
    # A question that comes back as a statement was answered, not cleaned,
    # even when the answer reuses the speaker's words.
    assert (check(said, cleaned).outcome, check(said, cleaned).reason) == ("reject", "answered")


def test_a_question_cleaned_up_stays_a_question():
    assert check("is this working better", "Is this working better?").outcome == "ok"
    assert check("so um is this working better?", "Is this working better?").outcome == "ok"


def test_replying_to_a_short_dictation_is_rejected():
    # Keeping none of the speaker's words is a reply, however short.
    verdict = check("Thank you.", "You're welcome.")
    assert (verdict.outcome, verdict.reason) == ("reject", "answered")


def test_a_short_dictation_cleaned_up_keeps_its_word():
    assert check("thanks", "Thanks!").outcome == "ok"


def test_self_correction_may_drop_the_retracted_words():
    flags = RefinementFlags()
    text, verdict = check_refinement(
        "the meeting is on Tuesday actually Wednesday", "The meeting is on Wednesday.", flags
    )
    assert (text, verdict.outcome) == ("The meeting is on Wednesday.", "ok")


def test_summary_takes_the_worst_outcome_and_joins_words():
    reviews = [check("merge domain", "Merge to main."), check("do not ship", "Ship.")]
    assert summarize_reviews(reviews) == {
        "outcome": "reject",
        "added": ["main"],
        "missing": ["domain", "not"],
        "reasons": ["negation"],
    }
    assert summarize_reviews([check("hello", "Hello.")]) is None


WALMART = (
    "Alright, let's create a list of things that we need to do. First, we need to drive home, "
    "then we need to drive to school, and then we have to go to Walmart."
)


def test_formatting_the_cleanup_adds_is_reviewed_not_rejected():
    cleaned = (
        "Alright, let's create a list of things that we need to do:\n"
        "1. Drive home\n2. Drive to school\n3. Go to Walmart"
    )
    verdict = check(WALMART, cleaned)
    assert verdict.outcome == "review"
    assert verdict.added == ["1", "2", "3"]


def test_a_number_that_was_said_must_survive():
    verdict = check("buy 2 apples and 3 pears", "1. Buy 2 apples\n2. Buy 4 pears")
    assert (verdict.outcome, verdict.reason, verdict.missing) == ("reject", "number", ["3"])


def test_a_name_that_was_said_must_survive():
    verdict = check("send the draft to Priya before lunch", "Send the draft to her before lunch.")
    assert (verdict.outcome, verdict.reason, verdict.missing) == ("reject", "name", ["priya"])


def test_sentence_capitals_are_not_names():
    assert check("Okay. Send the draft. I think it's ready", "Send the draft. It's ready.").reason is None


def test_a_technical_term_that_was_said_must_survive():
    verdict = check("open package.json first", "Open the config file first.")
    assert (verdict.outcome, verdict.reason) == ("reject", "technical")


def test_adding_a_not_is_rejected():
    verdict = check("ship it today", "Don't ship it today.")
    assert (verdict.outcome, verdict.reason) == ("reject", "negation")


def test_a_retracted_name_may_be_dropped():
    verdict = check("send it to Bob actually Alice", "Send it to Alice.", allow_retractions=True)
    assert verdict.outcome != "reject"


def test_a_name_said_after_the_correction_must_survive():
    verdict = check("send it to Bob actually Alice", "Send it to Bob.", allow_retractions=True)
    assert (verdict.outcome, verdict.reason, verdict.missing) == ("reject", "name", ["alice"])


def test_a_capitalized_restart_is_not_a_name():
    assert check("How does it look now? It might But we'll see", "How does it look now? We'll see.").reason is None


def test_copying_an_example_the_speaker_did_not_say_is_rejected():
    # A real case: the newest taught example leaked into the next dictation.
    said = "I'm sure we could do some compression and the bundle size is probably not that big."
    refined = "New line, what else can we improve?\nI'm sure we could do some compression and the bundle size is probably not that big."
    examples = [("New line, what else can we improve?", "What else can we improve?")]
    text, verdict = check_refinement(said, refined, RefinementFlags(), examples=examples)
    assert (verdict.outcome, verdict.reason) == ("reject", "copied example")
    assert text.startswith("I'm sure we could")


def test_saying_words_that_also_appear_in_an_example_is_fine():
    said = "what else can we improve on the bundle"
    refined = "What else can we improve on the bundle?"
    examples = [("New line, what else can we improve?", "What else can we improve?")]
    _, verdict = check_refinement(said, refined, RefinementFlags(), examples=examples)
    assert verdict.outcome == "ok"


def test_short_formatting_additions_are_not_mistaken_for_copying():
    said = "go to school come home do chores"
    refined = "1. Go to school\n2. Come home\n3. Do chores"
    examples = [("list go to the store go home", "1. Go to the store\n2. Go home")]
    _, verdict = check_refinement(said, refined, RefinementFlags(), examples=examples)
    assert verdict.outcome != "reject"


def test_a_cleanup_that_drops_a_spoken_mark_is_rejected():
    assert check('he said "hi (sort of)"', "He said hi, sort of.").outcome == "reject"
    assert check("and/or ls | wc", "And or ls wc.").outcome == "reject"
    assert check('he said "hi"', "He said “hi.”").outcome == "ok"
