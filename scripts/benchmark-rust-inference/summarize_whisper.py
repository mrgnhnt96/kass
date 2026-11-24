"""Summarize run_whisper_rounds.sh output.

usage: summarize_whisper.py <whisper.jsonl> <fx_dir>
Warm = every transcription except the first one of each process (that one is reported
as "first"). Agreement is measured against MLX's text for the same fixture, and against
the script for synthetic fixtures (<name>.txt).
"""

import json
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

rows = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines() if line.startswith("{")]
fx = Path(sys.argv[2])


def words(s):
    return re.findall(r"[a-z0-9']+", s.lower())


def wer(ref, hyp):
    r, h = words(ref), words(hyp)
    d = list(range(len(h) + 1))
    for i in range(1, len(r) + 1):
        prev, d[0] = d[0], i
        for j in range(1, len(h) + 1):
            cur = min(d[j] + 1, d[j - 1] + 1, prev + (r[i - 1] != h[j - 1]))
            prev, d[j] = d[j], cur
    return d[len(h)], len(r)


loads, first, warm, texts, mem = (
    defaultdict(list),
    defaultdict(list),
    defaultdict(lambda: defaultdict(list)),
    {},
    defaultdict(list),
)
seen_first = set()
for r in rows:
    v = r.get("variant")
    if r["event"] == "load":
        loads[v].append(r["load_s"])
    elif r["event"] == "done":
        mem[v].append((r["footprint_mb"], r["peak_footprint_mb"], r.get("resident_mb", 0)))
    elif r["event"] == "transcribe":
        key = (v, r["round"])
        if key not in seen_first:
            seen_first.add(key)
            first[v].append(r["wall_s"])
        else:
            warm[v][r["fixture"]].append(r["wall_s"])
        texts.setdefault((v, r["fixture"]), set()).add(r["text"])

variants = list(loads)
fixtures = sorted({f for v in warm for f in warm[v]})
print("| fixture | " + " | ".join(variants) + " |")
print("|---|" + "---:|" * len(variants))
for f in fixtures:
    cells = []
    for v in variants:
        xs = warm[v].get(f, [])
        cells.append(f"{statistics.median(xs):.3f} ({min(xs):.2f})" if xs else "-")
    print(f"| {f} | " + " | ".join(cells) + " |")
allw = {v: [x for f in warm[v] for x in warm[v][f]] for v in variants}
print(
    "| **all warm, median (min)** | "
    + " | ".join(f"**{statistics.median(allw[v]):.3f}** ({min(allw[v]):.2f})" for v in variants)
    + " |"
)
print("| first call after load, median | " + " | ".join(f"{statistics.median(first[v]):.3f}" for v in variants) + " |")
print(
    "| load s, median (all rounds) | "
    + " | ".join(f"{statistics.median(loads[v]):.2f} ({', '.join(f'{x:.1f}' for x in loads[v])})" for v in variants)
    + " |"
)
print(
    "| footprint MB at end / peak / resident at end | "
    + " | ".join(
        f"{max(m[0] for m in mem[v]):.0f} / {max(m[1] for m in mem[v]):.0f} / {max(m[2] for m in mem[v]):.0f}"
        for v in variants
    )
    + " |"
)

print("\nAgreement with MLX (word errors / words, over all fixtures), and vs script for synthetic fixtures:")
base = "mlx-turbo"
for v in variants:
    e = n = exact = 0
    se = sn = 0
    for f in fixtures:
        ref = sorted(texts[(base, f)])[0]
        hyp = sorted(texts[(v, f)])[0]
        a, b = wer(ref, hyp)
        e, n, exact = e + a, n + b, exact + (words(ref) == words(hyp))
        script = fx / f"{f}.txt"
        if script.exists():
            a, b = wer(script.read_text(), hyp)
            se, sn = se + a, sn + b
    distinct = sum(len(texts[(v, f)]) > 1 for f in fixtures)
    print(
        f"- {v}: vs MLX {e}/{n} = {e / n:.3f}, identical words on {exact}/{len(fixtures)} fixtures; "
        f"vs script {se}/{sn} = {se / max(1, sn):.3f}; fixtures with run-to-run differing text: {distinct}"
    )
for f in fixtures:
    ref = sorted(texts[(base, f)])[0]
    for v in variants:
        hyp = sorted(texts[(v, f)])[0]
        if words(hyp) != words(ref):
            print(f"  {f} {v}: {hyp!r}\n  {' ' * len(f)} mlx: {ref!r}")
