"""Replay recorded model calls on the Python backends, as the server makes them.

usage (repo root, backend venv):
  python scripts/benchmark-mlx-rs/replay_py.py qwen <calls dir> <out.jsonl> <model size|keep> <passes>
  python scripts/benchmark-mlx-rs/replay_py.py whisper <calls dir> <out.jsonl> <passes>

Cleanup runs greedy (temperature 0) so the output can be compared token for
token with the Rust replay.
"""

import asyncio
import json
import os
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, os.getcwd())


def load_calls(directory: str, kinds: tuple[str, ...]) -> list[dict]:
    lines = (Path(directory) / "calls.jsonl").read_text().splitlines()
    return [c for c in map(json.loads, lines) if c["kind"] in kinds]


async def qwen(directory: str, out: str, size: str, passes: int) -> None:
    from backend.backends import qwen_llm_backend as q

    calls = load_calls(directory, ("generate",))
    size = calls[0]["model_size"] if size == "keep" else size
    backend = q.MLXQwenLLMBackend(size)
    started = time.monotonic()
    await backend.load_model(size)
    print(f"loaded Qwen3 {size} in {time.monotonic() - started:.2f}s", file=sys.stderr)
    with open(out, "w") as file:
        for pass_ in range(passes):
            for c in calls:
                hint = q.generation_hint.set(c["hint"])
                key = q.prompt_cache_key.set(c["cache_key"])
                examples = [tuple(e) for e in c["examples"]] if c["examples"] else None
                t = time.monotonic()
                text = await backend.generate(c["prompt"], c["system"], c["max_tokens"], 0.0, size, examples, None)
                seconds = time.monotonic() - t
                q.generation_hint.reset(hint)
                q.prompt_cache_key.reset(key)
                ids = None
                if pass_ == 0:
                    messages = q._build_messages(c["prompt"], c["system"], examples)
                    chat = backend.tokenizer.apply_chat_template(
                        messages, tokenize=False, add_generation_prompt=True, enable_thinking=False
                    )
                    ids = backend.tokenizer.encode(chat, add_special_tokens=False)
                file.write(json.dumps({"n": c["n"], "pass": pass_, "seconds": seconds, "output": text, "prompt_ids": ids}) + "\n")


async def whisper(directory: str, out: str, passes: int) -> None:
    """Times each transcribe_array and each model.generate inside it.

    Pass 0 also writes <out>.decodes/: every generate's 16 kHz input and
    decoding spec (prompt, suppressed tokens), for mlxbench whisper.
    """
    import mlx.core as mx
    from mlx_audio.dsp import hanning, mel_filters
    from mlx_audio.stt.models.whisper.decoding import get_suppress_tokens

    from backend.backends.mlx_backend import MLXSTTBackend

    calls = load_calls(directory, ("transcribe_array", "transcribe"))
    backend = MLXSTTBackend(calls[0]["model_size"])
    started = time.monotonic()
    await backend.load_model(calls[0]["model_size"])
    print(f"loaded Whisper in {time.monotonic() - started:.2f}s", file=sys.stderr)
    model = backend.model
    dims = model.dims
    dump = Path(out + ".decodes")
    dump.mkdir(exist_ok=True)
    mx.save_safetensors(
        str(dump / "constants.safetensors"),
        {"window": hanning(400), "filters": mel_filters(16000, 400, dims.n_mels, norm="slaney", mel_scale=None)},
    )
    real_generate = model.generate
    state = {"pass": 0, "n": 0, "decodes": []}

    def generate(audio, **options):
        t = time.monotonic()
        result = real_generate(audio, **options)
        seconds = time.monotonic() - t
        k = len(state["decodes"])
        state["decodes"].append({"seconds": seconds, "text": result.text})
        if state["pass"] == 0:
            language = options.get("language") or "en"
            tokenizer = model.get_tokenizer(language=language)
            name = f"{state['n']}-{k}"
            mx.save_safetensors(str(dump / f"{name}.safetensors"), {"audio": mx.array(audio).astype(mx.float32)})
            prompt = options.get("initial_prompt")
            spec = {
                "name": name,
                "n": state["n"],
                "prompt": tokenizer.encode(" " + prompt.strip()) if prompt is not None else [],
                "sot_sequence": list(tokenizer.sot_sequence),
                "suppress": list(get_suppress_tokens(tokenizer, options.get("suppress_tokens", "-1"))),
                "blank": tokenizer.encode(" ") + [tokenizer.eot],
                "sot_prev": tokenizer.sot_prev,
                "eot": tokenizer.eot,
                "no_speech": tokenizer.no_speech,
                "no_timestamps": tokenizer.no_timestamps,
                "timestamp_begin": tokenizer.timestamp_begin,
                "condition_on_previous_text": options.get("condition_on_previous_text", True),
                "detect": options.get("language") is None,
                "language_tokens": list(tokenizer.all_language_tokens),
                "language_token": model.get_tokenizer(language=result.language).sot_sequence[1],
                "text": result.text,
                "tokens": [t for s in result.segments for t in s["tokens"]],
                "seconds": seconds,
            }
            with open(dump / "decodes.jsonl", "a") as file:
                file.write(json.dumps(spec) + "\n")
        return result

    model.generate = generate
    (dump / "decodes.jsonl").unlink(missing_ok=True)
    with open(out, "w") as file:
        for pass_ in range(passes):
            state["pass"] = pass_
            for c in calls:
                if "sample_rate" not in c:
                    continue
                samples = np.load(Path(directory) / "audio" / f"{c['n']}.npy")
                state["n"] = c["n"]
                state["decodes"] = []
                t = time.monotonic()
                text = await backend.transcribe_array(
                    samples, c["sample_rate"], c["language"], c["model_size"], c["previous_text"],
                    c["check_speech"], tuple(c["vocabulary"]),
                )
                seconds = time.monotonic() - t
                file.write(json.dumps({"n": c["n"], "pass": pass_, "seconds": seconds, "output": text,
                                       "decodes": state["decodes"]}) + "\n")


if __name__ == "__main__":
    kind, rest = sys.argv[1], sys.argv[2:]
    if kind == "qwen":
        asyncio.run(qwen(rest[0], rest[1], rest[2], int(rest[3])))
    else:
        asyncio.run(whisper(rest[0], rest[1], int(rest[2])))
