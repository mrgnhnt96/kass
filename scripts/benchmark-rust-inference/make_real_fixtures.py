"""Pick the user's real dictation captures closest to 2, 5, 9 and 14 s and convert them to 16 kHz mono PCM16.

usage: make_real_fixtures.py <captures_copy_dir> <out_dir> [per_length=2]
Work on a *copy* of ~/Library/Application Support/com.mrgnhnt.kass/captures, never the original.
Writes real<LL><a|b>.wav (no .txt reference: MLX output is used as the reference transcript).
"""

import subprocess
import sys
import wave
from pathlib import Path

src, out = Path(sys.argv[1]), Path(sys.argv[2])
per = int(sys.argv[3]) if len(sys.argv) > 3 else 2
out.mkdir(parents=True, exist_ok=True)

rows = []
for f in src.glob("*.wav"):
    with wave.open(str(f)) as w:
        rows.append((w.getnframes() / w.getframerate(), f))

for target in (2, 5, 9, 14):
    for i, (dur, f) in enumerate(sorted(rows, key=lambda r: abs(r[0] - target))[:per]):
        dst = out / f"real{target:02d}{'abcdef'[i]}.wav"
        subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(f), str(dst)], check=True)
        print(dst.name, round(dur, 2), "s", f.name)
