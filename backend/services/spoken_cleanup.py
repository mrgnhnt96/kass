"""Resolve the mechanical patterns of speech before refinement.

Repeats, restarts, changed answers and refined answers each have a shape that
can be rewritten the same way every time. Each rule fires on a tight shape or
not at all, and only ever deletes words the speaker said twice or replaced;
anything ambiguous is left for the refinement model.
"""

import re

_TOKEN = re.compile(r"[\w$%]+(?:['\u2019.:][\w%]+)*")


def _word_set(words: str) -> frozenset[str]:
    return frozenset(words.split())


# Words speakers stutter on. Content words and genuine doubles ("very, very",
# "no, no, no", "had had", "that that", "is is") are deliberately absent.
_STUTTER = _word_set(
    "a an the this these those my your our their his her its some any to of in on at "
    "for with from by about into i you we they he she it me us them and but or "
    "because if when was are were can will would should could do did have has i'm "
    "it's we're they're you're there's i've we've i'll we'll"
)
# Words a finished phrase cannot end on, so a fragment ending on one was abandoned.
# Not "a": in "item A, item B" it is a label, not an article.
_DANGLING = _word_set("the my your our their his her its and but or because")
# Verbs that need what is said or asked after them: "I just want to say," was abandoned.
_NEEDS_OBJECT = _word_set("say ask tell mention add explain")
_SUBORDINATORS = _word_set("when if because while where since until once unless whether")
_PRONOUNS = _word_set("i you we they he she it")
_AUX = _word_set("am is are was were will would can could did does do has have had 's 're 'm 've 'll 'd")
_CLAUSE_OPENERS = _word_set("because and but so that when if since while although though")
_HEDGES = _word_set("probably maybe")


def _norm(token: str) -> str:
    return token.replace("\u2019", "'").lower()


def _tokens(text: str) -> list[re.Match]:
    return list(_TOKEN.finditer(text))


def _gap(text: str, tokens: list[re.Match], index: int) -> str:
    """What separates token ``index`` from the next: "" (space), "," or other punctuation."""
    return text[tokens[index].end() : tokens[index + 1].start()].strip()


def _clause_start(text: str, tokens: list[re.Match], index: int) -> int:
    start = index
    while start > 0 and not _gap(text, tokens, start - 1):
        start -= 1
    return start


def _clause_end(text: str, tokens: list[re.Match], index: int) -> int:
    end = index
    while end + 1 < len(tokens) and not _gap(text, tokens, end):
        end += 1
    return end


def _drop(text: str, start: int, end: int) -> str:
    """Delete ``text[start:end]``, keeping a capital the deleted words started with."""
    kept = text[end:]
    if text[start : start + 1].isupper() and kept[:1].islower():
        kept = kept[:1].upper() + kept[1:]
    return text[:start] + kept


def _immediate_repeat(text: str, tokens: list[re.Match]) -> str | None:
    """Drop a stuttered word: "look at the, the budget" -> "look at the budget"."""
    for i in range(len(tokens) - 1):
        word = _norm(tokens[i].group())
        if tokens[i].group() == "A":
            continue  # a label or grade ("A, A, B"), not the article
        if word in _STUTTER and _gap(text, tokens, i) in ("", ",") and _norm(tokens[i + 1].group()) == word:
            return _drop(text, tokens[i].start(), tokens[i + 1].start())
    return None


def _repeated_restart(text: str, tokens: list[re.Match]) -> str | None:
    """Drop an abandoned start: "I can, I can probably" -> "I can probably"."""
    for i in range(len(tokens) - 1):
        if _gap(text, tokens, i) != ",":
            continue
        start = _clause_start(text, tokens, i)
        end = _clause_end(text, tokens, i + 1)
        before = [_norm(t.group()) for t in tokens[start : i + 1]]
        after = [_norm(t.group()) for t in tokens[i + 1 : end + 1]]
        for size in range(min(len(before), len(after) - 1, 6), 1, -1):
            if before[-size:] == after[:size]:
                return _drop(text, tokens[i + 1 - size].start(), tokens[i + 1].start())
        if len(after) < 2:
            continue
        for j in range(i - 1, max(start, i - 3) - 1, -1):
            if _norm(tokens[j].group()) != after[0]:
                continue
            fragment = before[j - start :]
            abandoned = fragment[-1] in _DANGLING or (
                len(fragment) == 2 and fragment[0] in _SUBORDINATORS and fragment[1] in _PRONOUNS
            )
            if abandoned and fragment != after[: len(fragment)]:
                return _drop(text, tokens[j].start(), tokens[i + 1].start())
            break
        # A longer start, abandoned and begun again with the same words:
        # "I just want to say, I just want a dialogue" -> "I just want a dialogue".
        # "say" can end a phrase ("I told you to ask"), so it needs more words in common.
        shared = 2 if before[-1] in _DANGLING else 3 if before[-1] in _NEEDS_OBJECT else 0
        if not shared or len(after) <= shared:
            continue
        for j in range(i - 2, max(start, i - 6) - 1, -1):
            fragment = before[j - start :]
            if fragment[:shared] == after[:shared] and fragment != after[: len(fragment)]:
                return _drop(text, tokens[j].start(), tokens[i + 1].start())
    return None


def _reworded_restart(text: str, tokens: list[re.Match]) -> str | None:
    """Drop a phrase said again with new middle words: "that we can do, that we need to do it" -> "that we need to do it".

    The second attempt opens with the same two words, ends the phrase on the same
    word and carries on past it, so it replaces the first rather than listing a
    second thing ("what we built, what we shipped").
    """
    words = [_norm(t.group()) for t in tokens]
    for i in range(len(tokens) - 1):
        if _gap(text, tokens, i) != ",":
            continue
        start = _clause_start(text, tokens, i)
        end = _clause_end(text, tokens, i + 1)
        after = words[i + 1 : end + 1]
        for j in range(i - 2, max(start, i - 6) - 1, -1):
            fragment = words[j : i + 1]
            if fragment[:2] != after[:2] or fragment == after[: len(fragment)]:
                continue
            last = next(
                (m for m in range(2, min(len(after) - 1, len(fragment) + 2)) if after[m] == fragment[-1]),
                None,
            )
            if last is not None:
                return _drop(text, tokens[j].start(), tokens[i + 1].start())
            break
    return None


_DETERMINERS = _word_set("the a an this these those my your our their his her its some any")
_PREPOSITIONS = _word_set("for to of in on at with from by about into")


def _plural(word: str) -> bool:
    return len(word) > 3 and word.endswith("s") and not word.endswith(("ss", "us", "is"))


def _unpaused_restart(text: str, tokens: list[re.Match]) -> str | None:
    """Drop a restart with no pause: "the tasks for tasks that" -> "the tasks that".

    "X for X" idioms ("word for word", "one on one", "end to end") repeat a singular
    noun, so only a plural one is taken as a restart.
    """
    words = [_norm(t.group()) for t in tokens]
    for i in range(1, len(tokens) - 2):
        noun = words[i]
        if (
            words[i - 1] not in _DETERMINERS
            or words[i + 1] not in _PREPOSITIONS
            or words[i + 2] != noun
            or noun in _STUTTER
            or not _plural(noun)
            or _gap(text, tokens, i - 1)
            or _gap(text, tokens, i)
            or _gap(text, tokens, i + 1) not in ("", ",")
            or i + 3 >= len(tokens)
            or _gap(text, tokens, i + 2)
        ):
            continue
        return _drop(text, tokens[i].start(), tokens[i + 2].start())
    return None


def _subject(token: str) -> tuple[str, str] | None:
    """("it", "'s") for "it's", ("it", "") for "it"; None when not a pronoun subject."""
    base, _, clitic = _norm(token).partition("'")
    if base not in _PRONOUNS:
        return None
    return base, f"'{clitic}" if clitic else ""


def _stem(word: str) -> str:
    for suffix in ("ing", "ed", "es", "s"):
        if word.endswith(suffix) and len(word) - len(suffix) >= 3:
            word = word[: -len(suffix)]
            break
    return word.rstrip("e")


def _subject_verb(words: list[str], at: int) -> tuple[str, int] | None:
    """The subject at ``at`` and the index of its main verb, skipping one auxiliary."""
    subject = _subject(words[at]) if at < len(words) else None
    if subject is None:
        return None
    verb = at + 1
    if not subject[1] and verb < len(words) and words[verb] in _AUX:
        verb += 1
    if verb >= len(words) or words[verb] in _AUX:
        return None
    return subject[0], verb


def _length(words: list[str]) -> int:
    """Words as spoken, so "it's loading" is three."""
    return len(words) + sum("'" in word for word in words)


def _stuttered_clause(text: str, tokens: list[re.Match]) -> str | None:
    """Keep the shorter clause: "it loads, it's loading everything" -> "it loads everything".

    Contractions count as two words, and on a tie the restated clause wins.
    """
    words = [_norm(t.group()) for t in tokens]
    for i in range(len(tokens) - 1):
        if _gap(text, tokens, i) != ",":
            continue
        end = _clause_end(text, tokens, i + 1)
        second = _subject_verb(words, i + 1)
        if second is None or second[1] >= end:
            continue
        start = _clause_start(text, tokens, i)
        for at in (i - 1, i - 2):
            if at < start:
                break
            first = _subject_verb(words, at)
            if first is None or first[1] != i or first[0] != second[0]:
                continue
            if at > start and words[at - 1] not in _CLAUSE_OPENERS:
                break
            first_verb, second_verb = words[i], words[second[1]]
            if _stem(first_verb) != _stem(second_verb) or words[at:i] == [*words[i + 1 : second[1]], first_verb]:
                break
            if _length(words[at : i + 1]) < _length(words[i + 1 : second[1] + 1]):
                return text[: tokens[i].end()] + text[tokens[second[1]].end() :]
            return _drop(text, tokens[at].start(), tokens[i + 1].start())
    return None


# --- Changed answers --------------------------------------------------------------

_WEEKDAYS = "monday|tuesday|wednesday|thursday|friday|saturday|sunday"
_MONTHS = "january|february|march|april|may|june|july|august|september|october|november|december"
_NUMBER_WORDS = (
    "zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|"
    "sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred|"
    "thousand|noon|midnight"
)
_VALUE_KINDS = (
    ("weekday", re.compile(rf"(?:{_WEEKDAYS})(?:\s+(?:morning|afternoon|evening|night))?\b", re.I)),
    ("month", re.compile(rf"(?:{_MONTHS})(?:\s+\d{{1,2}}(?:st|nd|rd|th)?)?\b", re.I)),
    (
        "number",
        re.compile(
            r"(?:\$?\d+(?:[.,:]\d+)*%?|(?:" + _NUMBER_WORDS + r")(?:[\s-](?:" + _NUMBER_WORDS + r"))*)"
            r"(?:\s*(?:am|pm|a\.m\.|p\.m\.|o'clock))?(?!\w|\.\d)",
            re.I,
        ),
    ),
    ("name", re.compile(r"[A-Z][a-z]+\b")),
)
_NOT_NAMES = frozenset({"I", *(w.title() for w in f"{_WEEKDAYS}|{_MONTHS}".split("|"))})
# The cue may follow a sentence break too: speech-to-text often hears
# "noon? No, actually, one" rather than "noon, no actually one". Where the
# speaker paused there, streamed dictation drops the mark at the seam
# and only the capital it gave the cue is left: "noon No, actually one".
_PAUSED_CUE = r"\s+(?=(?-i:(?:Or|No|Sorry|Actually)\b))"
_ANSWER_CUE = re.compile(
    rf"(?:[,.?!]\s*|{_PAUSED_CUE})(?:or\s+(?:was|is)\s+it|(?:no|sorry)(?:\s*,?\s*(?:actually|wait|I\s+mean))?|actually|I\s+mean)\s*,?\s*",
    re.I,
)
# A value followed by a verb starts a new clause ("5 is too many"), not an answer.
_AUX_AFTER = re.compile(r"\s+(?:is|are|was|were|has|have|had|will|would|can|could|should|does|did)\b", re.I)
# Names take any verb ("Sarah knows"); a number is followed by plural nouns ("4.5 seconds").
_VERB_AFTER_NAME = re.compile(_AUX_AFTER.pattern + r"|\s+\w+(?:ed|s)\b", re.I)
_TAIL = re.compile(rf"(?:\s+[a-z]+){{0,3}}?(?=[,.?!]|{_PAUSED_CUE})")


def _value_at(text: str, pos: int) -> tuple[str, re.Match] | None:
    if pos > 0 and (text[pos - 1].isalnum() or text[pos - 1] in "$.'\u2019"):
        return None
    for kind, pattern in _VALUE_KINDS:
        match = pattern.match(text, pos)
        if match and not (kind == "name" and match.group() in _NOT_NAMES):
            return kind, match
    return None


def _changed_answer(text: str) -> str | None:
    """Keep the final answer: "said Wednesday, or was it Tuesday, no Wednesday" -> "said Wednesday"."""
    for start in range(len(text)):
        first = _value_at(text, start)
        if first is None:
            continue
        kind, value = first
        if kind == "name" and (start == 0 or re.search(r"[.!?]\s*$", text[:start])):
            continue  # capitalized because it starts a sentence
        tail = _TAIL.match(text, value.end())
        if tail is None:
            continue
        answers = []
        pos = tail.end()
        while cue := _ANSWER_CUE.match(text, pos):
            answer = _value_at(text, cue.end())
            if answer is None or answer[0] != kind:
                break
            if cue.group().strip(" ,.?!").lower() == "no" and answer[1].group().lower().startswith("one"):
                break  # "no one"
            answers.append(answer[1])
            pos = answer[1].end()
        if not answers:
            continue
        last = answers[-1]
        if (kind == "name" and _VERB_AFTER_NAME.match(text, last.end())) or (
            kind == "number" and _AUX_AFTER.match(text, last.end())
        ):
            continue
        if " ".join(last.group().lower().split()) == " ".join(value.group().lower().split()):
            return text[: tail.end()] + text[last.end() :]
        if tail.end() == value.end():
            return text[:start] + text[last.start() :]
    return None


def _refined_answer(text: str, tokens: list[re.Match]) -> str | None:
    """Keep the fuller answer: "ship Thursday, well Thursday morning probably" -> "ship Thursday morning"."""
    words = [_norm(t.group()) for t in tokens]
    for i in range(len(tokens) - 2):
        if _gap(text, tokens, i) != "," or words[i + 1] != "well" or _gap(text, tokens, i + 1) not in ("", ","):
            continue
        after = i + 2
        if words[after] in _HEDGES and after + 1 < len(tokens) and not _gap(text, tokens, after):
            after += 1
        start = _clause_start(text, tokens, i)
        end = _clause_end(text, tokens, after)
        for size in range(min(i + 1 - start, 3), 0, -1):
            if words[i + 1 - size : i + 1] != words[after : after + size] or after + size > end:
                continue
            result = text
            if words[end] in _HEDGES and end > after + size:
                result = result[: tokens[end - 1].end()] + result[tokens[end].end() :]
            return _drop(result, tokens[i + 1 - size].start(), tokens[after].start())
    return None


def apply_spoken_cleanup(text: str) -> str:
    """Remove repeats, restarts and replaced answers; return ``text`` unchanged if none."""
    for _ in range(50):
        tokens = _tokens(text)
        cleaned = (
            _immediate_repeat(text, tokens)
            or _repeated_restart(text, tokens)
            or _reworded_restart(text, tokens)
            or _unpaused_restart(text, tokens)
            or _stuttered_clause(text, tokens)
            or _changed_answer(text)
            or _refined_answer(text, tokens)
        )
        if cleaned is None:
            break
        text = cleaned
    return text
