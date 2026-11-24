"""Summarize mlx_llm.py / llama_llm.py / llbench output.

usage: summarize_llm.py <llm.jsonl>...
TTFT = time to first token. Cold = the first call of a process (whole prompt prefilled). Warm = calls whose shared
prefix was reused. Agreement compares each engine's text per prompt with MLX's.
"""

import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path

rows = [json.loads(line) for p in sys.argv[1:] for line in Path(p).read_text().splitlines() if line.startswith("{")]
loads, mem, cold, warm, out, per_tok, prefill = (defaultdict(list) for _ in range(7))
for r in rows:
    v = r.get("variant")
    if r["event"] == "load":
        loads[v].append(r["load_s"])
    elif r["event"] == "done":
        mem[v].append((r["footprint_mb"], r["peak_footprint_mb"], r.get("resident_mb", 0)))
    elif r["event"] == "generate":
        (cold if r["call"] == 0 else warm)[v].append(r["wall_s"])
        out[v].append((r["entry"], r["text"]))
        if r["call"] > 0 and r.get("first_token_s"):
            prefill[v].append(r["first_token_s"])
            if r["generated"] > 2:
                per_tok[v].append((r["wall_s"] - r["first_token_s"]) * 1000 / (r["generated"] - 1))

vs = list(loads)


def tp(v, n):
    return statistics.median(prefill[v]) + (n - 1) * statistics.median(per_tok[v]) / 1000


def q(xs, f):
    return f"{f(xs):.3f}" if xs else "-"


print("| | " + " | ".join(vs) + " |")
print("|---|" + "---:|" * len(vs))
print("| load s (median) | " + " | ".join(f"{statistics.median(loads[v]):.2f}" for v in vs) + " |")
print("| cold call s (median) | " + " | ".join(q(cold[v], statistics.median) for v in vs) + " |")
print("| warm call s, median | " + " | ".join(q(warm[v], statistics.median) for v in vs) + " |")
print("| warm call s, p90 | " + " | ".join(q(sorted(warm[v]), lambda x: x[int(len(x) * 0.9)]) for v in vs) + " |")
print(
    "| warm time to first token s (read ~20 new tokens + 1 step), median | "
    + " | ".join(q(prefill[v], statistics.median) for v in vs)
    + " |"
)
print(
    "| ms per further token (wall - TTFT)/(n-1), median | "
    + " | ".join(q(per_tok[v], statistics.median) for v in vs)
    + " |"
)
for n in (10, 20, 30):
    print(
        f"| model: {n} output tokens, s | "
        + " | ".join(f"{tp(v, n):.3f}" if prefill[v] and per_tok[v] else "-" for v in vs)
        + " |"
    )
print(
    "| footprint MB end / peak / resident at end | "
    + " | ".join(
        f"{max(m[0] for m in mem[v]):.0f} / {max(m[1] for m in mem[v]):.0f} / {max(m[2] for m in mem[v]):.0f}"
        for v in vs
    )
    + " |"
)

base = next(v for v in vs if v.startswith("mlx"))
ref = {}
for e, t in out[base]:
    ref.setdefault(e, t)
for v in vs:
    texts = {}
    for e, t in out[v]:
        texts.setdefault(e, set()).add(t)
    same = sum(ref[e] in texts[e] for e in ref)
    unstable = sum(len(s) > 1 for s in texts.values())
    print(
        f"- {v}: identical to MLX on {same}/{len(ref)} prompts; prompts with pass-to-pass differing output: {unstable}"
    )
    for e in sorted(ref):
        if ref[e] not in texts[e]:
            print(f"    [{e}] {sorted(texts[e])[0]!r}\n    {' ' * len(str(e))}  mlx: {ref[e]!r}")
