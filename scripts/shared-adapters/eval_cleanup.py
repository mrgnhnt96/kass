#!/usr/bin/env python3
"""Score a cleanup model, with or without an adapter, on the frozen test set.

Run from the repository root with the model already downloaded:

    backend/venv/bin/python scripts/shared-adapters/eval_cleanup.py --size 4B
    backend/venv/bin/python scripts/shared-adapters/eval_cleanup.py --size 4B \
        --adapter build/shared-adapters/cleanup-4B --out build/shared-adapters/eval-4B.json

Each case runs through production cleanup: the same preprocessing, prompt,
examples and sampler, with no personal examples, notes or rules (an empty
data directory). Prints errors per category (token edits, counting
punctuation and capitals), exact matches, changed facts, words lost and
latency. See
docs/plans/SHARED_ADAPTERS.md.
"""

import argparse
import json
import os
import re
import statistics
import sys
import tempfile
import time
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TOKENIZERS_PARALLELISM"] = "false"
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from cleanup_test import CASES

SEEDS = (17, 29)


def dropped(output, expected):
    """Words the speaker meant (in ``expected``) that the output left out.

    Edit counts treat a lost "Can you" like a missing comma; this counts
    only lost words, the mistake that changes what the user said.
    """
    left = re.findall(r"[\w']+", output.casefold())
    missing = 0
    for word in re.findall(r"[\w']+", expected.casefold()):
        if word in left:
            left.remove(word)
        else:
            missing += 1
    return missing


def run(size, adapter, seeds):
    import mlx.core as mx
    from mlx_lm import generate, load
    from mlx_lm.sample_utils import make_sampler

    from backend import config
    from backend.backends.base import local_model_path
    from backend.backends.qwen_llm_backend import MLX_HF_REPOS, _build_messages
    from backend.services.correction_rules import loss
    from backend.services.model_improvement.evaluation import preserves_facts
    from backend.services.refinement import (
        RefinementFlags,
        build_refinement_prompt,
        prepare_refinement,
        refinement_examples,
    )

    config.set_data_dir(tempfile.mkdtemp(prefix="kass-eval-"))
    path = local_model_path(MLX_HF_REPOS[size], (".safetensors", ".bin", ".npz"))
    model, tokenizer = load(path, adapter_path=adapter, tokenizer_config={"trust_remote_code": False})
    flags = RefinementFlags()
    system = build_refinement_prompt(flags)
    examples = refinement_examples(flags)
    rows = []
    for seed in seeds:
        for index, (category, raw, expected) in enumerate(CASES):
            cleaned, bypass = prepare_refinement(raw, flags)
            started = time.perf_counter()
            prompt_tokens = 0
            if bypass is None:
                messages = _build_messages(cleaned, system, examples)
                prompt = tokenizer.apply_chat_template(
                    messages, tokenize=False, add_generation_prompt=True, enable_thinking=False
                )
                prompt_tokens = len(tokenizer.encode(prompt))
                mx.random.seed(seed)
                output = generate(
                    model,
                    tokenizer,
                    prompt=prompt,
                    max_tokens=512,
                    sampler=make_sampler(temp=0.2, top_p=0.9),
                    verbose=False,
                ).strip()
            else:
                output = bypass
            rows.append(
                {
                    "case": index,
                    "seed": seed,
                    "category": category,
                    "raw": raw,
                    "expected": expected,
                    "output": output,
                    "errors": loss(output, expected),
                    "facts_kept": preserves_facts(output, expected),
                    "dropped": dropped(output, expected),
                    "model": bypass is None,
                    "seconds": time.perf_counter() - started,
                    "prompt_tokens": prompt_tokens,
                }
            )
    return rows


def summarize(rows):
    categories = {}
    for row in rows:
        entry = categories.setdefault(
            row["category"], {"cases": 0, "errors": 0, "exact": 0, "facts_changed": 0, "words_dropped": 0}
        )
        entry["cases"] += 1
        entry["errors"] += row["errors"]
        entry["exact"] += row["errors"] == 0
        entry["facts_changed"] += not row["facts_kept"]
        entry["words_dropped"] += row["dropped"]
    timed = [row["seconds"] for row in rows if row["model"]]
    tokens = [row["prompt_tokens"] for row in rows if row["model"]]
    return {
        "categories": categories,
        "errors": sum(row["errors"] for row in rows),
        "exact": sum(row["errors"] == 0 for row in rows),
        "cases": len(rows),
        "facts_changed": sum(not row["facts_kept"] for row in rows),
        "words_dropped": sum(row["dropped"] for row in rows),
        "median_seconds": statistics.median(timed) if timed else 0,
        "median_prompt_tokens": statistics.median(tokens) if tokens else 0,
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--size", default="4B", choices=("0.6B", "1.7B", "4B"))
    parser.add_argument("--adapter", help="An adapter directory; omit for the stock model")
    parser.add_argument("--seeds", type=int, default=1, choices=(1, 2), help="Sampling seeds per case")
    parser.add_argument("--out", help="Write every output and the summary to this JSON file")
    parser.add_argument("--show", action="store_true", help="Print each case that has errors")
    args = parser.parse_args()

    rows = run(args.size, args.adapter, SEEDS[: args.seeds])
    summary = summarize(rows)
    print(f"{args.size} {'+ ' + args.adapter if args.adapter else '(stock)'}")
    print(f"{'category':<16}{'cases':>6}{'errors':>8}{'exact':>7}{'facts':>7}{'lost':>6}")
    for name, entry in [*summary["categories"].items(), ("total", summary)]:
        print(
            f"{name:<16}{entry['cases']:>6}{entry['errors']:>8}{entry['exact']:>7}"
            f"{entry['facts_changed']:>7}{entry['words_dropped']:>6}"
        )
    print(
        f"median {summary['median_seconds'] * 1000:.0f} ms per cleanup, {summary['median_prompt_tokens']:.0f} prompt tokens"
    )
    if args.show:
        for row in rows:
            if row["errors"]:
                print(f"\n[{row['category']}] {row['raw']}\n  want: {row['expected']}\n  got:  {row['output']}")
    if args.out:
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        from cleanup_pipeline import pipeline_id

        result = {"size": args.size, "adapter": args.adapter, "pipeline": pipeline_id(), "summary": summary}
        Path(args.out).write_text(json.dumps(result | {"rows": rows}, indent=2))


if __name__ == "__main__":
    main()
