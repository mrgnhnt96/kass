#!/usr/bin/env python3
"""Run Command Mode's eight test rewrites on the local Qwen3 models and check them.

Run from the repository root with backend/venv/bin/python, with the models
already downloaded:

    backend/venv/bin/python scripts/eval-command-mode.py --models 0.6B,1.7B,4B --samples 3

Each rewrite is prefilled with its selection first, as while the user speaks,
and timed from the instruction to the result. The checks are rough (length,
kept facts, language); read the outputs too. See docs/plans/COMMAND_MODE.md,
"Quality on the shared model".
"""

import argparse
import asyncio
import json
import logging
import os
import re
import sys
import time
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

CASES = [
    (
        "I think that we should maybe try and get the new onboarding flow out the door before the end of the month, "
        "but honestly I'm not totally sure if the design team is going to have the mockups ready in time, so we might "
        "need to push it.",
        "make this more concise",
    ),
    ("hey can you send me the slides from yesterday? i want to go over them before the call", "translate to Spanish"),
    ("yo, the build is broken again. somebody pushed without running tests. fix it pls", "rewrite this more formally"),
    (
        "For the release we need to bump the version, update the changelog, tag the commit, and publish the binaries "
        "to the downloads page.",
        "turn this into bullet points",
    ),
    (
        "i went to the store yesterday and buyed some apple's, then i realise i forget my wallet so i had to go back home",
        "polish this",
    ),
    (
        "write a python script that reads a csv and gives me the average of the price column, it should skip rows "
        "where price is empty",
        "prompt engineer",
    ),
    ("Thanks so much for your help with this, I really appreciate it!", "make it sound more casual"),
    ("The meeting is moved to 3pm on Thursday in room 204.", "translate this to French"),
]

_SPANISH = re.compile(r"[¿¡ñ]|\b(el|los|las|por|para|que|ha|está)\b", re.I)
_EMOJI = re.compile(r"[\U0001F300-\U0001FAFF]")


def _words(text: str) -> str:
    return re.sub(r"\W+", " ", text.lower()).strip()


def passes(selection: str, spoken: str, out: str) -> bool:
    o = out.lower()
    if spoken == "make this more concise":
        # "push it" means delay it; "push the flow" reverses the meaning.
        return bool(
            len(out) <= 0.8 * len(selection)
            and "month" in o
            and ("design" in o or "mockup" in o)
            and re.search(r"push it|delay|postpone|push (it|the \w+( \w+)?) back", o)
            and not re.search(r"(should|will|might|may|need to) push the", o)
        )
    if spoken == "translate to Spanish":
        return all(word in o for word in ("ayer", "llam", "envi"))
    if spoken == "rewrite this more formally":
        return not _SPANISH.search(out) and "test" in o and not re.search(r"\byo\b|\bpls\b|somebody|you are correct", o)
    if spoken == "turn this into bullet points":
        return out.startswith("- ") and out.count("\n- ") == 3
    if spoken == "polish this":
        return all(word in o for word in ("bought", "apples", "forgot")) and ("realized" in o or "realised" in o)
    if spoken == "prompt engineer":
        return all(word in o for word in ("csv", "average", "empty", "price")) and len(out) > 200
    if spoken == "make it sound more casual":
        return not _SPANISH.search(out) and "thank" in o and _words(out) != _words(selection) and not _EMOJI.search(out)
    if spoken == "translate this to French":
        return "jeudi" in o and ("15" in o or "3 " in o) and "204" in o and "réunion" in o
    raise KeyError(spoken)


async def evaluate(sizes: list[str], samples: int) -> dict:
    from backend.backends import get_llm_backend
    from backend.services import commands

    backend = get_llm_backend()
    transforms = commands.default_transforms()
    results = {}
    for size in sizes:
        await backend.load_model(size)
        await commands.rewrite("Hello there.", "make it formal", size)
        rows = []
        for sample in range(samples):
            for selection, spoken in CASES:
                instruction, _ = commands.resolve_instruction(spoken, transforms)
                await commands.prefill(selection, size)
                started = time.monotonic()
                text, _ = await commands.rewrite(selection, instruction, size)
                seconds = time.monotonic() - started
                ok = passes(selection, spoken, text)
                rows.append(dict(sample=sample, spoken=spoken, seconds=round(seconds, 3), passes=ok, out=text))
                print(
                    f"[{size} #{sample}] {'ok  ' if ok else 'FAIL'} {seconds:.2f}s {spoken!r}\n    -> {text!r}",
                    flush=True,
                )
        short = sorted(r["seconds"] for r in rows if r["spoken"] != "prompt engineer")
        print(
            f"[{size}] {sum(r['passes'] for r in rows)}/{len(rows)} pass; "
            f"short edits {short[0]:.2f}-{short[-1]:.2f} s after the instruction\n",
            flush=True,
        )
        results[size] = rows
    return results


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--models", default="0.6B,1.7B,4B")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--output", type=Path, help="write every output as JSON")
    args = parser.parse_args()
    logging.basicConfig(level=logging.WARNING)
    results = asyncio.run(evaluate(args.models.split(","), args.samples))
    if args.output:
        args.output.write_text(json.dumps(results, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
