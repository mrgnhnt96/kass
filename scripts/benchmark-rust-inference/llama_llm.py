"""llama.cpp (Metal) cleanup benchmark through llama-server, one slot, prompt cache on.

usage: llama_llm.py <llama-server> <model.gguf> <prompts.json> <passes> [extra server args...]
    -> JSON lines on stdout
Feeds the same chat-templated prompts as mlx_llm.py, in the same order, temperature 0.
llama-server keeps the slot's KV cache and reuses the longest shared prefix
(cache_prompt), which is what an in-process llama-cpp-2 integration would do by
keeping the context and trimming its KV cache. Reports both the client wall time and
llama.cpp's own prompt/predict timings (the in-process cost without HTTP).
"""

import json
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from footprint import footprint_mb, resident_mb

server, model, prompts, passes, extra = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5:]


def emit(d):
    print(json.dumps(d, sort_keys=True), flush=True)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def post(url, body):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.loads(r.read())


port = free_port()
base = f"http://127.0.0.1:{port}"
variant = Path(model).stem + ("+" + "".join(a.lstrip("-") for a in extra) if extra else "")
t = time.perf_counter()
proc = subprocess.Popen(
    [server, "-m", model, "--port", str(port), "-ngl", "99", "-c", "4096", "-np", "1", "--no-webui", *extra],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
try:
    while True:
        try:
            with urllib.request.urlopen(base + "/health", timeout=1) as r:
                if r.status == 200:
                    break
        except Exception:
            time.sleep(0.02)
        if proc.poll() is not None:
            sys.exit("llama-server exited")
    fp, peak = footprint_mb(proc.pid)
    emit(
        {
            "event": "load",
            "engine": "llama.cpp",
            "variant": variant,
            "load_s": time.perf_counter() - t,
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(proc.pid),
        }
    )
    entries = json.loads(Path(prompts).read_text())["entries"]
    call = 0
    for p in range(passes):
        for i, e in enumerate(entries):
            s = time.perf_counter()
            r = post(
                base + "/completion",
                {
                    "prompt": e["chat"],
                    "n_predict": 256,
                    "temperature": 0.0,
                    "cache_prompt": True,
                    "return_tokens": False,
                },
            )
            wall = time.perf_counter() - s
            tm = r.get("timings", {})
            emit(
                {
                    "event": "generate",
                    "engine": "llama.cpp",
                    "variant": variant,
                    "pass": p,
                    "call": call,
                    "entry": i,
                    "wall_s": wall,
                    "prompt_tokens": r.get("tokens_evaluated"),
                    "reused_tokens": tm.get("cache_n"),
                    "prompt_n": tm.get("prompt_n"),
                    "prompt_ms": tm.get("prompt_ms"),
                    "generated": tm.get("predicted_n"),
                    "predicted_ms": tm.get("predicted_ms"),
                    "first_token_s": (tm.get("prompt_ms", 0) + tm.get("predicted_per_token_ms", 0)) / 1000,
                    "text": r["content"].strip(),
                }
            )
            call += 1
    fp, peak = footprint_mb(proc.pid)
    emit(
        {
            "event": "done",
            "variant": variant,
            "footprint_mb": fp,
            "peak_footprint_mb": peak,
            "resident_mb": resident_mb(proc.pid),
        }
    )
finally:
    proc.terminate()
    proc.wait()
