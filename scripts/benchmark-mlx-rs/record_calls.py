"""Start the Python server with every Whisper and Qwen call logged, for replay.

usage (from the repo root, as parity.py's --server-cmd):
  RECORD_DIR=/scratch/calls {python} scripts/benchmark-mlx-rs/record_calls.py --port {port} --data-dir {data_dir}

Writes RECORD_DIR/calls.jsonl, one line per call in the order the MLX thread
received them, and RECORD_DIR/audio/<n>.npy for each transcribe_array's samples.
"""

import json
import os
import runpy
import sys
import threading
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, os.getcwd())
OUT = Path(os.environ["RECORD_DIR"])
(OUT / "audio").mkdir(parents=True, exist_ok=True)
_lock = threading.Lock()
_seq = [0]


def _write(entry: dict) -> None:
    with _lock, open(OUT / "calls.jsonl", "a") as file:
        file.write(json.dumps(entry) + "\n")


def _next() -> int:
    with _lock:
        _seq[0] += 1
        return _seq[0]


from backend.backends import mlx_backend, qwen_llm_backend  # noqa: E402

_generate = qwen_llm_backend.MLXQwenLLMBackend.generate


async def generate(self, prompt, system=None, max_tokens=512, temperature=0.7, model_size=None, examples=None, adapter_path=None):
    n = _next()
    entry = {
        "n": n,
        "kind": "generate",
        "prompt": prompt,
        "system": system,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "model_size": model_size or self.model_size,
        "examples": examples,
        "adapter_path": adapter_path,
        "hint": qwen_llm_backend.generation_hint.get(),
        "cache_key": qwen_llm_backend.prompt_cache_key.get(),
        "stop_set": qwen_llm_backend.generation_stop.get() is not None,
    }
    started = time.monotonic()
    try:
        text = await _generate(self, prompt, system, max_tokens, temperature, model_size, examples, adapter_path)
    except BaseException as error:
        entry["error"] = repr(error)
        _write(entry)
        raise
    entry["seconds"] = time.monotonic() - started
    entry["output"] = text
    _write(entry)
    return text


qwen_llm_backend.MLXQwenLLMBackend.generate = generate

_transcribe_array = mlx_backend.MLXSTTBackend.transcribe_array
_transcribe = mlx_backend.MLXSTTBackend.transcribe


async def transcribe_array(self, samples, sample_rate, language=None, model_size=None, previous_text=None,
                           check_speech=True, vocabulary=(), alignments=None):
    n = _next()
    np.save(OUT / "audio" / f"{n}.npy", np.asarray(samples))
    entry = {
        "n": n, "kind": "transcribe_array", "sample_rate": sample_rate, "language": language,
        "model_size": model_size or self.model_size, "previous_text": previous_text, "check_speech": check_speech,
        "vocabulary": list(vocabulary), "alignments": alignments is not None,
    }
    started = time.monotonic()
    text = await _transcribe_array(self, samples, sample_rate, language, model_size, previous_text, check_speech,
                                   vocabulary, alignments)
    entry["seconds"] = time.monotonic() - started
    entry["output"] = text
    _write(entry)
    return text


async def transcribe(self, audio_path, language=None, model_size=None, previous_text=None, check_speech=True,
                     vocabulary=()):
    n = _next()
    entry = {
        "n": n, "kind": "transcribe", "audio_path": str(audio_path), "language": language,
        "model_size": model_size or self.model_size, "previous_text": previous_text, "check_speech": check_speech,
        "vocabulary": list(vocabulary),
    }
    try:
        from backend.backends import whisper_audio

        np.save(OUT / "audio" / f"{n}.npy", np.asarray(whisper_audio.read_audio_file(str(audio_path))))
        entry["sample_rate"] = whisper_audio.SAMPLE_RATE
    except Exception as error:  # the file may be gone later; keep going
        entry["audio_error"] = repr(error)
    started = time.monotonic()
    text = await _transcribe(self, audio_path, language, model_size, previous_text, check_speech, vocabulary)
    entry["seconds"] = time.monotonic() - started
    entry["output"] = text
    _write(entry)
    return text


mlx_backend.MLXSTTBackend.transcribe_array = transcribe_array
mlx_backend.MLXSTTBackend.transcribe = transcribe

sys.argv = ["backend.main", *sys.argv[1:]]
runpy.run_module("backend.main", run_name="__main__", alter_sys=True)
