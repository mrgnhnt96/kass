"""Short rules summarized from the user's older examples.

Cleanup shows the model only the most recent examples, so without this an
example would stop counting once enough newer ones pushed it out. When enough
examples have left the prompt, an idle-time job has the cleanup model fold
them into the notes it already has: a bounded list of plain rules such as
"Write Kass as one word." Every cleanup reads the same notes, so the
cached prompt still matches until they change.

A new version is kept only when replaying the user's own examples with it
comes out no further from what they meant than with the current notes. If it
does worse, each new rule is tried on its own and kept only when it helps, so
a bad rule from a small model cannot make cleanup worse or sink good ones. Either way the folded
examples are marked considered, and the corrections stay recorded for the
personal model.

Each writing style (styles.py) has its own notes, summarized from its own
examples. ``style`` arguments are style ids; None is the default style.

Everything is local: one JSON file in the data directory.
"""

import asyncio
import hashlib
import json
import logging
import os
import re
import threading
from datetime import UTC, datetime

from .. import config
from . import capture_events

logger = logging.getLogger(__name__)

# Examples that must leave the prompt before a summary runs.
MIN_BATCH = 10
MAX_BATCH = 20
# Summarized examples replayed to check the new notes still cover them.
RETAINED_CHECKS = 8
# Replays are sampled, so a rule tried on its own must beat the noise.
MIN_GAIN = 2
MAX_NOTES = 12
MAX_NOTE_CHARS = 160
MAX_HISTORY = 10
CHECK_SECONDS = 60
# After a failure, such as the cleanup model not being downloaded yet.
RETRY_SECONDS = 30 * 60

_lock = threading.RLock()
_state = None

_SUMMARY_PROMPT = """You keep a short list of rules for cleaning up one person's dictation.

The user message has the current rules, then new examples of what the speaker said out loud and what they actually meant to write. Return the updated list of rules.
- A rule describes a habit that applies to anything the speaker might dictate: words they drop or replace, how they reorder or restructure, how they format lists, spellings they always want.
- Never write a rule about one example. Good: "Turn items said in a row into a list, one item per line." Bad: "If the speaker says 'here is a list of groceries', format it as a list."
- Never quote more than three words from an example.
- Keep a current rule unless an example shows it is wrong. Merge rules that say the same thing.
- Each rule is one short sentence. At most {max_notes} rules.
- Reply with only the rules, one per line, each starting with "- "."""

# A rule sharing this many words in a row with an example restates it
# instead of generalizing it.
COPIED_WORDS = 5
_WORD = re.compile(r"\w+")


def _path():
    return config.get_data_dir() / "correction-notes.json"


def _empty() -> dict:
    """One style's notes."""
    return {
        "notes": [],
        "considered_ids": [],
        "history": [],
        "last_run": None,
        "outcome": "waiting",
        "metrics": None,
    }


def _read() -> dict:
    """The whole file. Version 1 held one list; it becomes the migrated style's."""
    global _state
    with _lock:
        if _state is None:
            state = {"version": 2, "styles": {}}
            try:
                loaded = json.loads(_path().read_text())
                if loaded.get("version") == 1:
                    from .styles import MIGRATED_STYLE

                    state["styles"][MIGRATED_STYLE] = {k: v for k, v in loaded.items() if k != "version"}
                elif loaded.get("version") == 2 and isinstance(loaded.get("styles"), dict):
                    state = loaded
                else:
                    raise ValueError("Unsupported correction notes")
            except FileNotFoundError:
                pass
            except (OSError, ValueError, TypeError):
                logger.exception("Could not read correction notes; starting without them")
            _state = state
        return _state


def _style_id(style: str | None) -> str:
    if style is not None:
        return style
    from .styles import default_id

    return default_id()


def _load(style: str | None = None) -> dict:
    return {**_empty(), **_read()["styles"].get(_style_id(style), {})}


def _write(state: dict) -> None:
    global _state
    with _lock:
        path = _path()
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_suffix(".tmp")
        temporary.write_text(json.dumps(state, indent=2, ensure_ascii=False))
        os.replace(temporary, path)
        _state = state
    capture_events.publish("learned")


def _save(notes_state: dict, style: str | None = None) -> None:
    with _lock:
        state = _read()
        _write({**state, "styles": {**state["styles"], _style_id(style): notes_state}})


def forget_style(style: str) -> None:
    """Drop a deleted style's notes."""
    with _lock:
        state = _read()
        if style in state["styles"]:
            _write({**state, "styles": {k: v for k, v in state["styles"].items() if k != style}})


def notes(style: str | None = None) -> list[str]:
    return list(_load(style)["notes"])


def note_id(note: str) -> str:
    return hashlib.sha256(note.encode()).hexdigest()[:12]


def prompt_section(style: str | None = None, candidate: list[str] | None = None) -> str | None:
    """Refinement instructions from the user's older corrections in ``style``."""
    current = notes(style) if candidate is None else candidate
    if not current:
        return None
    lines = "\n".join(f"- {note}" for note in current)
    return f"Rules this speaker taught in earlier corrections. Follow them:\n{lines}"


def pending(style: str | None = None) -> list[dict]:
    """``style``'s examples no longer in the prompt that no summary has considered, oldest first."""
    from . import personal_examples

    shown = {example["id"] for example in personal_examples.in_prompt(style)}
    considered = set(_load(style)["considered_ids"])
    return [
        example
        for example in reversed(personal_examples.all_examples(style))
        if example["id"] not in shown and example["id"] not in considered
    ]


def status(style: str | None = None) -> dict:
    state = _load(style)
    return {
        "notes": [{"id": note_id(note), "text": note} for note in state["notes"]],
        "pending": len(pending(style)),
        "last_run": state["last_run"],
        "outcome": state["outcome"],
    }


def remove(identifier: str, style: str | None = None) -> bool:
    with _lock:
        state = json.loads(json.dumps(_load(style)))
        kept = [note for note in state["notes"] if note_id(note) != identifier]
        if len(kept) == len(state["notes"]):
            return False
        state["history"] = (state["history"] + [{"notes": state["notes"]}])[-MAX_HISTORY:]
        state["notes"] = kept
        _save(state, style)
    return True


def _runs(text: str) -> set[tuple[str, ...]]:
    words = _WORD.findall(text.casefold())
    return {tuple(words[i : i + COPIED_WORDS]) for i in range(len(words) - COPIED_WORDS + 1)}


def parse(reply: str, examples: list[dict] = ()) -> list[str]:
    """The general rules in a summary reply.

    Anything that is not a short rule is dropped, and so is a rule that
    copies one of ``examples`` instead of describing a habit.
    """
    copied = set()
    for example in examples:
        copied |= _runs(example["said"]) | _runs(example["meant"])
    parsed, seen = [], set()
    for line in reply.splitlines():
        match = re.match(r"\s*(?:[-*•]|\d+[.)])\s+(.+)", line)
        if not match:
            continue
        note = match.group(1).strip().strip('"').strip()
        key = note.casefold()
        if not note or len(note) > MAX_NOTE_CHARS or key in seen or _runs(note) & copied:
            continue
        seen.add(key)
        parsed.append(note)
    return parsed[:MAX_NOTES]


def _summary_request(current: list[str], batch: list[dict]) -> str:
    rules = "\n".join(f"- {note}" for note in current) or "(none yet)"
    examples = "\n\n".join(f"Said: {example['said']}\nMeant: {example['meant']}" for example in batch)
    return f"Current rules:\n{rules}\n\nNew examples:\n\n{examples}"


async def _distance(examples, flags, model_size, candidate, generation) -> int | None:
    """How far cleanup with ``candidate`` notes lands from what was meant, summed."""
    from .correction_rules import loss
    from .model_improvement import manager
    from .refinement import refine_transcript

    total = 0
    for example in examples:
        if manager.interrupted(generation):
            return None
        text, _ = await refine_transcript(example["said"], flags, model_size=model_size, correction_notes=candidate)
        total += loss(text, example["meant"])
    return total


async def summarize(flags, model_size, generation) -> dict | None:
    """Fold the oldest pending examples into the notes.

    Returns None when there is not enough to do or a dictation interrupts.
    """
    from .correction_rules import loss
    from .llm import get_llm_model
    from .model_improvement import manager

    style = flags.style
    batch = pending(style)[:MAX_BATCH]
    if len(batch) < MIN_BATCH:
        return None
    state = _load(style)
    current = list(state["notes"])
    reply = await get_llm_model().generate(
        prompt=_summary_request(current, batch),
        system=_SUMMARY_PROMPT.replace("{max_notes}", str(MAX_NOTES)),
        max_tokens=1024,
        temperature=0.2,
        model_size=model_size,
    )
    candidate = parse(reply, batch)
    if manager.interrupted(generation):
        return None

    from . import personal_examples

    batch_ids = {example["id"] for example in batch}
    considered = set(state["considered_ids"])
    retained = [example for example in personal_examples.all_examples(style) if example["id"] in considered][
        :RETAINED_CHECKS
    ]
    checks = batch + retained

    async def distance(notes):
        return await _distance(checks, flags, model_size, notes, generation)

    baseline = await distance(current)
    if baseline is None:
        return None
    chosen, best = current, baseline
    if candidate and candidate != current:
        whole = await distance(candidate)
        if whole is None:
            return None
        if whole <= baseline:
            chosen, best = candidate, whole
        else:
            # One bad rule can sink a list of good ones, so add the new rules
            # one at a time, each kept only when it clearly helps.
            for note in candidate:
                if note in chosen or len(chosen) >= MAX_NOTES:
                    continue
                trial = await distance([*chosen, note])
                if trial is None:
                    return None
                if trial <= best - MIN_GAIN:
                    chosen, best = [*chosen, note], trial
    accepted = chosen != current
    metrics = {
        "examples": len(batch),
        "checks": len(checks),
        "baseline_distance": baseline,
        "chosen_distance": best,
        "unchanged_distance": sum(loss(example["said"], example["meant"]) for example in checks),
    }
    with _lock:
        state = json.loads(json.dumps(_load(style)))
        if accepted:
            state["history"] = (state["history"] + [{"notes": state["notes"]}])[-MAX_HISTORY:]
            state["notes"] = chosen
        state["considered_ids"] = state["considered_ids"] + sorted(batch_ids)
        state.update(
            last_run=datetime.now(UTC).isoformat(),
            outcome="updated" if accepted else "no_change",
            metrics=metrics,
        )
        _save(state, style)
    logger.info("Correction notes %s: %s", "updated" if accepted else "unchanged", metrics)
    return status(style)


async def run_once() -> dict | None:
    """Summarize one style's examples if the app is idle and enough have left its prompt."""
    from ..database import session as database_session
    from . import settings as settings_service
    from .model_improvement import manager
    from .styles import flags_for, snapshot

    generation = manager.idle_generation()
    if generation is None or database_session.SessionLocal is None:
        return None
    style = next((s for s in snapshot().styles if len(pending(s.id)) >= MIN_BATCH), None)
    if style is None:
        return None
    with database_session.SessionLocal() as db:
        saved = settings_service.get_capture_settings(db)
        if not saved.auto_refine:
            return None
        flags = flags_for(style, saved)
        model_size = saved.llm_model
    return await summarize(flags, model_size, generation)


async def periodic_job():
    while True:
        await asyncio.sleep(CHECK_SECONDS)
        try:
            await run_once()
        except Exception:
            logger.exception("Correction notes job failed; keeping the current notes")
            await asyncio.sleep(RETRY_SECONDS)
