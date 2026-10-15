"""Acceptance gates on generated outputs, not on training loss."""

import math
import re
import statistics

from ..correction_rules import loss

PROTECTED_FACTS = (
    r"\b\d+(?:[.,]\d+)*\b",
    r"\b(?:not|never|cannot|can't|don't|doesn't|won't)\b",
    r"\b(?:zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred|thousand|million|billion)\b",
)


def preserves_facts(actual, expected):
    return all(
        re.findall(pattern, actual.casefold()) == re.findall(pattern, expected.casefold())
        for pattern in PROTECTED_FACTS
    )


def changes_facts(candidate, expected, baseline):
    """Whether ``candidate`` changed a protected fact that the model in use kept.

    A report whose fix the model in use also misses ("second item" written
    as "2.") is a mistake to learn from, not a fact the candidate changed;
    it already counts in the error totals.
    """
    return not preserves_facts(candidate, expected) and not preserves_facts(candidate, baseline)


def score_rows(rows):
    """All rows contain A/B outputs from the same prompt, seed and pipeline."""
    if not rows:
        return {"passed": False, "reasons": ["No independent evaluation examples"]}
    reasons = []
    baseline = candidate = 0
    improved = set()
    audio_groups = set()
    for row in rows:
        before = loss(row["baseline"], row["expected"])
        after = loss(row["candidate"], row["expected"])
        if after > before:
            reasons.append(f"Regression: {row['id']}")
        if row["kind"] in ("heldout", "audio"):
            baseline += before
            candidate += after
            if after < before:
                improved.add(row["id"])
        if row["kind"] == "audio":
            audio_groups.add(row["id"])
        # Numerical facts and negations are protected even if total edit score
        # improves. Cleanup must not change these to compensate for other wins.
        if changes_facts(row["candidate"], row["expected"], row["baseline"]):
            reasons.append(f"Changed protected facts: {row['id']}")
    if candidate >= baseline or len(improved) < 2:
        reasons.append("No improvement on at least two independent held-out recordings")
    if len(audio_groups) < 5:
        reasons.append("Fewer than five independent recorded-audio checks")
    cold_before = max(r.get("baseline_cold_seconds", 0) for r in rows)
    cold_after = max(r.get("candidate_cold_seconds", 0) for r in rows)
    if cold_after > cold_before * 1.1 + 0.25:
        reasons.append("Cold model load and first generation latency regressed")
    before_times = [r["baseline_seconds"] for r in rows]
    after_times = [r["candidate_seconds"] for r in rows]
    before_p95 = sorted(before_times)[max(0, math.ceil(len(rows) * 0.95) - 1)]
    after_p95 = sorted(after_times)[max(0, math.ceil(len(rows) * 0.95) - 1)]
    if statistics.median(after_times) > statistics.median(before_times) * 1.1 + 0.05:
        reasons.append("Median inference latency regressed")
    if after_p95 > before_p95 * 1.1 + 0.1:
        reasons.append("P95 inference latency regressed")
    before_memory = max(r["baseline_memory"] for r in rows)
    after_memory = max(r["candidate_memory"] for r in rows)
    if after_memory > min(16 * 1024**3, before_memory * 1.2 + 512 * 1024**2):
        reasons.append("Inference memory limit exceeded")
    return {
        "passed": not reasons,
        "reasons": sorted(set(reasons)),
        "baseline_errors": baseline,
        "candidate_errors": candidate,
        "improved_recordings": len(improved),
        "audio_recordings": len(audio_groups),
        "median_baseline_seconds": statistics.median(before_times),
        "median_candidate_seconds": statistics.median(after_times),
        "p95_baseline_seconds": before_p95,
        "p95_candidate_seconds": after_p95,
        "baseline_memory": before_memory,
        "candidate_memory": after_memory,
    }


def score_speech(rows):
    """Speech selection uses real audio, paired WER-like word edit counts."""
    if len({r["id"] for r in rows}) < 5:
        return {"passed": False, "reasons": ["Need five independent raw-transcript audio reports"]}
    reasons = []
    before = after = 0
    for row in rows:

        def normalize(text):
            return " ".join(re.findall(r"\w+", text.casefold()))

        baseline = loss(normalize(row["baseline"]), normalize(row["expected"]))
        candidate = loss(normalize(row["candidate"]), normalize(row["expected"]))
        before += baseline
        after += candidate
        if changes_facts(row["candidate"], row["expected"], row["baseline"]):
            reasons.append("Speech changed protected facts")
        if candidate > baseline:
            reasons.append("Speech accuracy regressed on a recording")
    if after >= before:
        reasons.append("Speech accuracy did not improve")
    if max(r["candidate_seconds"] for r in rows) > max(r["baseline_seconds"] for r in rows) * 1.1 + 0.1:
        reasons.append("Speech latency regressed")
    if max(r["candidate_memory"] for r in rows) > min(
        16 * 1024**3, max(r["baseline_memory"] for r in rows) * 1.2 + 512 * 1024**2
    ):
        reasons.append("Speech memory regressed")
    return {"passed": not reasons, "reasons": reasons, "baseline_errors": before, "candidate_errors": after}
