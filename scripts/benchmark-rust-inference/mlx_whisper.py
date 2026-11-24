"""Current Kass Whisper path, in memory: MLXSTTBackend.transcribe_array (mlx-audio Whisper turbo).

usage: mlx_whisper.py <repo_root> <scratch_data_dir> <runs> <wav16k>...   -> JSON lines on stdout
Same output shape as wrbench, so summarize.py reads both.
"""

import asyncio
import json
import os
import sys
import time
import wave
from pathlib import Path

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
repo, data_dir, runs, wavs = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4:]
sys.path.insert(0, repo)
sys.path.insert(0, str(Path(__file__).parent))
from footprint import footprint_mb, resident_mb  # noqa: E402

t_import = time.perf_counter()
from backend import config  # noqa: E402

config.set_data_dir(data_dir)
import mlx.core as mx  # noqa: E402
import numpy as np  # noqa: E402

from backend.services.transcribe import get_whisper_model  # noqa: E402

import_s = time.perf_counter() - t_import


def emit(d):
    print(json.dumps(d, sort_keys=True), flush=True)


def read(wav):
    with wave.open(wav) as w:
        assert w.getframerate() == 16000
        assert w.getnchannels() == 1
        return np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)


async def main():
    model = get_whisper_model()
    t = time.perf_counter()
    await model.load_model_async("turbo")
    fp, peak = footprint_mb()
    emit(
        {
            "event": "load",
            "engine": "mlx",
            "variant": "mlx-turbo",
            "import_s": import_s,
            "load_s": time.perf_counter() - t,
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(),
            "mlx_active_mb": mx.get_active_memory() / 1048576,
        }
    )
    for wav in wavs:
        samples = read(wav)
        for run in range(runs):
            s = time.perf_counter()
            text = await model.transcribe_array(samples, 16000, language="en", model_size="turbo")
            emit(
                {
                    "event": "transcribe",
                    "engine": "mlx",
                    "variant": "mlx-turbo",
                    "fixture": Path(wav).stem,
                    "run": run,
                    "wall_s": time.perf_counter() - s,
                    "text": text,
                }
            )
    fp, peak = footprint_mb()
    emit(
        {
            "event": "done",
            "variant": "mlx-turbo",
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(),
            "mlx_peak_mb": mx.get_peak_memory() / 1048576,
        }
    )


asyncio.run(main())
