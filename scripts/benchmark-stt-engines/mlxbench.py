"""Current Kass path: MLXSTTBackend (mlx-audio Whisper turbo). Load once, transcribe each fixture N times.
usage: mlxbench.py <repo_root> <data_dir> <runs> <wav>...   -> JSON lines on stdout
"""

import asyncio
import json
import os
import resource
import sys
import time
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
repo, data_dir, runs, wavs = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4:]
sys.path.insert(0, repo)

t_import = time.perf_counter()
from backend import config  # noqa: E402

config.set_data_dir(data_dir)
import mlx.core as mx  # noqa: E402

from backend.services.transcribe import get_whisper_model  # noqa: E402

import_s = time.perf_counter() - t_import


def emit(d):
    print(json.dumps(d, sort_keys=True), flush=True)


def rss_mb():
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1048576  # bytes on macOS


async def main():
    model = get_whisper_model()
    t = time.perf_counter()
    await model.load_model_async("turbo")
    emit(
        {
            "event": "load",
            "engine": "mlx",
            "import_s": import_s,
            "load_s": time.perf_counter() - t,
            "mlx_active_mb": mx.get_active_memory() / 1048576,
            "max_rss_mb": rss_mb(),
        }
    )
    for wav in wavs:
        for run in range(runs):
            s = time.perf_counter()
            text = await model.transcribe(wav, language="en", model_size="turbo")
            emit(
                {
                    "event": "transcribe",
                    "engine": "mlx",
                    "fixture": Path(wav).stem,
                    "run": run,
                    "wall_s": time.perf_counter() - s,
                    "text": text,
                }
            )
    emit({"event": "done", "mlx_peak_mb": mx.get_peak_memory() / 1048576, "max_rss_mb": rss_mb()})


asyncio.run(main())
