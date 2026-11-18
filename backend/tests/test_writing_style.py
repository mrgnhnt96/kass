"""Learning punctuation habits from taught replies and applying them."""

import pytest

from backend import config
from backend.services import writing_style
from backend.services.refinement import (
    REFINEMENT_EXAMPLES,
    RefinementFlags,
    build_refinement_prompt,
    refine_transcript,
    refinement_examples,
)

CASUAL = {
    "boundary": "comma",
    "lowercase_start": False,
    "drop_intro_comma": True,
    "drop_conjunction_comma": True,
    "drop_final_period": True,
}


@pytest.fixture(autouse=True)
def profile(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(writing_style, "_state", None)


def test_observe_counts_each_kind_of_choice():
    shown = "Yeah, that works. I can go at seven, but traffic is bad. Do you want food?"
    written = "yeah that works, I can go at seven but traffic is bad. do you want food?"
    counts = writing_style.observe(shown, written)
    assert counts["boundary"] == {"period": 1, "comma": 1, "none": 0}
    assert counts["intro_comma"] == {"kept": 0, "dropped": 1}
    assert counts["conjunction_comma"] == {"kept": 0, "dropped": 1}
    assert counts["lowercase_start"] == {"yes": 2, "no": 0}


def test_observe_ignores_words_the_user_rewrote():
    counts = writing_style.observe("It works. Ship it.", "Totally different words here")
    assert counts["boundary"] == {"period": 0, "comma": 0, "none": 0}


def test_final_period_is_counted_only_at_the_end():
    counts = writing_style.observe("That works for me.", "That works for me")
    assert counts["final_period"] == {"kept": 0, "dropped": 1}


def test_a_period_turned_into_a_question_mark_is_not_dropped():
    counts = writing_style.observe("Is it done.", "Is it done?")
    assert counts["final_period"] == {"kept": 0, "dropped": 0}


def test_a_period_added_at_the_end_is_kept():
    counts = writing_style.observe("That works for me", "That works for me.")
    assert counts["final_period"] == {"kept": 1, "dropped": 0}


def test_a_correction_that_only_fixes_words_says_nothing_about_punctuation():
    counts = writing_style.observe("Command and push. Then stop.", "Commit and push. Then stop.", deliberate=False)
    assert counts == writing_style._empty_counts()


def test_a_correction_that_changes_punctuation_counts_what_it_left_too():
    counts = writing_style.observe("It works. Ship it.", "It works, ship it.", deliberate=False)
    assert counts["boundary"] == {"period": 0, "comma": 1, "none": 0}
    assert counts["final_period"] == {"kept": 1, "dropped": 0}


def test_decide_needs_evidence_and_breaks_ties_toward_the_period():
    one = writing_style.observe("It works. Ship it.", "It works, ship it.")
    assert writing_style.decide(one)["boundary"] is None
    tie = writing_style._merge(one, writing_style.observe("It works. Ship it.", "It works. Ship it."))
    assert writing_style.decide(tie)["boundary"] == "period"


def test_apply_style_changes_only_punctuation_and_first_letters():
    text = "Quick update. The fix is merged, and QA is next. So we ship Thursday."
    styled = writing_style.apply_style(text, CASUAL)
    assert styled == "Quick update, the fix is merged and QA is next, so we ship Thursday"
    assert [w.strip(",.").casefold() for w in styled.split()] == [w.strip(",.").casefold() for w in text.split()]


def test_apply_style_keeps_i_acronyms_line_breaks_and_questions():
    text = "It works. I tested it. API calls pass.\nNext. Is it done? Yes."
    styled = writing_style.apply_style(text, {**CASUAL, "drop_final_period": False})
    assert styled == "It works, I tested it, API calls pass.\nNext, is it done? Yes."


def test_standard_habits_leave_text_alone():
    habits = writing_style.decide(writing_style._empty_counts())
    assert writing_style.apply_style("It works. Ship it.", habits) == "It works. Ship it."


# Stand-ins for Kass's cleanup of dictated replies, written the way
# Standard punctuates so what the user sent carries punctuation habits.
CLEANED = [
    "Yeah, that works. I can get there at seven, but traffic is bad. Do you want food?",
    "Okay, quick update. The fix is merged. QA is next, and it looks good.",
    "Hey, when is the report due? I thought Friday, but someone said Wednesday.",
    "First, pull the changes. Then run the script, and start the server.",
    "So, the page is slow. It loads everything at once, and most is hidden.",
]


def _rewrite(text):
    return writing_style.apply_style(text, CASUAL)


def _run(rewrite=_rewrite):
    """A finished teach session whose dictated replies were cleaned up as CLEANED."""
    examples = [{"said": text.casefold(), "shown": text, "written": rewrite(text)} for text in CLEANED]
    return writing_style.save_run(None, examples)


def test_a_saved_run_learns_habits_and_keeps_examples():
    assert not writing_style.is_ready()
    status = _run()
    assert status["ready"]
    assert status["runs"] == 1
    assert status["example_count"] == 5
    assert "boundary_comma" in status["habits"]
    assert (config.get_data_dir() / "writing-style.json").is_file()


def test_typed_replies_are_kept_but_teach_nothing():
    status = writing_style.save_run(None, [{"said": None, "shown": "yep, sounds good", "written": "yep, sounds good"}])
    assert status["example_count"] == 1
    assert writing_style.calibration_examples() == []
    assert writing_style.prompt_example() is None


def test_an_empty_run_is_refused():
    # Resetting a style is the Rust server's (tests/learned.rs).
    with pytest.raises(ValueError, match="Reply at least once"):
        writing_style.save_run(None, [])


def _learn_casual():
    _run()


def test_learned_style_has_its_own_prompt_and_example():
    learned = RefinementFlags(punctuation_style="learned")
    # Nothing learned yet: Match my writing punctuates like Standard.
    assert build_refinement_prompt(learned) == build_refinement_prompt(RefinementFlags())
    assert refinement_examples(learned) is REFINEMENT_EXAMPLES
    _learn_casual()
    prompt = build_refinement_prompt(learned)
    assert "Punctuation style: match how this speaker writes." in prompt
    assert "- Join related thoughts with commas instead of starting new sentences." in prompt
    assert '- Do not put a comma before "and", "but", "so" or "or".' in prompt
    assert "written prose" not in prompt
    assert "Punctuation style: casual." not in prompt
    said, written = refinement_examples(learned)[0]
    latest = writing_style._profile(None)["examples"][-1]
    assert (said, written) == (latest["said"], latest["written"])
    assert refinement_examples(learned)[1:] == REFINEMENT_EXAMPLES


def test_unedited_replies_give_no_example():
    _run(rewrite=lambda text: text)
    assert writing_style.prompt_example() is None


class _Backend:
    model_size = "0.6B"

    async def generate(self, **_):
        return "Quick update. The fix is merged, and QA is next."


@pytest.mark.asyncio
async def test_learned_style_restyles_refined_text():
    _learn_casual()
    text, _ = await refine_transcript(
        "quick update the fix is merged and qa is next",
        RefinementFlags(punctuation_style="learned"),
        backend_override=_Backend(),
        use_personal_model=False,
    )
    assert text == "Quick update, the fix is merged and QA is next"
