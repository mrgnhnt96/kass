"""Summarize bench JSONL files: load, median/min/max warm wall per fixture (run 0 excluded), WER vs reference."""

import json
import os
import re
import statistics
import sys
from pathlib import Path

FX = Path(os.environ.get("FX_DIR", "fx"))


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
    return d[len(h)] / max(1, len(r))


for path in sys.argv[1:]:
    rows = [json.loads(line) for line in Path(path).read_text().splitlines() if line.startswith("{")]
    print(f"== {Path(path).name}")
    for r in rows:
        if r["event"] in ("load", "done"):
            print("  ", {k: (round(v, 2) if isinstance(v, float) else v) for k, v in r.items()})
    by = {}
    for r in rows:
        if r["event"] == "transcribe":
            by.setdefault(r["fixture"], []).append(r)
    for fx, rs in by.items():
        warm = [x["wall_s"] for x in rs[1:]] or [rs[0]["wall_s"]]
        ref = (FX / f"{fx}.txt").read_text()
        texts = {x["text"] for x in rs}
        print(
            f"   {fx:9s} first={rs[0]['wall_s']:.3f} warm med={statistics.median(warm):.3f} "
            f"min={min(warm):.3f} max={max(warm):.3f}  WER={wer(ref, rs[-1]['text']):.3f}  distinct_texts={len(texts)}"
        )
        if wer(ref, rs[-1]["text"]) > 0:
            print("      hyp:", rs[-1]["text"])
