"""Current Kass cleanup path: MLXQwenLLMBackend.generate (mlx-lm, Qwen3 4B 4-bit) with its KV-cache reuse.

usage: mlx_llm.py <repo_root> <scratch_data_dir> <prompts.json> <passes>   -> JSON lines on stdout
The first call prefills the whole prompt (cold); every later call reuses the cached
shared prefix and only reads the new transcript, as production does. Temperature 0.
"""

import asyncio
import json
import os
import sys
import time
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
repo, data_dir, prompts, passes = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
sys.path.insert(0, repo)
sys.path.insert(0, str(Path(__file__).parent))
from footprint import footprint_mb, resident_mb  # noqa: E402

from backend import config  # noqa: E402

config.set_data_dir(data_dir)
import mlx.core as mx  # noqa: E402

from backend.backends.qwen_llm_backend import MLXQwenLLMBackend  # noqa: E402
from backend.services.mlx_thread import run_on_mlx_thread  # noqa: E402

# The backend imports stream_generate at call time; wrap it to keep mlx-lm's own
# prompt/generation timings for the last call (prefill of the uncached suffix, tokens/s).
# mlx-lm must be imported on the MLX worker thread (its generation stream is per thread).
_last = {}


def _recording_stream_generate(*a, **kw):
    started = time.perf_counter()
    for i, r in enumerate(_recording_stream_generate.inner(*a, **kw)):
        if i == 0:
            _last["first_token_s"] = time.perf_counter() - started
        _last.update(
            prompt_tps=r.prompt_tps,
            prompt_tokens=r.prompt_tokens,
            generation_tps=r.generation_tps,
            generation_tokens=r.generation_tokens,
        )
        yield r


def _install():
    import mlx_lm

    _recording_stream_generate.inner = mlx_lm.stream_generate
    mlx_lm.stream_generate = _recording_stream_generate


def emit(d):
    print(json.dumps(d, sort_keys=True), flush=True)


async def main():
    entries = json.loads(Path(prompts).read_text())["entries"]
    b = MLXQwenLLMBackend("4B")
    t = time.perf_counter()
    await b.load_model("4B")
    await run_on_mlx_thread(_install)
    fp, peak = footprint_mb()
    emit(
        {
            "event": "load",
            "engine": "mlx",
            "variant": "mlx-4bit",
            "load_s": time.perf_counter() - t,
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(),
        }
    )
    call = 0
    for p in range(passes):
        for i, e in enumerate(entries):
            before = list(b._cached_tokens)
            s = time.perf_counter()
            text = await b.generate(
                e["prompt"],
                system=e["system"],
                max_tokens=256,
                temperature=0.0,
                model_size="4B",
                examples=[tuple(x) for x in e["examples"]],
            )
            wall = time.perf_counter() - s
            n_prompt = e["n_tokens"]
            generated = len(b._cached_tokens) - n_prompt
            shared = 0
            for a, c in zip(before, b._cached_tokens, strict=False):
                if a != c:
                    break
                shared += 1
            emit(
                {
                    "event": "generate",
                    "engine": "mlx",
                    "variant": "mlx-4bit",
                    "pass": p,
                    "call": call,
                    "entry": i,
                    "wall_s": wall,
                    "prompt_tokens": n_prompt,
                    "reused_tokens": min(shared, n_prompt - 1),
                    "generated": generated,
                    "prefill_s": _last["prompt_tokens"] / _last["prompt_tps"],
                    "first_token_s": _last["first_token_s"],
                    "generation_tps": _last["generation_tps"],
                    "text": text,
                }
            )
            call += 1
    fp, peak = footprint_mb()
    emit(
        {
            "event": "done",
            "variant": "mlx-4bit",
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(),
            "mlx_peak_mb": mx.get_peak_memory() / 1048576,
        }
    )


asyncio.run(main())
