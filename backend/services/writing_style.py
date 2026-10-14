"""The user's punctuation habits, learned from calibration and corrections.

Teaching (docs/plans/TEACH_BY_REPLYING.md) keeps each dictated reply: what
was said, Kass's cleanup and what the user sent. Comparing the cleanup
with what was sent, word by word, counts what the user does at each sentence
break (keep the period, turn it into a comma, drop it), whether they lowercase
sentence starts, drop commas after opening words or before conjunctions, and
end with a period. Refined-output corrections add the same evidence.

"Match my writing" uses those counts three ways: it picks the refinement
prompt style, how dictation phrases join across pauses, and a deterministic
pass over refined text. The pass only ever changes punctuation and the case of
a sentence's first letter, never words.

Each writing style (styles.py) learns on its own: its calibration runs, and
the corrections made in the apps assigned to it. ``style`` arguments are style
ids; None is the default style.

Everything is local: one JSON file in the data directory.
"""

import hashlib
import json
import logging
import os
import re
import threading
from datetime import UTC, datetime
from difflib import SequenceMatcher

from .. import config
from . import capture_events

logger = logging.getLogger(__name__)

MAX_EXAMPLES = 50
# Least evidence before a habit is applied. Calibration alone gives about ten
# sentence breaks and a handful of each comma kind per run.
MIN_EVIDENCE = 2
# Bumped when ``observe`` counts corrections differently, so saved counts are
# recounted (``recount_if_stale``). 2: corrections that only fix words count nothing.
COUNTING = 2

_CONJUNCTIONS = frozenset(("and", "but", "so", "or", "yet", "though", "because"))
_ABBREVIATIONS = frozenset(("mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "approx"))
_OPENING_PUNCTUATION = "\"'(\u201c\u2018["
_TRAILING = re.compile(r"[.,!?;:\u2026\u2014-]+$")

_lock = threading.RLock()
_state = None


# --- Tokens --------------------------------------------------------------------


def _split(token: str) -> tuple[str, str]:
    """Split a whitespace token into its word and trailing punctuation."""
    match = _TRAILING.search(token)
    if not match or match.start() == 0:
        return token, ""
    return token[: match.start()], match.group()


def _key(word: str) -> str:
    return word.lstrip(_OPENING_PUNCTUATION).rstrip("\"')\u201d\u2019]").casefold().replace("\u2019", "'")


def _is_boundary(trail: str) -> bool:
    return "." in trail and ".." not in trail and "\u2026" not in trail


def _capitalized(word: str) -> bool:
    letters = word.lstrip(_OPENING_PUNCTUATION)
    return bool(letters) and letters[0].isupper()


def _keeps_capital(word: str) -> bool:
    """``I``, its contractions and acronyms keep their capitals anywhere."""
    letters = word.lstrip(_OPENING_PUNCTUATION)
    return letters == "I" or letters.startswith(("I'", "I\u2019")) or (len(letters) > 1 and letters.isupper())


def _lower_first(word: str) -> str:
    if _keeps_capital(word):
        return word
    index = len(word) - len(word.lstrip(_OPENING_PUNCTUATION))
    return word[:index] + word[index : index + 1].lower() + word[index + 1 :]


# --- Learning ------------------------------------------------------------------


def _empty_counts() -> dict:
    return {
        "boundary": {"period": 0, "comma": 0, "none": 0},
        "lowercase_start": {"yes": 0, "no": 0},
        "intro_comma": {"kept": 0, "dropped": 0},
        "conjunction_comma": {"kept": 0, "dropped": 0},
        "final_period": {"kept": 0, "dropped": 0},
    }


def _merge(*counts: dict) -> dict:
    total = _empty_counts()
    for item in counts:
        for habit, outcomes in (item or {}).items():
            for outcome, value in outcomes.items():
                if habit in total and outcome in total[habit]:
                    total[habit][outcome] += value
    return total


def observe(shown: str, written: str, deliberate: bool = True) -> dict:
    """Count the user's punctuation choices in ``written`` against ``shown``.

    A teach reply is ``deliberate``: what the user left is what they write. A
    correction is not: one that only fixes words leaves the punctuation as it
    was shown without choosing it, so it counts only if some punctuation or
    capital changed.
    """
    counts = _empty_counts()
    before = [_split(token) for token in shown.split()]
    after = [_split(token) for token in written.split()]
    if not before or not after:
        return counts
    matcher = SequenceMatcher(None, [_key(w) for w, _ in before], [_key(w) for w, _ in after], autojunk=False)
    aligned = {}
    for block in matcher.get_matching_blocks():
        for offset in range(block.size):
            aligned[block.a + offset] = block.b + offset
    if not deliberate and all(
        trail == after[j][1] and _capitalized(word) == _capitalized(after[j][0])
        for (word, trail), j in ((before[i], j) for i, j in aligned.items())
    ):
        return counts

    def record_start(i: int, j: int):
        word = before[i][0]
        if _capitalized(word) and not _keeps_capital(word):
            counts["lowercase_start"]["no" if _capitalized(after[j][0]) else "yes"] += 1

    if 0 in aligned and aligned[0] == 0:
        record_start(0, 0)
    sentence_start = True
    for i, (word, trail) in enumerate(before):
        j = aligned.get(i)
        last = i == len(before) - 1
        if j is not None:
            written_trail = after[j][1]
            if last:
                # A period turned into "?" or "!" is a different mark, not a dropped one.
                if j == len(after) - 1 and written_trail[-1:] not in ("?", "!"):
                    if _is_boundary(trail):
                        counts["final_period"]["kept" if _is_boundary(written_trail) else "dropped"] += 1
                    elif not trail and _is_boundary(written_trail):
                        counts["final_period"]["kept"] += 1
            elif aligned.get(i + 1) == j + 1:
                # Both words and the one after them survived, so the seam is comparable.
                if _is_boundary(trail) and _key(word) not in _ABBREVIATIONS:
                    if _is_boundary(written_trail) or written_trail[-1:] in "!?":
                        counts["boundary"]["period"] += 1
                        record_start(i + 1, j + 1)
                    elif written_trail == ",":
                        counts["boundary"]["comma"] += 1
                    elif written_trail == "":
                        counts["boundary"]["none"] += 1
                elif trail == ",":
                    habit = (
                        "intro_comma"
                        if sentence_start
                        else "conjunction_comma"
                        if _key(before[i + 1][0]) in _CONJUNCTIONS
                        else None
                    )
                    if habit and written_trail in (",", ""):
                        counts[habit]["kept" if written_trail == "," else "dropped"] += 1
        sentence_start = bool(trail) and (trail[-1] in "!?" or _is_boundary(trail))
    return counts


def decide(counts: dict) -> dict:
    """Turn counts into the habits to apply; a habit needs MIN_EVIDENCE first."""

    def share(habit: str, outcome: str) -> float | None:
        total = sum(counts[habit].values())
        return counts[habit][outcome] / total if total >= MIN_EVIDENCE else None

    boundary = counts["boundary"]
    decided_boundary = None
    if sum(boundary.values()) >= MIN_EVIDENCE:
        # Ties keep the period; the user has to show the change more often than not.
        decided_boundary = max(
            ("period", "comma", "none"), key=lambda outcome: (boundary[outcome], outcome == "period")
        )
    return {
        "boundary": decided_boundary,
        "lowercase_start": (share("lowercase_start", "yes") or 0) > 0.5,
        "drop_intro_comma": (share("intro_comma", "dropped") or 0) > 0.5,
        "drop_conjunction_comma": (share("conjunction_comma", "dropped") or 0) > 0.5,
        "drop_final_period": (share("final_period", "dropped") or 0) > 0.5,
    }


def summary(habits: dict) -> list[str]:
    """Stable codes the app turns into plain-language lines."""
    codes = []
    if habits["boundary"]:
        codes.append(f"boundary_{habits['boundary']}")
    for habit in ("lowercase_start", "drop_intro_comma", "drop_conjunction_comma", "drop_final_period"):
        if habits[habit]:
            codes.append(habit)
    return codes


# --- Applying ------------------------------------------------------------------


def apply_style(text: str, habits: dict) -> str:
    """Re-punctuate ``text`` with the user's habits; words are never changed."""
    if not text or not summary(habits) or summary(habits) == ["boundary_period"]:
        return text
    parts = re.split(r"(\s+)", text)
    words = parts[0::2]
    spaces = parts[1::2]
    sentence_start = True
    for index, token in enumerate(words):
        if not token:
            continue
        word, trail = _split(token)
        if sentence_start and habits["lowercase_start"] and _capitalized(word):
            word = _lower_first(word)
        following = next((w for w in words[index + 1 :] if w), None)
        gap = spaces[index] if index < len(spaces) else ""
        inline = following is not None and "\n" not in gap
        next_start = (bool(trail) and (trail[-1] in "!?" or _is_boundary(trail))) or "\n" in gap
        if inline and _is_boundary(trail) and _capitalized(following) and _key(word) not in _ABBREVIATIONS:
            if habits["boundary"] in ("comma", "none"):
                trail = trail.replace(".", "," if habits["boundary"] == "comma" else "", 1)
                words[index + 1] = _lower_first(following)
                next_start = False
        elif trail == "," and inline:
            if (sentence_start and habits["drop_intro_comma"]) or (
                not sentence_start and habits["drop_conjunction_comma"] and _key(_split(following)[0]) in _CONJUNCTIONS
            ):
                trail = ""
        elif following is None and _is_boundary(trail) and trail.endswith(".") and habits["drop_final_period"]:
            trail = trail[:-1]
        words[index] = word + trail
        sentence_start = next_start
    out = [None] * len(parts)
    out[0::2] = words
    out[1::2] = spaces
    return "".join(out)


# --- Profile -------------------------------------------------------------------


def _path():
    return config.get_data_dir() / "writing-style.json"


def _empty_style() -> dict:
    return {"runs": 0, "last_run_at": None, "examples": [], "feedback_counts": _empty_counts()}


def _empty_state() -> dict:
    return {"version": 2, "styles": {}, "hidden_examples": []}


def _migrate(loaded: dict) -> dict:
    """Version 1 had one profile; it becomes the style the global settings moved into."""
    if loaded.get("version", 1) >= 2:
        return {**_empty_state(), **loaded}
    from .styles import MIGRATED_STYLE

    profile = {key: loaded[key] for key in _empty_style() if key in loaded}
    return {
        **_empty_state(),
        "styles": {MIGRATED_STYLE: {**_empty_style(), **profile}},
        "hidden_examples": loaded.get("hidden_examples", []),
    }


def _load() -> dict:
    global _state
    with _lock:
        if _state is None:
            try:
                _state = _migrate(json.loads(_path().read_text()))
            except FileNotFoundError:
                _state = _empty_state()
            except (OSError, ValueError):
                logger.warning("Unreadable writing style profile; starting fresh", exc_info=True)
                _state = _empty_state()
        return _state


def _style_id(style: str | None) -> str:
    if style is not None:
        return style
    from .styles import default_id

    return default_id()


def _profile(style: str | None) -> dict:
    """``style``'s learned state; empty for a style that hasn't learned anything."""
    return {**_empty_style(), **_load()["styles"].get(_style_id(style), {})}


def _with_profile(state: dict, style: str | None, profile: dict) -> dict:
    return {**state, "styles": {**state["styles"], _style_id(style): profile}}


def _save(state: dict):
    global _state
    with _lock:
        path = _path()
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(state, indent=2, ensure_ascii=False))
        os.replace(temporary, path)
        _state = state
    capture_events.publish("learned")


def _counts(profile: dict, extra: list | None = None) -> dict:
    examples = profile["examples"] + (extra or [])
    return _merge(profile["feedback_counts"], *(observe(e["shown"], e["written"]) for e in examples))


def habits(style: str | None = None) -> dict:
    return decide(_counts(_profile(style)))


def is_ready(style: str | None = None) -> bool:
    profile = _profile(style)
    evidence = sum(sum(outcomes.values()) for outcomes in profile["feedback_counts"].values())
    return profile["runs"] > 0 or evidence >= 3


def status(style: str | None = None) -> dict:
    profile = _profile(style)
    learned = habits(style)
    return {
        "ready": is_ready(style),
        "runs": profile["runs"],
        "last_run_at": profile["last_run_at"],
        "example_count": len(profile["examples"]),
        "habits": summary(learned),
    }


def apply_learned(text: str, style: str | None = None) -> str:
    return apply_style(text, habits(style)) if is_ready(style) else text


_INSTRUCTIONS = {
    "boundary_period": "End each thought with a period and start the next one as a new sentence.",
    "boundary_comma": "Join related thoughts with commas instead of starting new sentences.",
    "boundary_none": "Run related thoughts together without punctuation between them.",
    "lowercase_start": 'Start sentences with a lowercase letter. Keep "I" and acronyms capitalized.',
    "drop_intro_comma": 'Do not put a comma after opening words like "yeah", "so", "okay" or "honestly".',
    "drop_conjunction_comma": 'Do not put a comma before "and", "but", "so" or "or".',
    "drop_final_period": "Do not end the text with a period.",
}


def prompt_section(style: str | None = None) -> str | None:
    """Refinement instructions describing how the user punctuates in ``style``."""
    if not is_ready(style):
        return None
    codes = summary(habits(style))
    if not codes:
        return None
    lines = "\n".join(f"- {_INSTRUCTIONS[code]}" for code in codes)
    return f"Punctuation style: match how this speaker writes.\n{lines}\n- Still use question marks for questions."


def _as_spoken(text: str) -> str:
    """What the speech-to-text step would hand refinement for ``text``."""
    return " ".join(re.sub(r"[^\w'\u2019$%-]", "", word) for word in text.split()).casefold()


def prompt_example(style: str | None = None) -> tuple[str, str] | None:
    """The user's latest calibration rewrite in ``style`` that differs from what was shown."""
    if not is_ready(style):
        return None
    for example in reversed(_profile(style)["examples"]):
        if example["written"].strip() != example["shown"].strip():
            return example.get("said") or _as_spoken(example["shown"]), example["written"]
    return None


def refresh_feedback(db) -> None:
    """Recount refined-output corrections for every style.

    Called after a correction is saved and after an app moves to another
    style: a correction counts for the style it teaches (``correction_style``).
    """
    from sqlalchemy import func

    from ..database.models import Capture, CaptureFeedback
    from .styles import correction_style, snapshot

    rows = (
        # A correction whose capture history retention deleted keeps its own copy.
        db.query(CaptureFeedback, func.coalesce(Capture.teaches_style_id, CaptureFeedback.teaches_style_id))
        .outerjoin(Capture, Capture.id == CaptureFeedback.capture_id)
        .filter(CaptureFeedback.target == "refined", CaptureFeedback.teaches_style())
        .order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id.desc())
        .limit(200)
        .all()
    )
    styles = snapshot()
    seen = set()
    counts: dict[str, list] = {}
    for row, teaches in rows:
        if row.capture_id in seen:
            continue
        seen.add(row.capture_id)
        try:
            captured = json.loads(row.snapshot)
            original = captured.get("transcript_refined")
        except (ValueError, TypeError, AttributeError):
            continue
        if original and max(len(original), len(row.expected_text)) <= 2000:
            style = correction_style(styles, captured.get("app_bundle_id"), teaches)
            counts.setdefault(style, []).append(observe(original, row.expected_text, deliberate=False))
    with _lock:
        state = _load()
        profiles = {
            style_id: {**_empty_style(), **profile, "feedback_counts": _merge(*counts.get(style_id, []))}
            for style_id, profile in state["styles"].items()
        }
        for style_id in counts.keys() - profiles.keys():
            profiles[style_id] = {**_empty_style(), "feedback_counts": _merge(*counts[style_id])}
        _save({**state, "styles": profiles, "counting": COUNTING})


def recount_if_stale(db) -> None:
    """Recount at startup when the counts were made by an older ``observe``."""
    if _load().get("counting") != COUNTING:
        refresh_feedback(db)


def reset(style: str | None = None) -> None:
    """Forget ``style``'s calibration and habits; other styles keep theirs."""
    with _lock:
        state = _load()
        _save(_with_profile(state, style, _empty_style()))
    _examples_changed()


def forget_style(style: str) -> None:
    """Drop a deleted style's learned state."""
    with _lock:
        state = _load()
        if style in state["styles"]:
            _save({**state, "styles": {k: v for k, v in state["styles"].items() if k != style}})
    _examples_changed()


def _examples_changed() -> None:
    from . import personal_examples

    personal_examples.invalidate()


def _example_id(example: dict) -> str:
    key = f"{example.get('created_at')}|{example['shown']}|{example['written']}"
    return "calibration:" + hashlib.sha256(key.encode()).hexdigest()[:16]


def calibration_examples(style: str | None = None) -> list[dict]:
    """``style``'s calibration rewrites the user edited, as "said, meant" examples."""
    examples = []
    for example in _profile(style)["examples"]:
        if example["written"].strip() == example["shown"].strip():
            continue
        examples.append(
            {
                "id": _example_id(example),
                "source": "calibration",
                # Spoken-jumble paragraphs are stored as said; punctuation ones as shown.
                "said": example.get("said") or _as_spoken(example["shown"]),
                "meant": example["written"],
                "created_at": example.get("created_at"),
            }
        )
    return examples


def hidden_examples() -> list[str]:
    return list(_load()["hidden_examples"])


def hide_example(example_id: str) -> None:
    with _lock:
        state = dict(_load())
        state["hidden_examples"] = [*state["hidden_examples"], example_id][-1000:]
        _save(state)


# --- Teaching -------------------------------------------------------------------


def save_run(style: str | None, examples: list[dict]) -> dict:
    """Keep a teach session's replies in ``style``'s profile (docs/plans/TEACH_BY_REPLYING.md).

    Each example has ``said`` (None for a typed reply), ``shown`` and
    ``written``, the way calibration rewrites were kept, so examples and
    habits read them unchanged.
    """
    if not examples:
        raise ValueError("Reply at least once before saving")
    with _lock:
        profile = _profile(style)
        now = datetime.now(UTC).isoformat()
        profile["examples"] = (profile["examples"] + [{**e, "created_at": now} for e in examples])[-MAX_EXAMPLES:]
        profile["runs"] += 1
        profile["last_run_at"] = now
        _save(_with_profile(_load(), style, profile))
    _examples_changed()
    return status(style)
