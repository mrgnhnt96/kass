"""Resolve explicit local clause revisions before generative refinement."""

import re

_CUE = re.compile(
    r"\b(?:no(?:[,\s]+no)+(?:[,\s]+(?:actually|wait))?|"
    r"no\s*,?\s*(?:actually|wait)|wait\s*,?\s*no|(?:sorry\s*,?\s*)?I meant?|or rather)\b[\s,]*",
    re.I,
)
_WORD = re.compile(r"\w+(?:['\u2019]\w+)*")
# Split coordinated clauses, but not objects such as "mac and cheese".
_COORDINATE = re.compile(r"\b(?:and|but|or)\s+(?=(?:I|we|you|he|she|they|it|the)\b)", re.I)

_INTRO = re.compile(r"(?:alright|all right|okay|ok|well|so)\s*,\s*", re.I)
_COPULA = re.compile(r"\b(is|was|are|were)\s+", re.I)
_PRONOUN_VALUE = re.compile(
    r"^(it['\u2019]s|that['\u2019]s|they['\u2019]re|(?:it|that|they)\s+(?:is|are|was|were))\s+(\S.*)$",
    re.I | re.S,
)
_PRONOUN_VERBS = {
    "it's": "is",
    "that's": "is",
    "it is": "is",
    "that is": "is",
    "it was": "was",
    "that was": "was",
    "they're": "are",
    "they are": "are",
    "they were": "were",
}


def _replace_pronoun_value(clause: str, revision: str) -> str | None:
    """Resolve 'X is A; no, no, it's B' when one local predicate is available."""
    restated = _PRONOUN_VALUE.match(revision)
    copulas = list(_COPULA.finditer(clause))
    if not restated or len(copulas) != 1:
        return None
    copula = copulas[0]
    pronoun = " ".join(restated[1].lower().replace("\u2019", "'").split())
    if _PRONOUN_VERBS.get(pronoun) != copula[1].lower():
        return None
    head, previous = clause[: copula.end()], clause[copula.end() :]
    value = restated[2]
    if not previous or re.match(r"(?:not|also)\b", value, re.I) or re.match(r"not\b", previous, re.I):
        return None
    if re.search(r"\b(?:said|says|asked|wrote|told|there)\b", head, re.I):
        return None
    return head + value


def _words(text: str) -> list[str]:
    return [match.group().replace("\u2019", "'").lower() for match in _WORD.finditer(text)]


def apply_spoken_corrections(text: str) -> str | None:
    """Replace a final clause when an explicit cue repeats its subject/predicate.

    Accept a shared two-word anchor, or a pronoun restating a single local
    copular value. Never search past an intervening sentence/clause.
    Quoted speech and ambiguous cues are left to ordinary refinement.
    """
    if any(mark in text for mark in ('"', "\u201c", "\u201d", "\u2018")) or re.search(r"(?<!\w)['\u2019](?=\w)", text):
        return None
    result = text
    changed = False
    for _ in range(20):
        cue = _CUE.search(result)
        if not cue:
            return result if changed else None
        before = result[: cue.start()].rstrip(" \t\r\n.,;:!?—-")
        after = result[cue.end() :].strip()
        boundaries = list(re.finditer(r"[.!?;\n]\s*", before))
        start = boundaries[-1].end() if boundaries else 0
        clauses = list(_COORDINATE.finditer(before, start))
        if clauses:
            start = clauses[-1].end()
        intro = _INTRO.match(before, start)
        if intro:
            start = intro.end()
        old_clause = before[start:]
        # A remaining conjunction/subclause may contain another fact. Without
        # a reliable clause boundary, let normal refinement interpret it.
        if re.search(
            r"\b(?:and|but|or|while|whereas|although|because|which|who)\b|,\s*(?:I|we|you|he|she|they|it|my|your|our|the)\b",
            old_clause,
            re.I,
        ):
            return None
        pronoun_replacement = _replace_pronoun_value(old_clause, after)
        if pronoun_replacement is not None:
            # Multiple coordinated clauses can provide competing antecedents.
            if clauses:
                return None
            result = before[:start] + pronoun_replacement
            changed = True
            continue
        old_words = _words(old_clause)
        new_words = _words(after)
        shared = 0
        for old, new in zip(old_words, new_words, strict=False):
            if old != new:
                break
            shared += 1
        if shared < 2 or shared >= min(len(old_words), len(new_words)):
            return None
        replacement = after
        if start == 0 or (boundaries and start == boundaries[-1].end()):
            replacement = replacement[:1].upper() + replacement[1:]
        result = before[:start] + replacement
        changed = True
    return None


# Cues that only ever take words back.
_RETRACTION = re.compile(
    r"\b(?:no[,\s]+(?:no|wait|actually)|wait[,\s]+no|scratch that|make that|I meant|or rather|sorry[,\s]+I mean)\b",
    re.I,
)
# Cues that open a correction, and also an answer or a new thought.
_OPENING_CUE = re.compile(r"^(?:(?:no|nope|wait|sorry|oops|actually|I mean)\b[\s,.!-]*)+", re.I)
# After a cue, these continue the thought instead of replacing words.
_CONTINUES = re.compile(r"(?:unless|if|because|but|and|so|since|though|although|when|until|that['’]s)\b", re.I)
# A bare replacement value ("Wednesday", "it's the blue one") is this short.
_REPLACEMENT_WORDS = 4
_FUNCTION_WORDS = frozenset(
    [
        "a",
        "an",
        "the",
        "i",
        "i'm",
        "we",
        "you",
        "he",
        "she",
        "they",
        "it",
        "it's",
        "my",
        "our",
        "your",
        "their",
        "to",
        "of",
        "in",
        "on",
        "at",
        "for",
        "with",
        "and",
        "or",
        "but",
        "so",
        "is",
        "are",
        "was",
        "were",
        "be",
        "do",
        "did",
        "want",
        "need",
        "think",
        "that",
        "this",
        "there",
        "then",
        "just",
    ]
)


def _restates(phrase: list[str], earlier: list[str], anchor: int) -> bool:
    """Whether ``phrase`` opens by repeating ``anchor`` or more earlier words, then says something else in their place."""
    run = phrase[:anchor]
    if len(run) < anchor or all(word in _FUNCTION_WORDS for word in run):
        return False
    for start in range(len(earlier) - anchor):
        if earlier[start : start + anchor] != run:
            continue
        shared = anchor
        while shared < len(phrase) and start + shared < len(earlier) and earlier[start + shared] == phrase[shared]:
            shared += 1
        if shared < len(phrase) and start + shared < len(earlier):
            return True
    return False


def retracted_by(phrase: str, earlier: str) -> str | None:
    """What ``phrase``, said after ``earlier``, may take back: ``earlier`` and the cue; None if it corrects nothing.

    A retraction cue always corrects. A cue that can also open an answer or
    a new thought ("No, unless...") does only when a short replacement value
    follows, or the rest restates earlier words with a change. Without a cue
    the phrase must restate three earlier words, one of them a content word,
    and then differ.
    """
    phrase = phrase.strip()
    if retraction := _RETRACTION.search(phrase):
        return f"{earlier} {phrase[: retraction.end()]}"
    previous = _words(earlier)
    cue = _OPENING_CUE.match(phrase)
    if not cue:
        return earlier if _restates(_words(phrase), previous, 3) else None
    rest = phrase[cue.end() :]
    words = _words(rest)
    if not words or _CONTINUES.match(rest):
        return None
    if len(words) <= _REPLACEMENT_WORDS or _restates(words, previous, 2):
        return f"{earlier} {phrase[: cue.end()]}"
    return None
