"""Conservative vocabulary candidates and deterministic offline evaluation."""

import hashlib
import re
import statistics
import time
from collections import defaultdict
from dataclasses import dataclass
from difflib import SequenceMatcher

WORD = re.compile(r"\w+(?:['\u2019-]\w+)*", re.UNICODE)
MAX_RULES = 32
MAX_TEXT = 4000
# Independent, unchanged examples: candidates must preserve all of these.
KNOWN_GOOD = (
    "Tell me a joke about databases.",
    "What time is the meeting?",
    "Remind me to call Mom tomorrow.",
    "The meeting is at four pm on Tuesday.",
    "Run npm install and open index.tsx.",
    "Keep the API key out of the logs.",
    "I mean what I said.",
    "Actually, that is correct.",
    "The voice box is part of the larynx.",
    "Put the git hub on the workbench.",
    "Send the report to Morgan.",
    "Do not delete the database.",
    "The total is $15.50, not $50.",
    "We need three hundred records.",
    "Open the box and close the door.",
    "The file is in src/components.",
)


@dataclass(frozen=True)
class Example:
    id: str
    capture_id: str
    original: str
    expected: str
    language: str | None
    # CaptureFeedback.source: "manual", "voice_fix" or "redictation".
    source: str = "manual"


def words(text):
    return [m.group() for m in WORD.finditer(text)]


# What Whisper writes for a pause between words of one phrase ("merge, domain").
_PAUSE = r",?\s+"
# After the last word of a take: closing punctuation only.
_TAKE_END = r"[\s.!?,;:]*\Z"


def candidate(example):
    """Learn one short replacement anchored on both sides.

    An anchor is an unchanged word, or the start or end of the take where
    the edit has no word on that side ("Commit, merge, domain" -> "Commit,
    merge to main"). Context stays in the matching key so a correction never
    becomes a global single-word replacement. Insertions/deletions and
    multi-edit rewrites are intentionally not learned automatically.
    """
    old, new = words(example.original), words(example.expected)
    edits = [op for op in SequenceMatcher(None, old, new, autojunk=False).get_opcodes() if op[0] != "equal"]
    if len(edits) != 1:
        return None
    tag, a, b, c, d = edits[0]
    if tag != "replace" or not (1 <= b - a <= 3 and 1 <= d - c <= 3):
        return None
    edges = [a == 0, b == len(old)]
    matches = list(WORD.finditer(example.original))
    if edges[0] and example.original[: matches[0].start()].strip():
        return None
    if edges[1] and not re.fullmatch(_TAKE_END, example.original[matches[-1].end() :]):
        return None
    # Only word phrases, where a comma may stand for a pause; never learn
    # edits to code punctuation.
    first, last = a - (not edges[0]), b - edges[1]
    phrase = example.original[matches[first].start() : matches[last].end()]
    if not re.fullmatch(r"[\w'\u2019\-]+(?:" + _PAUSE + r"[\w'\u2019\-]+)*", phrase):
        return None
    source = old[first : last + 1]
    replacement = new[c:d]
    old_term, new_term = " ".join(old[a:b]), " ".join(replacement)
    if any(char.isdigit() for char in old_term + new_term):
        return None
    # Letters, not word breaks: Whisper splits and joins words it mishears
    # ("domain" for "to main").
    if SequenceMatcher(None, *(term.casefold().replace(" ", "") for term in (old_term, new_term))).ratio() < 0.6:
        return None
    key = (source, replacement, example.language, edges)
    identity = hashlib.sha256(repr(key).encode()).hexdigest()[:16]
    return {"id": identity, "source": source, "replacement": replacement, "language": example.language, "edges": edges}


def _pattern(rule):
    if "edges" not in rule:
        # Rules learned before edges: a word on each side, spaces only.
        return r"(?<![\w'\u2019-])" + r"\s+".join(re.escape(w) for w in rule["source"]) + r"(?![\w'\u2019-])"
    start, end = rule["edges"]
    return (
        (r"\A\s*" if start else r"(?<![\w'\u2019-])")
        + _PAUSE.join(re.escape(w) for w in rule["source"])
        + (r"(?=" + _TAKE_END + ")" if end else r"(?![\w'\u2019-])")
    )


def compile_rules(rules):
    return tuple((rule, re.compile(_pattern(rule))) for rule in rules)


def apply_rules(text, compiled, language=None):
    """Apply at most one rule per span, preserving context and punctuation."""
    edits = []
    for rule, pattern in compiled:
        if rule["language"] != language:
            continue
        for match in pattern.finditer(text):
            tokens = list(WORD.finditer(match.group()))
            replacement = " ".join(rule["replacement"])
            start_edge, end_edge = rule.get("edges", (False, False))
            end = match.start() + tokens[-1 if end_edge else -2].end()
            if start_edge:
                start = match.start() + tokens[0].start()
            elif "edges" in rule:
                # From the anchor's end: a pause before the misheard words goes with them.
                start, replacement = match.start() + tokens[0].end(), " " + replacement
            else:
                start = match.start() + tokens[1].start()
            edits.append((start, end, replacement))
            if len(edits) > 128:
                return text
    # Overlapping candidates are ambiguous: leave those spans untouched.
    safe = [
        edit
        for edit in edits
        if not any(other != edit and edit[0] < other[1] and other[0] < edit[1] for other in edits)
    ]
    for start, end, replacement in sorted(set(safe), reverse=True):
        text = text[:start] + replacement + text[end:]
    return text


def loss(actual, expected):
    """Token edit distance, including punctuation and capitalization."""
    a, b = re.findall(r"\w+|[^\w\s]", actual), re.findall(r"\w+|[^\w\s]", expected)
    previous = list(range(len(b) + 1))
    for i, token in enumerate(a, 1):
        current = [i]
        for j, target in enumerate(b, 1):
            current.append(min(current[-1] + 1, previous[j] + 1, previous[j - 1] + (token != target)))
        previous = current
    return previous[-1]


def _improves(rule, before, example):
    prior, after = compile_rules(before), compile_rules([*before, rule])
    return loss(apply_rules(example.original, after, example.language), example.expected) < loss(
        apply_rules(example.original, prior, example.language), example.expected
    )


def _corrected(rule):
    """The words the rule writes, with its anchor words, and the edges it is anchored to."""
    start, end = rule.get("edges", (False, False))
    source = rule["source"]
    return [*source[:1][: not start], *rule["replacement"], *source[-1:][: not end]], start, end


def _contains(haystack, needle, start, end):
    spots = range(len(haystack) - len(needle) + 1)
    return any(
        haystack[i : i + len(needle)] == needle
        for i in spots
        if (not start or i == 0) and (not end or i == len(haystack) - len(needle))
    )


def _rewrites(rule, example):
    """The report wrote the rule's corrected phrase that its take misheard some other way.

    Whisper mishears a phrase differently each time ("merge, domain",
    "merged, been"): a report fixing another mishearing to the same words
    is a further recording where the user meant them.
    """
    phrase, start, end = _corrected(rule)
    return _contains(words(example.expected), phrase, start, end) and not _contains(
        words(example.original), phrase, start, end
    )


def _confirms(rule, before, example):
    return _improves(rule, before, example) or _rewrites(rule, example)


def _heldout_evidence(examples, active, blocked):
    """Derive on two distinct takes; require improvement on an unseen third.

    The chronological final third is withheld from candidate generation.
    """
    split = max(0, len(examples) * 2 // 3)
    training, heldout = examples[:split], examples[split:]
    groups = defaultdict(list)
    for example in training:
        rule = candidate(example)
        if rule and rule["id"] not in blocked:
            groups[rule["id"]].append((rule, example))
    proposed = []
    for group in groups.values():
        taught = {example.capture_id for _, example in group}
        if len(taught) < 2:
            continue
        rule = group[0][0]
        if any(_confirms(rule, [], e) for e in heldout if e.capture_id not in taught):
            proposed.append(rule)

    def confirmed(rule, before):
        return any(_confirms(rule, before, e) for e in heldout)

    return list(active), proposed, confirmed


def _every_report_evidence(examples, active, blocked):
    """Two recordings teach a rule and a third confirms it; every report counts at once.

    The third is fixed by the rule or writes its corrected phrase where its
    take misheard it another way (``_rewrites``).

    At least one teaching report must be explicit (a redictation only backs a
    rule up). No report is held back by age, so a new one is evidence as soon
    as it is saved. Active rules are checked the same way, so one that lost its
    support (a withdrawn or deleted report) is dropped.
    """
    teachers, candidates = defaultdict(list), {}
    for example in examples:
        rule = candidate(example)
        if rule:
            teachers[rule["id"]].append(example)
            candidates[rule["id"]] = rule

    def confirmed(rule, before):
        improved = [e for e in examples if _improves(rule, before, e)]
        taught = [e for e in teachers[rule["id"]] if e in improved]
        confirming = improved + [e for e in examples if e not in improved and _rewrites(rule, e)]
        return (
            len({e.capture_id for e in confirming}) >= 3
            and len({e.capture_id for e in taught}) >= 2
            and any(e.source != "redictation" for e in taught)
        )

    proposed = [
        candidates[rule_id]
        for rule_id, group in teachers.items()
        if rule_id not in blocked and len({e.capture_id for e in group}) >= 2
    ]
    return [rule for rule in active if confirmed(rule, [])], proposed, confirmed


def evaluate(examples, active, blocked=(), every_report=False):
    """Add a rule only on independent evidence; never regress a saved example.

    ``every_report`` (the voice_edits beta, backend/beta.py) lets new reports
    count at once instead of holding the newest third back. Duplicate
    utterances cannot supply independent evidence.
    """
    unique = {}
    for example in examples:
        unique[(example.language, example.original)] = example
    examples = list(unique.values())
    evidence = _every_report_evidence if every_report else _heldout_evidence
    retained, proposed, confirmed = evidence(examples, active, blocked)
    # Require each addition to be confirmed and regress nowhere.
    selected = list(retained)
    validation = [(e.original, e.expected, e.language) for e in examples]
    validation += [(e.expected, e.expected, e.language) for e in examples]
    languages = {e.language for e in examples} | {None}
    validation += [(text, text, language) for text in KNOWN_GOOD for language in languages]
    accepted = 0
    for rule in sorted(proposed, key=lambda r: r["id"]):
        if rule["id"] in {r["id"] for r in selected} or len(selected) >= MAX_RULES:
            continue
        before, after = compile_rules(selected), compile_rules([*selected, rule])
        if any(
            loss(apply_rules(text, after, lang), expected) > loss(apply_rules(text, before, lang), expected)
            for text, expected, lang in validation
        ):
            continue
        if not confirmed(rule, selected):
            continue
        selected.append(rule)
        accepted += 1
    compiled = compile_rules(selected)
    # Worst-size input, repeated timings: no GPU/model calls and a hard 5 ms gate.
    probe = (" ".join(e.original for e in examples) + " ".join(KNOWN_GOOD))[:MAX_TEXT]
    probe = (probe + " ") * (MAX_TEXT // max(1, len(probe)) + 1)
    timings = []
    for _ in range(25):
        start = time.perf_counter()
        for language in languages:
            apply_rules(probe[:MAX_TEXT], compiled, language)
        timings.append((time.perf_counter() - start) * 1000)
    latency = statistics.median(timings)
    passed = latency <= 5
    # Dropping unsupported rules only makes the layer faster.
    return (selected if passed else retained), {
        "examples": len(examples),
        "candidates": len(proposed),
        "accepted": accepted if passed else 0,
        "withdrawn": len(active) - len(retained),
        "median_rule_ms": round(latency, 3),
        "latency_passed": passed,
    }
