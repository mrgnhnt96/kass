"""What the worker strips from Whisper's text before it answers.

Loops (``collapse_repetitive_artifacts``), for both the single-word loops the
original algorithm handled and the multi-word / CJK / emoji loops the
character-level pass added, and laughs and spelled letters Whisper splits
apart.
"""

from backend.services.stt_artifacts import (
    collapse_repetitive_artifacts,
    join_laughter,
    join_spelling,
    strip_stt_artifacts,
)

# ── single-word loops (word-level pass) ─────────────────────────────────


def test_single_word_loop_stripped():
    raw = "Hello " + ("URL " * 8).strip() + " goodbye"
    assert collapse_repetitive_artifacts(raw) == "Hello goodbye"


def test_single_word_loop_with_punctuation_normalized():
    # URL, URL, URL, URL, URL, URL. — six repeats if you normalize
    # trailing punctuation; word-level pass strips them all.
    raw = "Hello URL, URL, URL, URL, URL, URL. goodbye"
    assert collapse_repetitive_artifacts(raw) == "Hello goodbye"


def test_single_word_loop_case_insensitive():
    raw = "hi " + " ".join(["Url", "URL", "url", "Url", "URL", "url"]) + " bye"
    assert collapse_repetitive_artifacts(raw) == "hi bye"


def test_short_single_word_run_preserved():
    # Five repeats — below threshold.
    raw = "no no no no no"
    assert collapse_repetitive_artifacts(raw) == raw


def test_rhetorical_repetition_preserved():
    raw = "I said no, no, no, no, no and she left"
    # Five repeats of "no" — below threshold.
    assert collapse_repetitive_artifacts(raw) == raw


# ── multi-word loops (character-level pass) ─────────────────────────────


def test_multi_word_english_loop_stripped():
    # Classic Whisper tail hallucination. Word-level pass sees no
    # consecutive identical tokens, so it's the character-level pass's
    # job to catch this.
    loop = "thanks for watching " * 6
    raw = f"Okay so the meeting is at three. {loop}"
    result = collapse_repetitive_artifacts(raw)
    assert "thanks for watching" not in result
    assert "Okay so the meeting is at three" in result


def test_three_word_loop_stripped():
    loop = "please like and " * 7
    raw = f"The point is clear. {loop}right"
    result = collapse_repetitive_artifacts(raw)
    assert "please like and" not in result
    assert "The point is clear" in result


def test_long_phrase_loop_within_60_char_cap():
    unit = "Please like and subscribe to my channel. "  # 41 chars, within cap
    raw = "End of video. " + unit * 6
    result = collapse_repetitive_artifacts(raw)
    assert unit.strip() not in result
    assert "End of video" in result


def test_multi_word_short_run_preserved():
    # Five repeats of a multi-word unit — below threshold.
    raw = "thanks for watching thanks for watching thanks for watching thanks for watching thanks for watching"
    assert collapse_repetitive_artifacts(raw) == raw


# ── CJK loops (character-level pass, no whitespace) ──────────────────────


def test_cjk_loop_stripped():
    # Common Chinese Whisper hallucination: "thanks for watching".
    # text.split() yields one token for the whole loop; only the
    # character-level pass can catch this.
    prefix = "會議在三點開始"
    loop = "謝謝觀看" * 7
    raw = prefix + loop
    result = collapse_repetitive_artifacts(raw)
    assert "謝謝觀看" not in result
    assert prefix in result


def test_japanese_loop_stripped():
    # Same pattern, kana/kanji mix. "ご視聴ありがとうございました" is a
    # frequent Japanese Whisper tail hallucination.
    loop = "ご視聴ありがとうございました" * 6
    raw = f"明日の会議は午後三時です。{loop}"
    result = collapse_repetitive_artifacts(raw)
    assert "ご視聴ありがとうございました" not in result
    assert "明日の会議は午後三時です" in result


def test_cjk_short_run_preserved():
    # Five repeats — below threshold, stays in.
    raw = "好好好好好"
    assert collapse_repetitive_artifacts(raw) == raw


# ── whitespace / edge cases ──────────────────────────────────────────────


def test_empty_string_passes_through():
    assert collapse_repetitive_artifacts("") == ""


def test_below_word_threshold_passes_through_unmodified():
    raw = "just three words"
    assert collapse_repetitive_artifacts(raw) == raw


def test_emphasis_vowel_run_preserved():
    # "wooooooow" is 1 char (plus 8 o's). Character-level min unit is 2,
    # so "oo…" doesn't get stripped and this legitimate emphasis stays.
    raw = "that's wooooooow amazing"
    assert collapse_repetitive_artifacts(raw) == raw


def test_custom_threshold_honored():
    # With min_run=3, even short rhetorical repetition should now strip.
    raw = "ha ha ha ha context"
    result = collapse_repetitive_artifacts(raw, min_run=3)
    assert "ha ha" not in result
    assert "context" in result


def test_leading_and_trailing_whitespace_stripped_after_collapse():
    # When character pass fires, the normalised result is stripped so
    # downstream prompts don't carry edge whitespace from the removal.
    loop = "loop-phrase " * 7
    raw = loop
    assert collapse_repetitive_artifacts(raw) == ""


def test_stt_artifacts_are_stripped_from_what_whisper_wrote():
    looped = "my teach voice box the�, the,R," + "A," * 110 + " or will it just perpetually get bigger?"
    assert strip_stt_artifacts(looped) == "my teach voice box the, the,R, or will it just perpetually get bigger?"
    # Rhetorical repetition and ordinary text are left alone.
    assert strip_stt_artifacts("no, no, no, no, no") == "no, no, no, no, no"
    assert strip_stt_artifacts("Send  it") == "Send  it"


def test_spelled_out_text_is_joined_in_what_whisper_wrote():
    assert strip_stt_artifacts("m-r-g-n-h-n-t at gmail.com") == "mrgnhnt at gmail.com"


def test_a_laugh_is_written_as_one_word():
    assert join_laughter("ha ha ha") == "hahaha"
    assert join_laughter("Ha, ha, ha. That's great") == "Hahaha. That's great"
    assert join_laughter("that's funny ha ha") == "that's funny haha"
    assert join_laughter("haha ha-ha") == "hahahaha"
    assert join_laughter("HA HA HA") == "HAHAHA"
    assert join_laughter("Ha Ha") == "Haha"


def test_a_laugh_is_spelled_the_way_it_was_heard():
    assert join_laughter("he he he") == "hehehe"
    assert join_laughter("that's sneaky, he he.") == "that's sneaky, hehe."
    assert join_laughter("He, he, he, I win") == "Hehehe, I win"
    assert join_laughter("ha ha he he") == "hahahehe"


def test_he_said_again_is_a_restart_not_a_laugh():
    assert join_laughter("he, he went home") == "he, he went home"
    assert join_laughter("and he he said no") == "and he he said no"


def test_words_that_are_not_a_laugh_are_left_alone():
    assert join_laughter("ha") == "ha"
    assert join_laughter("hah, I knew it") == "hah, I knew it"
    assert join_laughter("Hawaii has a hat") == "Hawaii has a hat"
    assert join_laughter("aha ha") == "aha ha"


def test_a_long_laugh_is_kept_not_taken_for_a_whisper_loop():
    assert strip_stt_artifacts("ha ha ha ha ha ha ha ha that's so good") == "hahahahahahahaha that's so good"
    assert strip_stt_artifacts("hahahahahahaha") == "hahahahahahaha"


def test_spelled_out_letters_lose_the_dashes_whisper_adds():
    assert join_spelling("m-r-g-n-h-n-t at gmail.com") == "mrgnhnt at gmail.com"
    assert join_spelling("It's spelled J-O-S-É.") == "It's spelled JOSÉ."
    # Real hyphenated words and two-letter pairs keep their dashes.
    assert join_spelling("an A-B test on my e-mail and T-shirt") == "an A-B test on my e-mail and T-shirt"
    assert join_spelling("a well-known x-ray") == "a well-known x-ray"


def test_saying_capital_before_a_letter_leaves_just_the_capital_letter():
    assert join_spelling("capital C-H-E-N-E-Y") == "Cheney"
    assert join_spelling("capital c-h-e-n-e-y") == "Cheney"
    assert join_spelling("Plan capital b, then capital C.") == "Plan B, then C."
    assert join_spelling("capital I") == "I"
    assert join_spelling("capital I-B-M stock") == "Ibm stock"
    assert join_spelling("I-B-M stock") == "IBM stock"
    # "capital" before a word is the word itself.
    assert join_spelling("the capital city of Utah") == "the capital city of Utah"
    assert join_spelling("capital-intensive work") == "capital-intensive work"
    # A letter that is a word, going on into a sentence, is that word.
    assert join_spelling("the capital I visited was Paris") == "the capital I visited was Paris"
    assert join_spelling("a capital a lot of people love") == "a capital a lot of people love"


def test_numbers_and_punctuation_said_with_spelling_join_it():
    assert join_spelling("My password is capital C-H-E-N-E-Y 0021!") == "My password is Cheney0021!"
    assert join_spelling("It's capital A 1 2 3") == "It's A123"
    assert join_spelling("user m-r-g-n underscore 96") == "user mrgn_96"
    assert join_spelling("C-A-T 42 exclamation point 7") == "CAT42!7"
    assert join_spelling("0021 C-H-E-N-E-Y") == "0021CHENEY"
    assert join_spelling("capital A dash capital B 7") == "A-B7"


def test_runs_without_spelling_or_across_sentence_punctuation_stay_apart():
    assert join_spelling("I got the iPhone 15 in room 12") == "I got the iPhone 15 in room 12"
    assert join_spelling("Spell C-A-T, 3 dogs ran.") == "Spell CAT, 3 dogs ran."
    assert join_spelling("It's C-A-T. 3 more to go") == "It's CAT. 3 more to go"
    assert join_spelling("A-B-C and 1 2 3") == "ABC and 1 2 3"
