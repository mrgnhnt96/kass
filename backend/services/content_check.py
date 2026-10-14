"""Does cleaned-up text still say what the speaker said?

Cleanup may restructure: drop false starts and repeats, reorder a jumbled
sentence, fix grammar. It must not add ideas, summarize, or leave something
out. Three rules, by comparing words (no second model):

1. What the speaker said that carries meaning must survive: every number,
   technical term, name and spoken command, whether they said "not", who
   does what to whom ("you ... for me" never becomes "I ... for you"), and
   all but a few of their other words. Losing any of it rejects.
2. Anything the cleanup added is listed for review, never rejected on its own,
   except a "not" nobody said. Formatting such as list numbers lands here.
3. Many added words, a cleanup that is mostly (or only) words nobody said, or
   a question that comes back without a question mark, means the model
   answered or obeyed the dictation instead of cleaning it up; that rejects.

Verdicts: ``ok`` (only structure changed), ``review`` (the cleanup is kept and
the capture flagged for the user to check) and ``reject`` (the transcript is
pasted instead and the capture flagged). New concerns belong in the example
set of real dictations, not in new rules here.
"""

import re
import unicodedata
from dataclasses import dataclass, field
from difflib import SequenceMatcher

from .voice_commands import count_commands

_FUNCTION_WORDS = frozenset(
    re.findall(
        r"\S+",
        """
    a an the and or but so yet nor to of in on at for with from by as into onto about than then that this these those
    there here it its it's i i'm i've i'll i'd me my mine you you're your yours we we're our us they they're their them
    he she his her him is are was were be been being am do does did done have has had having will would can could
    should shall may might must just like really very actually basically literally kind sort mean know well okay ok
    yeah oh um uh er hmm ah some any all what which who whom whose when where why how if because while also too wait right anyway
    """,
    )
)
# "no" is left out: as an interjection ("no wait, actually...") a cleanup drops it.
_NEGATIONS = frozenset(
    re.findall(
        r"\S+",
        """
    not never nothing none nobody neither nor cannot can't don't doesn't didn't isn't aren't wasn't weren't won't
    wouldn't shouldn't couldn't haven't hasn't hadn't mustn't without
    """,
    )
)
_FILLERS = frozenset(("um", "uh", "er", "hmm", "ah"))
_TOKEN = re.compile(r"[+-]?\d+(?:[.,]\d+)*%?|[\w'./-]*\w")
# The model answered or obeyed instead of cleaning up when it added this many
# content words, or this share of the transcript's, whichever is larger.
ANSWERED_WORDS = 4
ANSWERED_SHARE = 0.5
# A word or two dropped is normal for false starts; a larger share of a short
# take gets a review.
MISSING_SHARE = 0.3
# A cleanup that leaves out more than this share of the speaker's content
# words dropped what they said: a request, a question, a sentence. Before,
# these were reviews (over MISSING_SHARE) or passed outright, and a review
# pastes the cleanup, so the user had to say the sentences again. A rejected
# cleanup pastes the transcript instead, which costs only polish. Measured on
# 149 real dictations (one speaker): this and the "person" rule below moved
# 15 cleanups to reject. 13 had lost content ("check for comments and see if
# you need to reply" gone, a 13-word question cut to 5, "make sure they are"
# turned into "are we"); 2 dropped a vague trailing clause ("and all of the
# other type things"). The one legitimate drop left was 7% ("set up" written
# "setup").
DROPPED_SHARE = 0.15
# And at least this many: a short take loses a third of its content words to
# one dropped hedge or false start ("I think it's ready" -> "It's ready").
DROPPED_WORDS = 2
_FIRST_PERSON = frozenset(("i", "me", "my", "mine", "myself", "i'm", "i've", "i'll", "i'd"))
_SECOND_PERSON = frozenset(("you", "your", "yours", "yourself", "you're", "you've", "you'll", "you'd"))


@dataclass
class Verdict:
    outcome: str
    added: list[str] = field(default_factory=list)
    missing: list[str] = field(default_factory=list)
    reason: str | None = None

    def to_dict(self) -> dict:
        return {"outcome": self.outcome, "added": self.added, "missing": self.missing, "reason": self.reason}


def _tokens(text: str) -> list[str]:
    normalized = unicodedata.normalize("NFKC", text).casefold().replace("\u2019", "'").replace("\u2212", "-")
    tokens = []
    for token in _TOKEN.findall(normalized):
        # A sign belongs to a number ("-3.5"); anywhere else it is punctuation.
        token = token.rstrip("./-'") if _is_number(token.rstrip(".")) else token.strip("./-'")
        if token and token not in _FILLERS:
            tokens.append(token)
    return tokens


def _is_number(token: str) -> bool:
    return bool(re.fullmatch(r"[+-]?\d+(?:[.,]\d+)*%?", token))


def _is_technical(token: str) -> bool:
    return bool(re.search(r"\w[_/.]\w", token)) and not _is_number(token)


def _parts(token: str) -> list[str]:
    return [part for part in re.split(r"[_/.-]+", token) if part]


_WORD = re.compile(r"[\w'][\w'.-]*\w|\w")


def _names(text: str) -> set[str]:
    """Words capitalized mid-sentence: names, places, products ("Walmart").

    Whisper capitalizes proper nouns, so this needs no model. A capital at
    the start of a sentence, and "I", say nothing about the word.
    """
    names = set()
    text = unicodedata.normalize("NFKC", text).replace("\u2019", "'")
    for match in _WORD.finditer(text):
        word = match.group()
        before = text[: match.start()].rstrip()
        if not word[0].isupper() or not before or before[-1] in ".!?:\n" or re.fullmatch(r"I('\w+)?", word):
            continue
        name = word.casefold().strip("'.-")
        # Whisper also capitalizes a word that restarts a cut-off sentence
        # ("It might But we'll see"); common words are never names.
        if name not in _FUNCTION_WORDS and name not in _NEGATIONS:
            names.add(name)
    return names


def _number(token: str) -> str:
    return token.replace(",", "")


_CORRECTION_CUE = re.compile(r"\b(actually|no wait|no actually|make that|I mean|scratch that)\b", re.I)


def _retracted(said: str):
    """Whether a word was said before the speaker's last correction cue."""
    cues = list(_CORRECTION_CUE.finditer(said))
    if not cues:
        return lambda _: False
    spoken = unicodedata.normalize("NFKC", said[: cues[-1].start()]).casefold().replace(",", "")
    return lambda word: bool(re.search(rf"(?<![\w]){re.escape(word.replace(',', ''))}(?![\w])", spoken))


def check(said: str, cleaned: str, allow_retractions: bool = False) -> Verdict:
    """Compare ``cleaned`` against the transcript ``said``.

    With ``allow_retractions``, a number, name or term said before a spoken
    correction ("Tuesday, actually Wednesday") may be left out.
    """
    retracted = _retracted(said) if allow_retractions else lambda _: False
    before = _tokens(said)
    after = _tokens(cleaned)
    heard = set(before)
    heard_parts = heard | {part for token in before for part in _parts(token)}
    kept = set(after)
    kept_parts = kept | {part for token in after for part in _parts(token)}
    numbers_before = {_number(t) for t in before if _is_number(t)}
    numbers_after = {_number(t) for t in after if _is_number(t)}
    added_numbers = sorted(numbers_after - numbers_before)

    # Rule 1: what the speaker said that carries meaning must survive.
    # "index dot tsx" may become "index.tsx", so a said word also survives as
    # part of a joined technical term.
    lost_numbers = sorted(n for n in numbers_before - numbers_after if not retracted(n))
    if lost_numbers:
        return Verdict("reject", reason="number", added=added_numbers, missing=lost_numbers)
    negations_before = {t for t in before if t in _NEGATIONS}
    negations_after = {t for t in after if t in _NEGATIONS}
    # "don't" may become "do not"; only losing every negation, or adding one
    # where none was said, flips the meaning.
    if bool(negations_before) != bool(negations_after):
        return Verdict(
            "reject",
            reason="negation",
            added=sorted(negations_after - negations_before),
            missing=sorted(negations_before - negations_after),
        )
    if count_commands(cleaned) != count_commands(said):
        return Verdict("reject", reason="command")
    # Spoken quotes, brackets and symbols are in ``said`` as characters;
    # losing one loses what the speaker asked for.
    if any(_count_marks(cleaned, marks) < _count_marks(said, marks) for marks in _MARKS):
        return Verdict("reject", reason="command")
    lost_terms = sorted({t for t in before if _is_technical(t) and t not in kept and not retracted(t)})
    if lost_terms:
        return Verdict("reject", reason="technical", missing=lost_terms)
    lost_names = sorted({name for name in _names(said) if name not in kept_parts and not retracted(name)})
    if lost_names:
        return Verdict("reject", reason="name", missing=lost_names)

    ignored = _FUNCTION_WORDS | _NEGATIONS | {"no"}
    content_before = [t for t in before if t not in ignored and not _is_number(t)]
    content_after = [t for t in after if t not in ignored and not _is_number(t)]
    said_words = set(content_before) | heard_parts
    # Rule 2: anything the cleanup added is listed for review, never rejected
    # on its own. List numbering, "1st", headings are formatting it may add.
    added = sorted({t for t in content_after if t not in said_words and not all(p in said_words for p in _parts(t))})
    missing = sorted(set(content_before) - set(content_after) - {p for t in content_after for p in _parts(t)})

    # Rule 3: many new words, or new words outnumbering the ones kept, means
    # the model answered or obeyed the dictation. So does keeping none of the
    # speaker's words at all ("Thank you." -> "You're welcome."), or turning a
    # question into a statement ("Is this working better?" -> "Yes, it's
    # working better."), even when the answer reuses the speaker's words.
    if "?" in said and "?" not in cleaned:
        return Verdict("reject", reason="answered", added=added, missing=missing)
    kept_content = set(content_after) - set(added)
    if len(added) >= max(ANSWERED_WORDS, ANSWERED_SHARE * len(set(content_before))) or (
        len(added) > len(kept_content) and (len(added) >= 2 or not kept_content)
    ):
        return Verdict("reject", reason="answered", added=added, missing=missing)
    dropped = [t for t in missing if not retracted(t)]
    if len(dropped) >= DROPPED_WORDS and len(dropped) > DROPPED_SHARE * len(set(content_before)):
        return Verdict("reject", reason="dropped", added=added, missing=missing)
    if _swapped_people(before, after):
        return Verdict("reject", reason="person", added=added, missing=missing)
    # A new word in place of a heard one may fix a mishearing ("merge domain"
    # -> "merge to main"); one put between heard words replaces nothing, so
    # the model made it up ("Merge." -> "Merge to main"). In 3,496 real
    # dictations all 13 such words were inventions.
    invented = sorted(set(_inserted(before, after)) & set(added))
    if invented:
        return Verdict("reject", reason="invented", added=invented, missing=missing)
    added = sorted(set(added) | set(added_numbers))
    if added or len(missing) > MISSING_SHARE * len(set(content_before)):
        return Verdict("review", added=added, missing=missing)
    return Verdict("ok")


def _inserted(before: list[str], after: list[str]) -> list[str]:
    """Words of ``after`` that sit between kept words without replacing any heard one."""
    opcodes = SequenceMatcher(None, before, after, autojunk=False).get_opcodes()
    return [token for tag, _, _, j1, j2 in opcodes if tag == "insert" for token in after[j1:j2]]


def _people(tokens: list[str]) -> list[int]:
    return [1 if t in _FIRST_PERSON else 2 for t in tokens if t in _FIRST_PERSON or t in _SECOND_PERSON]


def _swapped_people(before: list[str], after: list[str]) -> bool:
    """Whether a "you" became "I" and an "I" became "you" in the same places.

    "so that you can pull some data for me" -> "so that I can pull some data
    for you" keeps every word, so no other rule sees it, yet it reverses who
    asked for what. Pronouns are compared in order; only one of each turning
    into the other counts, so a cleanup that drops or rewords a pronoun is
    left to the other rules. In 149 real dictations it found the one real swap
    and nothing else.
    """
    said, cleaned = _people(before), _people(after)
    if len(said) != len(cleaned):
        return False
    changes = {(a, b) for a, b in zip(said, cleaned, strict=True) if a != b}
    return changes == {(1, 2), (2, 1)}


# This many consecutive output words the speaker never said, found in an
# example the model was shown, means it copied the example.
COPIED_RUN = 3


def _copied_example(said: str, cleaned: str, examples) -> list[str] | None:
    """A run of unsaid output words that appears word for word in an example."""
    heard = set(_tokens(said))
    heard |= {part for token in heard for part in _parts(token)}
    shown = [" " + " ".join(_tokens(text)) + " " for pair in examples for text in pair]
    run: list[str] = []
    for token in [*_tokens(cleaned), None]:
        if token is not None and token not in heard:
            run.append(token)
            continue
        if len(run) >= COPIED_RUN:
            # Any COPIED_RUN-long stretch of the run is enough.
            for start in range(len(run) - COPIED_RUN + 1):
                window = " " + " ".join(run[start : start + COPIED_RUN]) + " "
                if any(window in text for text in shown):
                    return run
        run = []
    return None


def _shown_examples() -> list[tuple[str, str]]:
    """Every example the cleanup model sees: built-in and the user's own."""
    try:
        from .personal_examples import for_prompt
        from .refinement import REFINEMENT_EXAMPLES

        return [*REFINEMENT_EXAMPLES, *for_prompt()]
    except Exception:
        return []


_MARKS = ('"\u201c\u201d', *"()[]{}<>/\\|^")


def _count_marks(text: str, marks: str) -> int:
    return sum(text.count(mark) for mark in marks)


def check_refinement(said: str, refined: str, flags, examples=None) -> tuple[str, Verdict]:
    """Check a cleanup and choose what to keep: the cleanup unless rejected.

    ``said`` is compared after the deterministic spoken-correction pass, so a
    retraction the speaker made ("3, no actually 4") is not a changed number.
    Text copied from an example the model was shown is rejected, whatever
    else the check finds.
    """
    from .refinement import prepare_refinement

    cleaned, explicit = prepare_refinement(said, flags)
    if explicit is not None:
        return explicit, Verdict("ok")
    copied = _copied_example(cleaned, refined, _shown_examples() if examples is None else examples)
    if copied:
        return cleaned, Verdict("reject", reason="copied example", added=copied)
    verdict = check(cleaned, refined, allow_retractions=flags.self_correction)
    if verdict.outcome == "review" and not verdict.added and flags.self_correction and _CORRECTION_CUE.search(said):
        # The speaker took words back, so leaving them out is the point.
        verdict = Verdict("ok")
    return (cleaned if verdict.outcome == "reject" else refined), verdict


def summarize_reviews(verdicts: list[Verdict]) -> dict | None:
    """One flag for a capture from its phrases' verdicts; None when all passed."""
    flagged = [v for v in verdicts if v.outcome != "ok"]
    if not flagged:
        return None
    return {
        "outcome": "reject" if any(v.outcome == "reject" for v in flagged) else "review",
        "added": sorted({word for v in flagged for word in v.added}),
        "missing": sorted({word for v in flagged for word in v.missing}),
        "reasons": sorted({v.reason for v in flagged if v.reason}),
    }
