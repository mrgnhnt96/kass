"""How a speaker writes the way things were said, learned from their own edits.

Phase 4 of docs/plans/EXPRESSIVE_DICTATION.md. The defaults are strict: not
everyone wants "!" or "wayyy" from their voice, and a loose default would push
them on people who never asked. A speaker who wants more shows it by editing
their text, in Captures or by voice:

- A period they change to "!" on a sentence they said with some energy
  lowers their "!" cutoff toward it. A "!" they change back raises it.
- A word they write drawn out ("way" to "wayyy") teaches what their own
  stretches sound like; one they write plainly again stops it.

A redictation is weaker evidence (the user may only have reworded): it can
make the rules stricter, never looser.

The rule never moves on one edit, and never so far that more than a few of
the speaker's own plain sentences or words would change. Learning goes on
with "Write how it was said" off; only applying it waits for the switch.

Everything is read again from the saved measurements (``captures.prosody``)
and reports when a dictation starts, so a withdrawn report stops teaching at
once.
"""

from __future__ import annotations

import difflib
import json
import logging
import re
from dataclasses import dataclass, field

import numpy as np

from .prosody import EXCLAIM_ENERGY, FUNCTION_WORDS, STRETCH_LOUDER_DB, STRETCH_PACE, STRETCH_PLATEAU_S, StretchRule

logger = logging.getLogger(__name__)

# Edits that must agree before a rule moves.
MIN_EXAMPLES = 2
# A learned "!" cutoff never goes below this: a "!" typed on a sentence said
# calmly is the user's taste in punctuation, not something heard.
MIN_CUTOFF = 1.5
# How far past an edit's sentence or word the rule moves.
ENERGY_MARGIN = 0.25
PACE_MARGIN = 0.9
LOUDER_MARGIN_DB = 0.5
PLATEAU_MARGIN_S = 0.05
# Bounds no learned stretch rule passes.
MIN_PACE = 1.5
MIN_LOUDER_DB = -2.0
PLATEAU_LIMITS_S = (0.1, 1.0)
# At most this share of the speaker's own plain sentences and words may change.
PLAIN_SHARE = 0.02
STRETCH_SHARE = 0.005
# Plain sentences needed before they bound the cutoff.
MIN_PLAIN = 50

_CLOSERS = re.escape("\"')]”’")
_ENDING = re.compile(f"([.!?])[{_CLOSERS}]*$")
_DRAWN = re.compile(r"([a-z])\1\1", re.IGNORECASE)


@dataclass(frozen=True)
class Learned:
    """A speaker's own "!" cutoff and stretch rule; the defaults until they teach otherwise."""

    cutoff: float = EXCLAIM_ENERGY
    rule: StretchRule = field(default_factory=StretchRule)


def _key(word: str) -> str:
    return re.sub(r"[\W_]", "", word.lower())


def _plain(word: str) -> str:
    """A word with every repeated letter written once: "wayyy" and "way" agree, so do "goood" and "good"."""
    return re.sub(r"(.)\1+", r"\1", _key(word))


def _sentences(text: str) -> list[str]:
    return re.findall(rf"\S.*?[.!?][{_CLOSERS}]*(?=\s|$)", text, flags=re.DOTALL)


def _ending(sentence: str) -> str | None:
    match = _ENDING.search(sentence)
    return match.group(1) if match else None


def _words_key(sentence: str) -> tuple[str, ...]:
    return tuple(_plain(word) for word in sentence.split() if _plain(word))


@dataclass
class Evidence:
    """What the speaker's edits and plain dictations show."""

    wanted: list[float] = field(default_factory=list)
    unwanted: list[float] = field(default_factory=list)
    plain: list[float] = field(default_factory=list)
    stretched: list[dict] = field(default_factory=list)
    unstretched: list[dict] = field(default_factory=list)
    # Every content word of the speaker's dictations, as (capture, index, shape).
    shapes: list[tuple[str, int, dict]] = field(default_factory=list)
    edited_shapes: set[tuple[str, int]] = field(default_factory=set)


def _shape(saved: list) -> dict:
    word, stretch, louder, plateau = saved
    return dict(word=word, stretch=stretch, louder=louder, plateau=plateau)


def gather(
    captures: list[tuple[str, dict, str]], edits: dict[str, str], redictated: frozenset[str] = frozenset()
) -> Evidence:
    """Evidence from each capture's saved measurements and, for an edited one, its text as the user left it.

    ``captures`` are (id, measured, text delivered); ``edits`` maps a capture
    id to its edited text. An edit in ``redictated`` counts only where it
    makes the rules stricter.
    """
    evidence = Evidence()
    for capture_id, measured, delivered in captures:
        shapes = [_shape(saved) for saved in measured.get("shapes", [])]
        evidence.shapes += [(capture_id, index, shape) for index, shape in enumerate(shapes)]
        written = measured.get("written", [])
        edited = edits.get(capture_id)
        if edited is None:
            evidence.plain += [sentence["energy"] for sentence in written if _ending(sentence["text"]) == "."]
            continue
        strict_only = capture_id in redictated
        endings = {_words_key(sentence): _ending(sentence) for sentence in _sentences(edited)}
        for sentence in written:
            before, after = _ending(sentence["text"]), endings.get(_words_key(sentence["text"]))
            if before == "." and after == "!":
                if not strict_only:
                    evidence.wanted.append(sentence["energy"])
            elif before == "!" and after == ".":
                evidence.unwanted.append(sentence["energy"])
            elif before == "." and after == ".":
                evidence.plain.append(sentence["energy"])
        _gather_stretches(evidence, capture_id, shapes, delivered, edited, strict_only)
    return evidence


def _gather_stretches(
    evidence: Evidence, capture_id: str, shapes: list[dict], delivered: str, edited: str, strict_only: bool
) -> None:
    before, after = delivered.split(), edited.split()
    matcher = difflib.SequenceMatcher(None, [_plain(w) for w in before], [_plain(w) for w in after], autojunk=False)
    for block in matcher.get_matching_blocks():
        for offset in range(block.size):
            was, now = before[block.a + offset], after[block.b + offset]
            drawn_before, drawn_now = bool(_DRAWN.search(was)), bool(_DRAWN.search(now))
            if drawn_before == drawn_now or (drawn_now and strict_only):
                continue
            # The most drawn-out reading of that word in the recording.
            found = [(index, shape) for index, shape in enumerate(shapes) if _plain(shape["word"]) == _plain(was)]
            if not found:
                continue
            index, shape = max(found, key=lambda item: item[1]["stretch"])
            evidence.edited_shapes.add((capture_id, index))
            (evidence.stretched if drawn_now else evidence.unstretched).append(shape)


def learn(evidence: Evidence) -> Learned:
    return Learned(cutoff=_cutoff(evidence), rule=_rule(evidence))


def _cutoff(evidence: Evidence) -> float:
    cutoff = EXCLAIM_ENERGY
    # A "!" typed on a sentence said calmly isn't about the voice.
    wanted = sorted(energy for energy in evidence.wanted if energy >= MIN_CUTOFF)
    if len(wanted) >= MIN_EXAMPLES:
        cutoff = min(cutoff, wanted[0] - ENERGY_MARGIN)
        if len(evidence.plain) >= MIN_PLAIN:
            cutoff = max(cutoff, float(np.quantile(evidence.plain, 1 - PLAIN_SHARE)))
        cutoff = max(cutoff, MIN_CUTOFF)
    if evidence.unwanted:
        cutoff = max(cutoff, max(evidence.unwanted) + ENERGY_MARGIN)
    return round(cutoff, 2)


def _rule(evidence: Evidence) -> StretchRule:
    rule = StretchRule()
    wanted = evidence.stretched
    if len(wanted) >= MIN_EXAMPLES:
        rule = StretchRule(
            pace=max(MIN_PACE, min(STRETCH_PACE, min(shape["stretch"] for shape in wanted) * PACE_MARGIN)),
            louder_db=max(
                MIN_LOUDER_DB, min(STRETCH_LOUDER_DB, min(shape["louder"] for shape in wanted) - LOUDER_MARGIN_DB)
            ),
            plateau=(
                max(
                    PLATEAU_LIMITS_S[0], min(STRETCH_PLATEAU_S[0], min(s["plateau"] for s in wanted) - PLATEAU_MARGIN_S)
                ),
                min(
                    PLATEAU_LIMITS_S[1], max(STRETCH_PLATEAU_S[1], max(s["plateau"] for s in wanted) + PLATEAU_MARGIN_S)
                ),
            ),
            words=frozenset(_key(shape["word"]) for shape in wanted if _key(shape["word"]) in FUNCTION_WORDS),
        )
        # Slower until few of the speaker's own plain words would be drawn out.
        plain = [
            shape for capture_id, index, shape in evidence.shapes if (capture_id, index) not in evidence.edited_shapes
        ]
        allowed = max(1, int(STRETCH_SHARE * len(plain)))
        while rule.pace < STRETCH_PACE and sum(rule.holds(shape) for shape in plain) > allowed:
            rule = _with_pace(rule, min(STRETCH_PACE, rule.pace * 1.1))
    for shape in evidence.unstretched:
        if rule.holds(shape):
            rule = _with_pace(rule, shape["stretch"] + 0.1)
    return rule


def _with_pace(rule: StretchRule, pace: float) -> StretchRule:
    return StretchRule(pace=round(pace, 2), louder_db=rule.louder_db, plateau=rule.plateau, words=rule.words)


# Recent dictations read for the speaker's plain sentences and words.
RECENT_CAPTURES = 500


def load() -> Learned:
    """The speaker's learned rule, from their saved measurements and their reports of them."""
    from ..database import Capture, CaptureFeedback, session as database_session

    with database_session.SessionLocal() as db:
        recent = (
            db.query(Capture.id, Capture.prosody, Capture.transcript_refined)
            .filter(Capture.prosody.isnot(None))
            .order_by(Capture.created_at.desc())
            .limit(RECENT_CAPTURES)
            .all()
        )
        reports = (
            db.query(
                CaptureFeedback.capture_id,
                CaptureFeedback.expected_text,
                CaptureFeedback.source,
                Capture.prosody,
                Capture.transcript_refined,
            )
            .join(Capture, Capture.id == CaptureFeedback.capture_id)
            .filter(CaptureFeedback.target == "refined", Capture.prosody.isnot(None))
            .order_by(CaptureFeedback.created_at, CaptureFeedback.id)
            .all()
        )
    captures, edits, redictated = {}, {}, set()
    for capture_id, saved, delivered in recent:
        captures[capture_id] = (saved, delivered)
    for capture_id, expected, source, saved, delivered in reports:
        captures[capture_id] = (saved, delivered)
        # Reports stack: the newest is the text as the user left it.
        edits[capture_id] = expected
        if source in CaptureFeedback.EXPLICIT_SOURCES:
            redictated.discard(capture_id)
        else:
            redictated.add(capture_id)
    measured = []
    for capture_id, (saved, delivered) in captures.items():
        try:
            measured.append((capture_id, json.loads(saved), delivered or ""))
        except ValueError:
            continue
    return learn(gather(measured, edits, frozenset(redictated)))
