#!/usr/bin/env python3
"""Train the shared cleanup adapter for one Qwen3 size.

    backend/venv/bin/python scripts/shared-adapters/make_cleanup_data.py
    backend/venv/bin/python scripts/shared-adapters/train_cleanup.py --size 4B

The adapter has the same shape as a personal one (rank 8 on the last four
layers, model_improvement/worker.py), so a Mac's own training can continue
from it. Pairs go through production preprocessing and the production
prompt, and only the answer counts toward the loss. Writes
build/shared-adapters/cleanup-<size>/. Score it with eval_cleanup.py. See
docs/plans/SHARED_ADAPTERS.md.

Training runs only after two minutes without keyboard or mouse input, so
leave the Mac alone (overnight is easiest). It trains in chunks of 100
examples and redoes a chunk that macOS stops.
"""

import argparse
import hashlib
import json
import os
import sys
import tempfile
import time
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TOKENIZERS_PARALLELISM"] = "false"
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))

from idle import interrupted, wait_for_idle  # noqa: E402

DATA = ROOT / "build" / "shared-adapters" / "cleanup-data"
LORA = {"rank": 8, "dropout": 0.0, "scale": 16.0}
LAYERS = 4
CHUNK = 100


def read(split):
    return [json.loads(line) for line in (DATA / f"{split}.jsonl").read_text().splitlines()]


def dataset(samples, tokenizer):
    """Token sequences for ``samples``, prepared by the worker's Python copy
    of dictation's cleanup (the app's server prepares them in Rust)."""
    from backend.services.model_improvement.worker import token_dataset
    from backend.services.refinement import (
        RefinementFlags,
        build_refinement_prompt,
        prepare_refinement,
        refinement_examples,
    )

    cases = []
    for sample in samples:
        flags = RefinementFlags.from_dict(sample.get("flags"))
        text, bypass = prepare_refinement(sample["raw"], flags)
        cases.append(
            {"text": text, "bypass": bypass, "system": build_refinement_prompt(flags), "examples": refinement_examples(flags)}
        )
    return token_dataset(samples, tokenizer, cases)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--size", default="4B", choices=("0.6B", "1.7B", "4B"))
    parser.add_argument("--iters", type=int, default=2000)
    parser.add_argument("--accumulate", type=int, default=4, help="Examples per update")
    parser.add_argument("--lr", type=float, default=5e-5)
    parser.add_argument("--out", help="Adapter directory (default build/shared-adapters/cleanup-<size>)")
    args = parser.parse_args()

    import mlx.core as mx
    import mlx.optimizers as optim
    import numpy as np
    from mlx.utils import tree_flatten
    from mlx_lm import load
    from mlx_lm.tuner.trainer import TrainingArgs, train
    from mlx_lm.tuner.utils import linear_to_lora_layers

    from backend import config
    from backend.backends.base import local_model_path
    from backend.backends.qwen_llm_backend import MLX_HF_REPOS
    from backend.services.refinement import REFINEMENT_EXAMPLES

    # No personal styles, notes or rules: an empty data directory.
    config.set_data_dir(tempfile.mkdtemp(prefix="kass-train-"))
    mx.set_memory_limit(16 * 1024**3)
    np.random.seed(17)
    mx.random.seed(17)
    repo = MLX_HF_REPOS[args.size]
    model, tokenizer = load(local_model_path(repo, (".safetensors", ".bin", ".npz")))
    training = dataset(read("train"), tokenizer)
    validation = dataset(read("valid"), tokenizer)
    rehearsal = dataset([{"raw": raw, "expected": expected} for raw, expected in REFINEMENT_EXAMPLES], tokenizer)
    print(f"{len(training)} training pairs, {len(validation)} validation pairs (the rest resolved before the model)")
    model.freeze()
    linear_to_lora_layers(model, LAYERS, LORA)
    destination = Path(args.out) if args.out else ROOT / "build" / "shared-adapters" / f"cleanup-{args.size}"
    destination.mkdir(parents=True, exist_ok=True)
    # The same config a personal adapter writes, with the repo rather than a
    # local path, so it loads on any Mac that has the model.
    (destination / "adapter_config.json").write_text(
        json.dumps(
            {"fine_tune_type": "lora", "num_layers": LAYERS, "lora_parameters": LORA, "kass_base_path": repo},
            indent=2,
        )
    )
    started = time.monotonic()
    adapter_file = destination / "adapters.safetensors"
    schedule = optim.cosine_decay(args.lr, args.iters // args.accumulate, args.lr / 10)

    def fresh_optimizer(done):
        # Continues the learning-rate schedule from where training stands.
        offset = done // args.accumulate
        return optim.Adam(learning_rate=lambda step: schedule(step + offset))

    # A stopped GPU pass leaves the weights and optimizer state unusable, so
    # every retry starts from saved weights and a new optimizer.
    mx.save_safetensors(str(adapter_file), dict(tree_flatten(model.trainable_parameters())))
    optimizer = fresh_optimizer(0)
    done, retries = 0, 0
    # Train in chunks, saving after each. macOS stops a GPU job that keeps the
    # screen or a dictation waiting ("Impacting Interactivity"); a stopped
    # chunk starts over from the last save.
    while done < args.iters:
        chunk = min(CHUNK, args.iters - done)
        wait_for_idle()
        evaluate = done == 0 or (done // CHUNK) % 5 == 4 or done + chunk == args.iters
        try:
            train(
                model=model,
                optimizer=optimizer,
                args=TrainingArgs(
                    # One example per GPU pass, so each pass is short.
                    batch_size=1,
                    grad_accumulation_steps=args.accumulate,
                    iters=chunk,
                    val_batches=40,
                    steps_per_report=chunk,
                    steps_per_eval=chunk,
                    steps_per_save=chunk,
                    adapter_file=adapter_file,
                    max_seq_length=4096,
                ),
                train_dataset=training + rehearsal,
                val_dataset=validation if evaluate else None,
            )
        except RuntimeError as error:
            if not interrupted(error):
                raise
            retries += 1
            print(f"interrupted by macOS at {done}; retrying the chunk ({retries})", flush=True)
            model.load_weights(str(adapter_file), strict=False)
            optimizer = fresh_optimizer(done)
            continue
        done += chunk
        print(f"{done}/{args.iters} examples", flush=True)
    (destination / "training.json").write_text(
        json.dumps(
            {
                "size": args.size,
                "repo": repo,
                "iters": args.iters,
                "accumulate": args.accumulate,
                "lr": args.lr,
                "interruptions": retries,
                "train_pairs": len(training),
                "validation_pairs": len(validation),
                "minutes": round((time.monotonic() - started) / 60, 1),
                "data_sha256": hashlib.sha256((DATA / "train.jsonl").read_bytes()).hexdigest(),
                "adapter_sha256": hashlib.sha256((destination / "adapters.safetensors").read_bytes()).hexdigest(),
            },
            indent=2,
        )
    )
    print(f"wrote {destination}")


if __name__ == "__main__":
    main()
