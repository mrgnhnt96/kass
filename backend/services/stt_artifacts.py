"""What Whisper writes that nobody said, taken out before the worker answers.

Loops Whisper falls into when audio trails off, U+FFFD where it stopped
partway through a character, laughs split into syllables ("ha ha ha") and
spelled letters split apart ("m-r-g-n"). The Rust server runs the rest of
cleanup (kass_text::refinement); this is only what recognition hands it.
"""

import re

_SYLLABLES = r"(?:ha|he)+"

# Two or more laugh syllables (each may already be joined, "haha ha") with only
# spaces, commas, periods or dashes between them.
_LAUGH = re.compile(rf"(?<![\w-]){_SYLLABLES}(?:[ \t,.\-]+{_SYLLABLES})+(?![\w-])", re.IGNORECASE)

# "he" is also a word, and a restart repeats it ("he, he went home"). Only
# "he"s that run into another word must be three or more to be a laugh.
_GOES_ON = re.compile(r"[ \t]*\w")


def is_laughter(text: str) -> bool:
    """Whether ``text`` is nothing but laugh syllables: "hahaha", "HeHe"."""
    return bool(re.fullmatch(_SYLLABLES, text, re.IGNORECASE))


def _join(match: re.Match) -> str:
    laugh = re.sub(r"[^a-zA-Z]", "", match.group())
    if "a" not in laugh.lower() and len(laugh) < 6 and _GOES_ON.match(match.string, match.end()):
        return match.group()
    if laugh.isupper():
        return laugh
    return laugh[0] + laugh[1:].lower()


def join_laughter(text: str) -> str:
    """``text`` with every spaced-out laugh written as one word ("hahaha")."""
    return _LAUGH.sub(_join, text)


_LETTER = r"[^\W\d_]"

# Marks the text spelled letters became, so only runs that start from spelling
# are joined: "CHENEY 0021" was spelled, "iPhone 15" was not.
_SPELLED = "\ue000"

# Three or more single letters joined by dashes. Two letters stay dashed, since
# "A-B testing" and "T-shirt"-style words are written that way on purpose.
_SPELLED_LETTERS = re.compile(rf"(?<![\w-]){_SPELLED}?{_LETTER}(?:-{_LETTER}){{2,}}(?![\w-])")

_SYMBOLS = {
    "underscore": "_",
    "dash": "-",
    "hyphen": "-",
    "exclamation point": "!",
    "exclamation mark": "!",
    "question mark": "?",
    "at sign": "@",
    "dollar sign": "$",
    "percent sign": "%",
    "pound sign": "#",
    "hash": "#",
    "hashtag": "#",
    "ampersand": "&",
    "asterisk": "*",
    "period": ".",
    "dot": ".",
    "slash": "/",
    "plus sign": "+",
}
_SYMBOL_WORD = "|".join(sorted((re.escape(word) for word in _SYMBOLS), key=len, reverse=True))
_SYMBOL_WORDS = re.compile(rf"\b(?:{_SYMBOL_WORD})\b", re.IGNORECASE)

# "capital" said before a letter becomes that letter in upper case: "capital
# c" is "C". Before spelled-out letters it is the first one only, the way a
# name is spelled: "capital C-H-E-N-E-Y" is "Cheney". Before a word it's left
# alone, and so is a letter that is itself a word going on into more words:
# "the capital I visited" is the city, not the letter. A symbol word after the letter
# ("capital A dash") means the speaker is still spelling. "with a capital L"
# asks for the case of a word already said, so it is kept for cleanup
# (kass_text::spoken_case).
_CAPITAL_LETTER = re.compile(
    rf"(?<![Ww]ith a )(?<![Ww]ith an )(?<![Ww]ith )\b[Cc]apital\s+(?![AaIiOo]\s+(?!(?i:{_SYMBOL_WORD})\b){_LETTER})({_LETTER})((?:-{_LETTER})*)(?![\w-])"
)

# One piece of a spelled run: spelled letters, a number, or a symbol said as a
# word or written as itself, with any punctuation Whisper stuck on. Sentence
# punctuation ends the run, so "C-A-T, 3 dogs" doesn't become "CAT,3 dogs".
_PIECE = rf"(?:{_SPELLED}{_LETTER}+|\d+|(?i:{_SYMBOL_WORD})\b|[^\w\s{_SPELLED}])"
_RUN = re.compile(rf"(?<![\w{_SPELLED}])(?:{_PIECE}[^\w\s,;:.?!{_SPELLED}]*[ \t]+)+{_PIECE}[^\w\s{_SPELLED}]*")


def _join_run(match: re.Match) -> str:
    run = match.group()
    if _SPELLED not in run:
        return run
    run = _SYMBOL_WORDS.sub(lambda word: _SYMBOLS[word.group().lower()], run)
    return re.sub(r"[ \t]+", "", run)


def join_spelling(text: str) -> str:
    """Join spelled letters, and the numbers and punctuation said with them."""
    marked = _CAPITAL_LETTER.sub(lambda match: _SPELLED + match.group(1).upper() + match.group(2).lower(), text)
    marked = _SPELLED_LETTERS.sub(lambda match: _SPELLED + match.group().replace("-", "").lstrip(_SPELLED), marked)
    if _SPELLED not in marked:
        return text
    return _RUN.sub(_join_run, marked).replace(_SPELLED, "")


# A run that repeats this many times gets collapsed before the LLM sees
# the transcript. Whisper occasionally loops content hundreds of times
# when audio trails off — "URL URL URL…" (single word), "thanks for
# watching thanks for watching…" (multi-word phrase), or
# "谢谢观看谢谢观看…" (CJK with no spaces). Smaller refine models truncate
# legitimate output to "make room" for the loop, and bigger ones echo
# the run verbatim because "never omit ideas" overrides the no-garbage
# heuristic. Stripping deterministically sidesteps both.
_REPETITION_RUN_THRESHOLD = 6

# Upper bound on the length of a repeating unit that the character-level
# pass will detect. Covers every Whisper hallucination phrase we've
# observed ("Please like and subscribe to my channel." ≈ 41 chars,
# "Subtitles by the Amara.org community" ≈ 36 chars) while being short
# enough that coincidental long-phrase repetition stays below the
# threshold in legitimate speech.
_MAX_REPETITION_UNIT_CHARS = 60


def _token_key(word: str) -> str:
    """Normalize a token for repetition comparison — strip surrounding
    punctuation and lowercase so "URL", "url," and "URL." all compare
    equal inside a loop."""
    return re.sub(r"[^\w]", "", word).lower()


def collapse_repetitive_artifacts(text: str, min_run: int = _REPETITION_RUN_THRESHOLD) -> str:
    """Strip STT-artifact loops. Two passes handle the full space:

    1. Word-level: any token repeated ``min_run``+ times consecutively
       (with surrounding punctuation stripped for comparison). Catches
       single-word loops like "URL URL URL…" and normalizes punctuated
       variants like "URL, URL, URL, URL, URL, URL".
    2. Character-level: any substring 2-60 chars long that repeats
       ``min_run``+ times immediately after itself. Catches multi-word
       English loops ("thanks for watching" x 6) that the word-level
       pass misses (no consecutive identical tokens) and CJK loops
       ("谢谢观看" x 6) where ``text.split()`` yields a single unsplit
       token.

    Both passes preserve rhetorical repetition: "no, no, no, no, no"
    (5 repeats) and "yeah yeah yeah" (3 repeats) stay in the transcript
    because they don't cross the threshold.
    """
    collapsed = _collapse_word_runs(text, min_run)
    collapsed = _collapse_character_runs(collapsed, min_run)
    return collapsed


def strip_stt_artifacts(text: str) -> str:
    """Remove what Whisper writes that nobody said.

    Loops (see ``collapse_repetitive_artifacts``), and U+FFFD, which Whisper
    emits when it stops partway through a multi-byte character, typically
    at the start of a loop ("box the\ufffd, the,R,A,A,A,..."). Also spelled-out
    text and laughs Whisper splits apart (see ``join_spelling``,
    ``join_laughter``).
    Applied to every transcript, so saved captures and the examples made from
    them are clean.
    """
    text = join_laughter(text)
    cleaned = collapse_repetitive_artifacts(text.replace("\ufffd", ""))
    if cleaned != text:
        cleaned = re.sub(r"[ \t]{2,}", " ", cleaned).strip()
    return join_spelling(cleaned)


def _collapse_word_runs(text: str, min_run: int) -> str:
    words = text.split()
    if len(words) < min_run:
        return text

    out: list[str] = []
    i = 0
    while i < len(words):
        key = _token_key(words[i])
        j = i
        # Empty keys (all-punctuation tokens) shouldn't count as a match.
        if key:
            while j < len(words) and _token_key(words[j]) == key:
                j += 1
        else:
            j = i + 1
        run_len = j - i
        if run_len >= min_run:
            # Drop the whole run — the surrounding prose still carries
            # the speaker's thought, and a 6-token repeat almost always
            # means the speech-to-text model glitched.
            pass
        else:
            out.extend(words[i:j])
        i = j

    return " ".join(out)


def _collapse_character_runs(text: str, min_run: int) -> str:
    # Non-greedy unit so the shortest repeating substring wins. Lower
    # bound of 2 chars avoids stripping emphasized single-letter runs
    # ("wooooooow", "hmmmmm") that aren't hallucinations. re.DOTALL so a
    # newline inside a looped unit (rare) doesn't break the match.
    pattern = re.compile(
        r"(.{2," + str(_MAX_REPETITION_UNIT_CHARS) + r"}?)\1{" + str(min_run - 1) + r",}",
        flags=re.DOTALL,
    )
    # A long laugh ("hahahahahaha") is said, not looped.
    result = pattern.sub(lambda run: run.group() if is_laughter(run.group(1)) else "", text)
    if result == text:
        return text
    # Stripping a run leaves double whitespace where the loop used to
    # bridge surrounding context; normalize so the LLM prompt stays
    # clean. Only runs when we actually modified the text so transcripts
    # that didn't hit any loop keep their original whitespace.
    return re.sub(r"\s+", " ", result).strip()
