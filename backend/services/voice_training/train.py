"""Training and evaluation of the voice adapter, run in the model-improvement
worker process (never in the server).

``train`` continues from the last trained adapter for a fixed time budget,
one example at a time with gradients summed over ``ACCUMULATE`` examples,
which keeps memory near 10 GB (the worker's MLX limit is 16 GB). Each example
is one of the user's training takes, mixed with a random talker or room
(mixing.training_mix); its target is the take's transcript, so the model
learns to give the same text whatever is going on behind the user. Takes the
user corrected are drawn more often (``CORRECTED_SHARE``), so it also learns
their words.

``evaluate`` transcribes the test takes under fixed conditions with plain
turbo, the model in use and the candidate, for gate.score_voice.
"""

import gc
import hashlib
import json
import time
from pathlib import Path

import numpy as np

from . import gate, lora, mixing
from .bank import read_wav

ACCUMULATE = 4
PROMPT_SHARE = 0.3  # examples that carry earlier text as a prompt, as phrases in a dictation do
# Examples drawn from takes the user corrected. There are few of them among
# a thousand takes, and they hold the mistakes training should fix.
CORRECTED_SHARE = 0.2
MAX_TEXT_TOKENS = 200
EVAL_TAKES = 40


def _verified_audio(take: dict) -> np.ndarray:
    path = Path(take["audio"])
    if hashlib.sha256(path.read_bytes()).hexdigest() != take["hash"]:
        raise ValueError("A voice-bank recording changed during training")
    return read_wav(path)


def _beds(voice: dict, sound_beds):
    """The downloaded rooms plus the user's own room noise."""
    return list(sound_beds) + [read_wav(Path(path)) for path in voice.get("room", []) if Path(path).is_file()]


def _language(voice):
    return voice.get("language") or "en"


def train(plan: dict, directory: Path) -> dict:
    import mlx.core as mx
    import mlx.nn as nn
    import mlx.optimizers as optim
    from mlx.utils import tree_flatten, tree_map
    from mlx_audio.stt.models.whisper.audio import N_FRAMES, N_SAMPLES, log_mel_spectrogram

    from ...backends import mlx_whisper_loader
    from . import sounds

    voice = plan["voice"]
    rng = np.random.default_rng(int(time.time()))
    talk, sound_beds = sounds.load()
    beds = _beds(voice, sound_beds)
    examples = [take for take in voice["takes"] if take["split"] == "train"]
    audio = {take["id"]: _verified_audio(take) for take in examples}

    model = mlx_whisper_loader.load_whisper(voice["repo"])
    tokenizer = model.get_tokenizer(language=_language(voice))
    n_mels = model.dims.n_mels
    if voice.get("resume"):
        lora.load_into(model, Path(voice["resume"]))
    else:
        lora.add(model)
    before = {name: mx.array(value) for name, value in tree_flatten(model.trainable_parameters())}
    mx.eval(before)

    def target(take):
        text = tokenizer.encode(" " + take["text"].strip())
        if len(text) > MAX_TEXT_TOKENS:
            return None
        end = tokenizer.timestamp_begin + round(min(take["end"], 29.98) / 0.02)
        return [*tokenizer.sot_sequence, tokenizer.timestamp_begin, *text, end, tokenizer.eot]

    targets = {take["id"]: target(take) for take in examples}
    examples = [take for take in examples if targets[take["id"]]]
    if len(examples) < 20:
        raise ValueError("Not enough voice-bank takes to train on")

    corrected = [take for take in examples if take.get("corrected")]

    def example():
        pool = corrected if corrected and rng.random() < CORRECTED_SHARE else examples
        take = pool[rng.integers(len(pool))]
        mixed = mixing.training_mix(rng, audio[take["id"]], talk, beds)
        mel = log_mel_spectrogram(mx.array(mixed), n_mels=n_mels, padding=N_SAMPLES)[:N_FRAMES]
        prompt = []
        if rng.random() < PROMPT_SHARE:
            other = examples[rng.integers(len(examples))]["text"]
            prompt = [tokenizer.sot_prev, *tokenizer.encode(" " + other.strip())[-60:]]
        sequence = prompt + targets[take["id"]]
        inputs = mx.array([sequence[:-1]])
        labels = mx.array([sequence[1:]])
        # The loss covers the transcript only, not the prompt.
        mask = mx.array([[0.0] * len(prompt) + [1.0] * (len(sequence) - len(prompt) - 1)])
        return mel[None].astype(mx.float16), inputs, labels, mask

    def loss_fn(model, mel, inputs, labels, mask):
        logits = model(mel, inputs).astype(mx.float32)
        losses = nn.losses.cross_entropy(logits, labels, reduction="none")
        return (losses * mask).sum() / mask.sum()

    step = nn.value_and_grad(model, loss_fn)
    learning_rate = 5e-5 if voice.get("resume") else 1e-4
    optimizer = optim.AdamW(
        learning_rate=lambda update: learning_rate * mx.minimum(1.0, (update + 1) / 20), weight_decay=0.0
    )
    started, updates, losses = time.monotonic(), 0, []
    while updates < voice.get("max_updates", 250) and time.monotonic() - started < voice.get("train_seconds", 900):
        total, summed = 0.0, None
        for _ in range(ACCUMULATE):
            value, grads = step(model, *example())
            summed = grads if summed is None else tree_map(lambda a, b: a + b, summed, grads)
            total += value.item()
        optimizer.update(model, tree_map(lambda g: g / ACCUMULATE, summed))
        mx.eval(model.trainable_parameters(), optimizer.state)
        updates += 1
        losses.append(total / ACCUMULATE)
    changed = any(
        bool(mx.any(value != before[name]).item()) for name, value in tree_flatten(model.trainable_parameters())
    )
    if not changed:
        raise RuntimeError("Training did not change the voice adapter")
    destination = directory / "voice"
    lora.save(
        model,
        destination,
        {
            "model_size": "turbo",
            "repo": voice["repo"],
            "resumed_from": voice.get("resume"),
            "shared": voice.get("shared"),
            "updates": updates,
        },
    )
    report = {
        "updates": updates,
        "seconds": round(time.monotonic() - started),
        "train_takes": len(examples),
        "corrected_takes": len(corrected),
        "loss_first": round(float(np.mean(losses[:10])), 4),
        "loss_last": round(float(np.mean(losses[-10:])), 4),
    }
    (directory / "voice_training.json").write_text(json.dumps(report, indent=2))
    del model, before, optimizer
    gc.collect()
    mx.clear_cache()
    return report


def _transcriber(voice: dict, adapter: str | None):
    from ...backends import mlx_whisper_loader

    model = mlx_whisper_loader.load_whisper(voice["repo"])
    if adapter:
        lora.apply(model, Path(adapter))
    return model


def _text(output) -> str:
    if isinstance(output, str):
        return output.strip()
    if isinstance(output, dict):
        return output.get("text", "").strip()
    return output.text.strip()


def evaluate(plan: dict, directory: Path) -> dict:
    import mlx.core as mx

    from ...backends import whisper_audio
    from . import sounds

    voice = plan["voice"]
    talk, sound_beds = sounds.load()
    beds = _beds(voice, sound_beds)
    tests = [take for take in voice["takes"] if take["split"] == "test"]
    tests = sorted(tests, key=lambda take: take["created"])[-EVAL_TAKES:]
    cases = []
    for take in tests:
        clean = _verified_audio(take)
        for condition in mixing.CONDITIONS:
            cases.append(
                (take["id"], condition, take["text"], mixing.condition_mix(take["id"], condition, clean, talk, beds))
            )
    for sample in voice.get("corrections", []):
        with Path(sample["audio"]).open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != sample["audio_hash"]:
                raise ValueError("A corrected recording changed during evaluation")
        y = np.array(whisper_audio.read_audio_file(sample["audio"]), dtype=np.float32)
        cases.append((sample["id"], "correction", sample["expected"], y))
    options = {"language": voice["language"]} if voice.get("language") else {}
    variants = {"base": None, "candidate": str(directory / "voice")}
    if voice.get("production"):
        variants["production"] = voice["production"]
    results = {}
    for name, adapter in variants.items():
        model = _transcriber(voice, adapter)
        model.generate(mx.array(cases[0][3]), **options)  # warm-up, not timed
        outputs = []
        for _, _, _, y in cases:
            mx.reset_peak_memory()
            started = time.perf_counter()
            text = _text(model.generate(mx.array(y), **options))
            outputs.append((text, time.perf_counter() - started, mx.get_peak_memory()))
        results[name] = outputs
        del model
        gc.collect()
        mx.clear_cache()
    rows = []
    for index, (take_id, condition, expected, _) in enumerate(cases):
        row = {"id": take_id, "condition": condition, "expected": expected}
        for name, outputs in results.items():
            row[name], row[f"{name}_seconds"], row[f"{name}_memory"] = outputs[index]
        rows.append(row)
    return {"rows": rows, "metrics": gate.score_voice(rows)}
