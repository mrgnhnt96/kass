"""Record what the Python text stages return, for the Rust port to match.

Wraps every public function of the text-stage modules, runs their pytest
files and the capture-stream tests (which call them the way a dictation
does), then runs each stage over the parity set's transcripts. Every call
and its result goes into ``python.json``; ``tests/golden.rs`` replays them
against kass-text.

Whisper's vocabulary is left out (``_vocabulary`` returns nothing), so
``_common_word`` decides by the system word list alone and the recording
doesn't depend on which models this Mac has downloaded. Run from the
repository root with the backend venv:

    backend/venv/bin/python tauri/src-tauri/text/tests/golden/record.py \
        [--parity scripts/parity/baseline/python.json]
"""

import argparse
import dataclasses
import functools
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT))

from backend.services import (  # noqa: E402
    content_check,
    dictation_edits,
    laughter,
    phrase_seams,
    sentence_tail,
    spoken_case,
    spoken_cleanup,
    spoken_corrections,
    spoken_punctuation,
    text_merge,
    voice_commands,
    voice_edits,
)

OUT = Path(__file__).with_name("python.json")

TESTS = [
    "test_spoken_punctuation.py",
    "test_spoken_case.py",
    "test_spoken_cleanup.py",
    "test_spoken_corrections.py",
    "test_phrase_seams.py",
    "test_sentence_tail.py",
    "test_text_merge.py",
    "test_laughter.py",
    "test_content_check.py",
    "test_voice_commands.py",
    "test_voice_edits.py",
    "test_dictation_edits.py",
    "test_spoken_style.py",
    "test_capture_stream.py",
    "test_capture_stream_tail.py",
    "test_capture_stream_context.py",
    "test_capture_stream_provisional.py",
    "test_refinement_prompt.py",
    "test_refinement_collapse.py",
]


def _value(value):
    """``value`` as JSON: sets sorted, dataclasses as dicts tagged with their type."""
    if isinstance(value, (set, frozenset)):
        return sorted(_value(item) for item in value)
    if isinstance(value, (list, tuple)):
        return [_value(item) for item in value]
    if isinstance(value, dict):
        return {str(key): _value(item) for key, item in value.items()}
    if isinstance(value, content_check.Verdict):
        return value.to_dict()
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        fields = {field.name: _value(getattr(value, field.name)) for field in dataclasses.fields(value)}
        return {"type": type(value).__name__, **fields}
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    raise TypeError(f"can't record {type(value).__name__}")


def _learned(learned):
    if learned is None:
        return None
    aliases, kept = learned
    return {"aliases": dict(sorted(aliases.items())), "kept": sorted(kept)}


def _example(example):
    return {
        "original": example.original,
        "expected": example.expected,
        "capture_id": example.capture_id,
        "source": example.source,
    }


def _verdict(verdict):
    return verdict.to_dict()


# How each recorded function's arguments are written, by position (missing
# ones keep their default) and keyword.
SIGNATURES = {
    (spoken_punctuation, "apply_spoken_punctuation"): (["text", ("learned", _learned)], {}),
    (spoken_punctuation, "keep_spoken_punctuation"): (["said", "text", ("learned", _learned)], {}),
    (spoken_punctuation, "period_after_closers"): (["text"], {}),
    (spoken_punctuation, "learn"): ([("examples", lambda examples: [_example(e) for e in examples])], {}),
    (spoken_case, "apply_spoken_case"): (["text"], {}),
    (spoken_cleanup, "apply_spoken_cleanup"): (["text"], {}),
    (spoken_corrections, "apply_spoken_corrections"): (["text"], {}),
    (spoken_corrections, "retracted_by"): (["phrase", "earlier"], {}),
    (phrase_seams, "_common_word"): (["word"], {}),
    (phrase_seams, "open_phrase"): (["text", "raw"], {}),
    (phrase_seams, "strip_pause_mark"): (["text"], {}),
    (phrase_seams, "mid_sentence_capitals"): (["text"], {}),
    (phrase_seams, "continue_phrase"): (["phrase", "earlier", ("names", sorted)], {}),
    (phrase_seams, "continues_sentence"): (["before"], {}),
    (phrase_seams, "match_raw_start"): (["text", "raw"], {}),
    (phrase_seams, "continue_after_seam"): (["before", "phrase", "earlier", ("names", sorted)], {}),
    (phrase_seams, "keep_raw_capitals"): (["text", "raw", ("names", sorted)], {}),
    (phrase_seams, "close_phrase"): (["text"], {}),
    (phrase_seams, "join_phrases"): (["previous", "phrase", "raw_phrase", "style"], {}),
    (sentence_tail, "settle"): (["raw", "cleaned"], {}),
    (text_merge, "take_back"): (["before", "removed", "newer"], {}),
    (laughter, "is_laughter"): (["text"], {}),
    (laughter, "join_laughter"): (["text"], {}),
    (content_check, "check"): (["said", "cleaned", "allow_retractions", "retractable"], {}),
    (content_check, "_copied_example"): (["said", "cleaned", ("examples", lambda e: [list(p) for p in e])], {}),
    (content_check, "summarize_reviews"): ([("verdicts", lambda v: [_verdict(x) for x in v])], {}),
    (voice_commands, "mark_commands"): (["text"], {}),
    (voice_commands, "count_commands"): (["text"], {}),
    (voice_commands, "commands_alone"): (["text"], {}),
    (voice_edits, "words"): (["text"], {}),
    (voice_edits, "opening"): (["text"], {}),
    (voice_edits, "starts_edit"): (["text"], {}),
    (voice_edits, "parse"): (["said"], {}),
    (voice_edits, "plan"): (["said", "take"], {}),
    (voice_edits, "changes_end"): ([("planned", _value), "chars"], {}),
    (voice_edits, "corrected"): (["output", "before", "after"], {}),
    (dictation_edits, "apply_dictation_edits"): (["text"], {"formatting": None, "corrections": None}),
    (dictation_edits, "apply_line_breaks"): (["text"], {}),
    (dictation_edits, "apply_spoken_marks"): (["text"], {}),
}

calls: dict[str, dict] = {}


def _encode(names, keywords, args, kwargs):
    encoded = {}
    for index, spec in enumerate(names):
        name, encode = spec if isinstance(spec, tuple) else (spec, _value)
        if index < len(args):
            encoded[name] = encode(args[index])
        elif name in kwargs:
            encoded[name] = encode(kwargs[name])
    for name in keywords:
        if name in kwargs:
            encoded[name] = _value(kwargs[name])
    return encoded


def _wrap(module, name, names, keywords):
    original = getattr(module, name)
    qualified = f"{module.__name__.rsplit('.', 1)[-1]}.{name}"

    @functools.wraps(original)
    def recorded(*args, **kwargs):
        result = original(*args, **kwargs)
        try:
            entry = {"fn": qualified, "args": _encode(names, keywords, args, kwargs), "out": _value(result)}
        except TypeError:
            return result
        calls.setdefault(json.dumps([entry["fn"], entry["args"]], sort_keys=True), entry)
        return result

    setattr(module, name, recorded)


def _parity_texts(path: Path) -> list[tuple[str, str]]:
    """(raw, cleaned) of every take in a parity recording."""
    found = []

    def walk(node):
        if isinstance(node, dict):
            raw, refined = node.get("transcript_raw"), node.get("transcript_refined")
            if isinstance(raw, str):
                found.append((raw, refined if isinstance(refined, str) else raw))
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for value in node:
                walk(value)

    walk(json.loads(path.read_text()))
    return list(dict.fromkeys(found))


def _run_stages(raw: str, cleaned: str) -> None:
    """Every stage over one parity take, the way a dictation would run them."""
    for text in (raw, cleaned):
        spoken_punctuation.apply_spoken_punctuation(text)
        spoken_punctuation.period_after_closers(text)
        spoken_case.apply_spoken_case(text)
        spoken_cleanup.apply_spoken_cleanup(text)
        spoken_corrections.apply_spoken_corrections(text)
        phrase_seams.strip_pause_mark(text)
        phrase_seams.close_phrase(text)
        phrase_seams.mid_sentence_capitals(text)
        phrase_seams.continues_sentence(text)
        laughter.join_laughter(text)
        voice_commands.mark_commands(text)
        voice_edits.parse(text)
        voice_edits.plan(text, raw)
        dictation_edits.apply_dictation_edits(text, formatting=True, corrections=True)
        dictation_edits.apply_line_breaks(text)
        dictation_edits.apply_spoken_marks(text)
    spoken_punctuation.keep_spoken_punctuation(raw, cleaned)
    phrase_seams.open_phrase(cleaned, raw)
    phrase_seams.match_raw_start(cleaned.lower(), raw)
    phrase_seams.keep_raw_capitals(cleaned.lower(), raw)
    phrase_seams.continue_phrase(cleaned, raw)
    phrase_seams.join_phrases(raw, cleaned, cleaned, "standard")
    phrase_seams.join_phrases(raw, cleaned, cleaned, "casual")
    sentence_tail.settle(raw, cleaned)
    text_merge.take_back(raw, cleaned, cleaned + " Thanks.")
    content_check.check(raw, cleaned)
    content_check.check(raw, cleaned, allow_retractions=True)
    spoken_corrections.retracted_by(cleaned, raw)
    words = raw.split()
    # A pause after some of the words: at most a dozen places, spread out.
    for cut in sorted({1 + i * (len(words) - 1) // 12 for i in range(12)} - {len(words)}):
        before, after = " ".join(words[:cut]), " ".join(words[cut:])
        phrase_seams.continue_after_seam(before, after, before)
        phrase_seams.join_phrases(before, after, after, "standard")
        sentence_tail.settle(raw, " ".join(cleaned.split()[:cut]))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--parity", type=Path, help="a parity recording whose transcripts every stage runs over")
    options = parser.parse_args()

    phrase_seams._vocabulary.cache_clear()
    phrase_seams._vocabulary = functools.cache(lambda: {})
    for (module, name), (names, keywords) in SIGNATURES.items():
        _wrap(module, name, names, keywords)

    import pytest

    tests = [str(ROOT / "backend" / "tests" / name) for name in TESTS]
    # Failures don't matter here: every call is recorded with what it returned.
    pytest.main(["-q", "-p", "no:cacheprovider", *tests])

    if options.parity:
        for raw, cleaned in _parity_texts(options.parity):
            _run_stages(raw, cleaned)

    entries = sorted(calls.values(), key=lambda entry: (entry["fn"], json.dumps(entry["args"], sort_keys=True)))
    # The cleanup examples are the same long list in most calls: kept once, by index.
    shared: list = []
    for entry in entries:
        examples = entry["args"].get("examples")
        if entry["fn"] == "content_check._copied_example" and examples is not None:
            if examples not in shared:
                shared.append(examples)
            entry["args"]["examples"] = shared.index(examples)
    lines = [json.dumps(entry, ensure_ascii=False, sort_keys=True) for entry in entries]
    OUT.write_text(
        '{"examples": ' + json.dumps(shared, ensure_ascii=False) + ',\n"calls": [\n' + ",\n".join(lines) + "\n]}\n"
    )
    print(f"Recorded {len(entries)} calls to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
