"""Isolated MLX trainer/evaluator, also dispatched by the frozen server binary.

No database writes and no deployment authority. Only the parent can promote a
complete result. Killing this process never changes the production model.

The cleanup prompt, its example turns and the text rules are Kass's server's
(Rust): this process sends it the transcripts and gets back what dictation's
cleanup would do with them (``served_cases``, ``served_fixes``).
"""

import argparse
import gc
import hashlib
import json
import os
import re
import threading
import time
from pathlib import Path

from .data import REGRESSION, ask
from .evaluation import score_rows, score_speech


def write_json(path, data):
    path = Path(path)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(data, indent=2))
    temporary.replace(path)


def phase(directory, name):
    write_json(directory / "progress.json", {"phase": name})


# A sentence's end mark, with any closing quotes or brackets after it.
_END_MARK = re.compile(r"[.?!\u2026]+[\"'\u201d\u2019)\]]*$")
# A list item: "- Milk", "* Milk", "1. Milk", "2) Milk".
_LIST_ITEM = re.compile(r"^\s*(?:[-*\u2022]|\d+[.)])\s")


def target(sample):
    """What the cleanup adapter learns to write for ``sample``: the user's fix,
    with an end mark when the fix left it off.

    Fixes typed in Captures often leave off the final period, and the output
    they corrected may have lost it to the style's habit; learning that taught
    the adapter to drop it in every style. Leaving it off is a writing style
    habit, learned per style and applied after the model (``apply_learned``),
    which only ever drops a period. So the mark is the output's, else a period.
    An end mark the user typed ("?" for ".") is kept, and so is a short command
    ("/clear") or a list item written without one.
    """
    expected = sample["expected"].rstrip()
    if _END_MARK.search(expected) or not expected[-1:].isalnum():
        return expected
    if len(re.findall(r"\w+", expected)) < 3 or _LIST_ITEM.match(expected.rsplit("\n", 1)[-1]):
        return expected
    end = _END_MARK.search((sample.get("output") or "").rstrip())
    return expected + (end.group() if end else ".")


def _ask_server(plan, path, body):
    return ask(plan["cleanup_server"], path, body)


def served_cases(plan, samples):
    """Each sample's cleanup input as dictation prepares it, from the server:
    ``{"text", "bypass", "system", "examples"}``, in order. A ``bypass`` is
    the finished text when a spoken edit resolved it without the model."""
    if not samples:
        return []
    cases = [{"text": s["raw"], "flags": s.get("flags")} for s in samples]
    return _ask_server(plan, "/capture/learning/training/prompts", {"cases": cases})["cases"]


def served_fixes(plan, texts, samples):
    """``texts`` with the plan's learned corrections applied in each sample's
    language, as dictation applies them after cleanup."""
    if not texts:
        return []
    cases = [{"text": text, "language": s.get("language")} for text, s in zip(texts, samples, strict=True)]
    return _ask_server(plan, "/capture/learning/training/fixed", {"rules": plan["rules"], "cases": cases})["texts"]


def token_dataset(samples, tokenizer, cases):
    """Token sequences to train on, from ``samples`` and their cleanup inputs
    ``cases`` (``served_cases``). Samples a spoken edit resolves are left out."""
    from ...backends.qwen_llm_backend import _build_messages

    result = []
    for sample, case in zip(samples, cases, strict=True):
        if case["bypass"] is not None:
            continue
        messages = _build_messages(case["text"], case["system"], [tuple(turn) for turn in case["examples"]])
        prefix = tokenizer.apply_chat_template(
            messages, tokenize=True, add_generation_prompt=True, enable_thinking=False
        )
        answer = [*tokenizer.encode(target(sample), add_special_tokens=False), tokenizer.eos_token_id]
        if len(prefix) + len(answer) > 4096:
            continue
        result.append((prefix + answer, len(prefix)))
    return result


def train_adapter(plan, directory):
    import mlx.core as mx
    import mlx.optimizers as optim
    import numpy as np
    from mlx.utils import tree_flatten
    from mlx_lm import load
    from mlx_lm.tuner.trainer import TrainingArgs, train
    from mlx_lm.tuner.utils import linear_to_lora_layers

    np.random.seed(17)
    mx.random.seed(17)
    model, tokenizer = load(plan["model_path"], tokenizer_config={"trust_remote_code": False})

    def dataset(samples, tokenizer):
        return token_dataset(samples, tokenizer, served_cases(plan, samples))

    training = dataset([s for s in plan["samples"] if s["target"] == "refined" and s["split"] == "train"], tokenizer)
    validation = dataset(
        [s for s in plan["samples"] if s["target"] == "refined" and s["split"] == "validation"], tokenizer
    )
    if len(training) < plan.get("min_train", 12) or len(validation) < plan.get("min_validation", 3):
        raise ValueError(
            "Not enough model-eligible training/validation reports after tokenization and deterministic edits"
        )
    # The prompt's own example turns, so the adapter keeps the default cleanup.
    rehearsal = dataset([{"raw": raw, "expected": expected} for raw, expected in plan["rehearsal"]], tokenizer)
    parameters = {"rank": 8, "dropout": 0.0, "scale": 16.0}
    model.freeze()
    linear_to_lora_layers(model, 4, parameters)
    # The start can differ from the model in use: a new shared adapter
    # (manager._prepare) is the start, and the personal one stays the baseline.
    start = plan.get("start_adapter", plan.get("baseline_adapter"))
    if start:
        model.load_weights(str(Path(start) / "adapters.safetensors"), strict=False)
    before = {name: mx.array(value) for name, value in tree_flatten(model.trainable_parameters())}
    mx.eval(before)
    destination = directory / "adapter"
    destination.mkdir(exist_ok=True)
    write_json(
        destination / "adapter_config.json",
        {
            "fine_tune_type": "lora",
            "num_layers": 4,
            "lora_parameters": parameters,
            "kass_base_path": plan["model_path"],
        },
    )
    steps = plan.get("iterations", min(200, max(40, len(training) * 2)))
    args = TrainingArgs(
        batch_size=1,
        iters=steps,
        val_batches=-1,
        steps_per_report=5,
        steps_per_eval=max(1, steps // 4),
        steps_per_save=steps,
        adapter_file=destination / "adapters.safetensors",
        max_seq_length=4096,
    )
    train(
        model=model,
        optimizer=optim.Adam(learning_rate=1e-5),
        args=args,
        train_dataset=training + rehearsal,
        val_dataset=validation,
    )
    changed = any(
        bool(mx.any(value != before[name]).item()) for name, value in tree_flatten(model.trainable_parameters())
    )
    if not changed:
        raise RuntimeError("Training did not change adapter weights")
    write_json(
        directory / "training.json",
        {
            "steps": steps,
            "weights_changed": True,
            "train_examples": len(training),
            "validation_examples": len(validation),
            "adapter_sha256": hashlib.sha256((destination / "adapters.safetensors").read_bytes()).hexdigest(),
        },
    )
    del model, tokenizer, before
    gc.collect()
    mx.clear_cache()


def generate_one(model, tokenizer, case, seed):
    """The model's cleanup of ``case`` (``served_cases``), before the learned
    corrections, with its seconds and peak memory."""
    import mlx.core as mx
    from mlx_lm import generate
    from mlx_lm.sample_utils import make_sampler

    from ...backends.qwen_llm_backend import _build_messages

    start = time.perf_counter()
    if case["bypass"] is None:
        messages = _build_messages(case["text"], case["system"], [tuple(turn) for turn in case["examples"]])
        prompt = tokenizer.apply_chat_template(
            messages, tokenize=False, add_generation_prompt=True, enable_thinking=False
        )
        mx.random.seed(seed)
        output = generate(
            model, tokenizer, prompt=prompt, max_tokens=2048, sampler=make_sampler(temp=0.2, top_p=0.9), verbose=False
        ).strip()
    else:
        output = case["bypass"]
    return output, time.perf_counter() - start, mx.get_peak_memory()


def transcribe_samples(plan, samples, model_size):
    import mlx.core as mx
    from huggingface_hub import snapshot_download
    from mlx_audio.stt import load

    from ...backends.mlx_backend import WHISPER_HF_REPOS

    # Never start a new model download during background improvement.
    path = snapshot_download(WHISPER_HF_REPOS[model_size], local_files_only=True)
    model = load(path)
    result = {}
    for sample in samples:
        if not sample.get("audio"):
            continue
        options = {"language": sample["language"]} if sample.get("language") else {}
        # Verify audio has not changed since the parent froze the dataset.
        with Path(sample["audio"]).open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != sample["audio_hash"]:
                raise ValueError("A recorded-audio evaluation file changed")
        # Warm-up excluded from steady-state latency comparison.
        if not result:
            model.generate(sample["audio"], **options)
        mx.reset_peak_memory()
        start = time.perf_counter()
        output = model.generate(sample["audio"], **options)
        text = (
            output if isinstance(output, str) else output.get("text", "") if isinstance(output, dict) else output.text
        )
        result[sample["id"]] = (text.strip(), time.perf_counter() - start, mx.get_peak_memory())
    del model
    gc.collect()
    mx.clear_cache()
    return result


def evaluate_adapter(plan, directory, candidate_adapter="trained", candidate_stt=None):
    import mlx.core as mx
    from mlx_lm import load

    phase(directory, "evaluating_audio")
    heldout = [s for s in plan["samples"] if s["target"] == "refined" and s["split"] == "test"]
    audio = transcribe_samples(plan, heldout, plan["stt_model"])
    candidate_audio = transcribe_samples(plan, heldout, candidate_stt) if candidate_stt else audio
    # Judged against what the adapter learns to write (:func:`target`).
    heldout = [dict(s, expected=target(s)) for s in heldout]
    cases = [dict(s, kind="heldout") for s in heldout]
    cases += [dict(s, raw=audio[s["id"]][0], kind="audio") for s in heldout if s["id"] in audio]
    # All evaluated flag combinations are recorded; other settings keep the base model.
    # Each writing style's current flags are among them.
    flags = {json.dumps(s.get("flags") or {}, sort_keys=True) for s in heldout}
    flags |= {json.dumps(f, sort_keys=True) for f in plan.get("style_flags", [])}
    flags.add("{}")
    for flag in flags:
        cases += [
            {
                "id": f"control-{i}-{flag}",
                "raw": raw,
                "expected": expected,
                "flags": json.loads(flag),
                "kind": "control",
                "language": None,
            }
            for i, (raw, expected) in enumerate(REGRESSION)
        ]
    rows = []
    cold_load = {}
    if candidate_adapter == "trained":
        candidate_adapter = str(directory / "adapter")
    # Alternate order across seeds so the candidate is not always measured warmest.
    for seed in (17, 29):
        outputs = {}
        order = ("baseline", "candidate") if seed == 17 else ("candidate", "baseline")
        for variant in order:
            phase(directory, f"evaluating_{variant}")
            adapter = plan.get("baseline_adapter") if variant == "baseline" else candidate_adapter
            variant_cases = [
                dict(case, raw=candidate_audio[case["id"]][0])
                if variant == "candidate" and case["kind"] == "audio"
                else case
                for case in cases
            ]
            prompts = served_cases(plan, variant_cases)
            load_started = time.perf_counter()
            model, tokenizer = load(
                plan["model_path"], adapter_path=adapter, tokenizer_config={"trust_remote_code": False}
            )
            generate_one(model, tokenizer, prompts[0], seed)
            cold_load.setdefault(variant, []).append(time.perf_counter() - load_started)
            mx.reset_peak_memory()
            generated = [generate_one(model, tokenizer, prompt, seed) for prompt in prompts]
            fixed = served_fixes(plan, [output for output, _, _ in generated], variant_cases)
            outputs[variant] = [(text, *rest) for text, (_, *rest) in zip(fixed, generated, strict=True)]
            del model, tokenizer
            gc.collect()
            mx.clear_cache()
        for index, case in enumerate(cases):
            before, after = outputs["baseline"][index], outputs["candidate"][index]
            rows.append(
                {
                    "id": case["id"],
                    "kind": case["kind"],
                    "seed": seed,
                    "expected": case["expected"],
                    "baseline": before[0],
                    "candidate": after[0],
                    "baseline_seconds": before[1] + (audio[case["id"]][1] if case["kind"] == "audio" else 0),
                    "candidate_seconds": after[1] + (candidate_audio[case["id"]][1] if case["kind"] == "audio" else 0),
                    "baseline_cold_seconds": max(cold_load["baseline"]),
                    "candidate_cold_seconds": max(cold_load["candidate"]),
                    "baseline_memory": before[2],
                    "candidate_memory": after[2],
                }
            )
    return {"rows": rows, "metrics": score_rows(rows), "tested_flags": [json.loads(f) for f in flags]}


def evaluate_speech(plan, directory):
    phase(directory, "evaluating_speech")
    samples = [s for s in plan["samples"] if s["target"] == "raw" and s["split"] == "test" and s.get("audio")]
    if len(samples) < 5 or not plan.get("speech_candidates"):
        return None
    baseline = transcribe_samples(plan, samples, plan["stt_model"])
    evaluations = []
    for size in plan["speech_candidates"]:
        candidate = transcribe_samples(plan, samples, size)
        rows = [
            {
                "id": s["id"],
                "expected": s["expected"],
                "baseline": baseline[s["id"]][0],
                "candidate": candidate[s["id"]][0],
                "baseline_seconds": baseline[s["id"]][1],
                "candidate_seconds": candidate[s["id"]][1],
                "baseline_memory": baseline[s["id"]][2],
                "candidate_memory": candidate[s["id"]][2],
            }
            for s in samples
        ]
        evaluations.append({"model": size, "rows": rows, "metrics": score_speech(rows)})
    passing = [item for item in evaluations if item["metrics"]["passed"]]
    winner = min(passing, key=lambda item: item["metrics"]["candidate_errors"]) if passing else None
    return {"evaluations": evaluations, "winner": winner["model"] if winner else None}


def run(plan_path):
    # These apply only to the isolated child, never to the app's download state.
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["HF_DATASETS_OFFLINE"] = "1"
    os.environ["TOKENIZERS_PARALLELISM"] = "false"
    import mlx.core as mx

    mx.set_memory_limit(16 * 1024**3)
    plan_path = Path(plan_path)
    directory = plan_path.parent
    plan = json.loads(plan_path.read_text())
    if plan.get("data_dir"):
        from ... import config

        config.set_data_dir(plan["data_dir"])
    result = {"adapter": None, "speech": None, "voice": None}
    if plan.get("voice"):
        from ..voice_training import train as voice_training

        phase(directory, "training")
        training = voice_training.train(plan, directory)
        phase(directory, "evaluating_voice")
        result["voice"] = voice_training.evaluate(plan, directory) | {"training": training}
    if plan["train_ready"]:
        phase(directory, "training")
        train_adapter(plan, directory)
        if plan.get("training_only"):
            return
        result["adapter"] = evaluate_adapter(plan, directory)
    result["speech"] = evaluate_speech(plan, directory)
    if (
        result["speech"]
        and result["speech"]["winner"]
        and plan.get("model_path")
        and sum(s["target"] == "refined" and s["split"] == "test" and bool(s.get("audio")) for s in plan["samples"])
        >= 5
        and not (result["adapter"] and result["adapter"]["metrics"]["passed"])
    ):
        result["speech"]["pipeline"] = evaluate_adapter(
            plan, directory, candidate_adapter=plan.get("baseline_adapter"), candidate_stt=result["speech"]["winner"]
        )
    write_json(directory / "result.json", result)
    phase(directory, "complete")


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan", required=True)
    parser.add_argument("--parent-pid", type=int)
    args = parser.parse_args(argv)
    if args.parent_pid:

        def watch_parent():
            while True:
                try:
                    os.kill(args.parent_pid, 0)
                except ProcessLookupError:
                    os._exit(1)
                time.sleep(2)

        threading.Thread(target=watch_parent, daemon=True).start()
    run(args.plan)


if __name__ == "__main__":
    main()
