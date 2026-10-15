"""When a trained voice adapter may replace the current speech model.

Rows come from ``train.evaluate``: each test take transcribed clean, with a
person talking 6 and 0 dB below the user, and with room noise 5 dB below,
by plain turbo ("base"), the model in use ("production", absent when that
is plain turbo) and the candidate. Corrected takes, when there are any, are
rows with condition "correction". A candidate passes only if:

- it makes clearly fewer mistakes than the model in use with talk and noise,
- clean takes are no worse than with plain turbo or the model in use (0.5% of
  words allowed, or 3% when it cuts noisy mistakes by a quarter or more),
- the user's corrected takes are no worse,
- it's no slower and uses no more memory.
"""

import math
import re
import statistics

from ..correction_rules import loss

MIN_TAKES = 12
# Cutting noisy mistakes by this share lets clean takes lose up to
# BIG_WIN_CLEAN_SHARE of their words; otherwise the allowance is 0.5%.
BIG_WIN = 0.25
BIG_WIN_CLEAN_SHARE = 0.03


def _normalize(text: str) -> str:
    return " ".join(re.findall(r"\w+", text.casefold()))


def errors(text: str, expected: str) -> int:
    return loss(_normalize(text), _normalize(expected))


def _totals(rows, key):
    out = {"clean": 0, "noisy": 0, "correction": 0}
    for row in rows:
        group = row["condition"] if row["condition"] in ("clean", "correction") else "noisy"
        out[group] += errors(row[key], row["expected"])
    return out


def score_voice(rows: list[dict]) -> dict:
    takes = {row["id"] for row in rows if row["condition"] != "correction"}
    if len(takes) < MIN_TAKES:
        return {"passed": False, "reasons": [f"Need at least {MIN_TAKES} test takes"], "takes": len(takes)}
    current = "production" if all("production" in row for row in rows) else "base"
    base, production, candidate = _totals(rows, "base"), _totals(rows, current), _totals(rows, "candidate")
    words = {"clean": 0, "noisy": 0}
    for row in rows:
        if row["condition"] != "correction":
            words["clean" if row["condition"] == "clean" else "noisy"] += len(_normalize(row["expected"]).split())
    reasons = []
    if production["noisy"] - candidate["noisy"] < max(2, math.ceil(0.03 * production["noisy"])):
        reasons.append("It didn't make clearly fewer mistakes with background talk and noise")

    # A big win in noise is worth a few clean mistakes (the user's call,
    # 2026-10-02); a small one isn't. Judged against plain turbo and the model
    # in use separately, so a candidate that beats the model in use on both
    # counts isn't held to turbo's clean score without its noise win.
    def allowance(reference):
        big_win = candidate["noisy"] <= (1 - BIG_WIN) * reference["noisy"]
        if big_win:
            return max(1, math.ceil(BIG_WIN_CLEAN_SHARE * words["clean"]))
        return max(1, round(0.005 * words["clean"]))

    if any(candidate["clean"] > reference["clean"] + allowance(reference) for reference in (base, production)):
        reasons.append("Clean takes got worse")
    if any(row["condition"] == "correction" for row in rows) and candidate["correction"] > production["correction"]:
        reasons.append("Your corrected takes got worse")
    timed = [row for row in rows if row["condition"] != "correction"]
    if (
        statistics.median(r["candidate_seconds"] for r in timed)
        > statistics.median(r["base_seconds"] for r in timed) * 1.1 + 0.05
    ):
        reasons.append("Recognition got slower")
    if max(r["candidate_memory"] for r in timed) > max(r["base_memory"] for r in timed) * 1.2 + 512 * 1024**2:
        reasons.append("Recognition used more memory")
    return {
        "passed": not reasons,
        "reasons": reasons,
        "takes": len(takes),
        "words": words,
        "base": base,
        "production": production,
        "candidate": candidate,
    }


# A candidate is only built on when it's no worse than the model in use:
# continuing from ones that traded clean takes for noise let clean mistakes
# grow from 7 to 15 over a week (2026-10-14), as each run started from a
# model already off. The allowance on clean takes is the gate's small one.
CONTINUE_CLEAN_SHARE = 0.005


def worth_continuing(metrics: dict) -> bool:
    """Whether the next run should continue training from this candidate.

    True when it beat the model in use with talk and noise, kept the user's
    corrected takes, and is no worse on clean takes than plain turbo or the
    model in use (``CONTINUE_CLEAN_SHARE`` of words). Activation still needs
    ``score_voice`` to pass.
    """
    candidate, current, base = metrics.get("candidate"), metrics.get("production"), metrics.get("base")
    if not candidate or not current or not base:
        return False
    margin = max(1, round(CONTINUE_CLEAN_SHARE * metrics["words"]["clean"]))
    return (
        candidate["noisy"] < current["noisy"]
        and candidate["correction"] <= current["correction"]
        and candidate["clean"] <= min(base["clean"], current["clean"]) + margin
    )
