#!/usr/bin/env python3
"""Synthesize the parity corpus's audio with macOS's built-in voice.

The WAVs are committed, so the set stays fixed even when macOS changes the
voice; run this only to add or change a recording. Existing files are kept
unless --force is given.

    python3 scripts/parity/make_audio.py [--force] [name ...]
"""

import argparse
import json
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent
CORPUS = ROOT / "corpus.json"
AUDIO = ROOT / "audio"


def speak(text: str, voice: dict, folder: Path) -> bytes:
    aiff, wav = folder / "part.aiff", folder / "part.wav"
    subprocess.run(["say", "-v", voice["name"], "-r", str(voice["rate"]), "-o", str(aiff), text], check=True)
    subprocess.run(
        ["afconvert", "-f", "WAVE", "-d", f"LEI16@{voice['sample_rate']}", "-c", "1", str(aiff), str(wav)],
        check=True,
    )
    with wave.open(str(wav)) as audio:
        return audio.readframes(audio.getnframes())


def silence(ms: int, rate: int) -> bytes:
    return bytes(2 * rate * ms // 1000)


def build(spec: dict, voice: dict) -> bytes:
    rate = voice["sample_rate"]
    if "silence_ms" in spec:
        return silence(spec["silence_ms"], rate)
    parts = [silence(voice["lead_ms"], rate)]
    with tempfile.TemporaryDirectory(prefix="kass-parity-") as folder:
        for index, text in enumerate(spec["speech"]):
            if index:
                parts.append(silence(spec.get("pause_ms", 0), rate))
            parts.append(speak(text, voice, Path(folder)))
    return b"".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("names", nargs="*", help="recordings to make (default: every missing one)")
    parser.add_argument("--force", action="store_true", help="overwrite existing recordings")
    args = parser.parse_args()
    corpus = json.loads(CORPUS.read_text())
    voice = corpus["voice"]
    unknown = set(args.names) - set(corpus["audio"])
    if unknown:
        print(f"Not in corpus.json: {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2
    AUDIO.mkdir(exist_ok=True)
    for name, spec in corpus["audio"].items():
        if args.names and name not in args.names:
            continue
        path = AUDIO / f"{name}.wav"
        if path.exists() and not args.force:
            continue
        pcm = build(spec, voice)
        with wave.open(str(path), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(2)
            audio.setframerate(voice["sample_rate"])
            audio.writeframes(pcm)
        print(f"{path.relative_to(ROOT)}  {len(pcm) / 2 / voice['sample_rate']:.2f} s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
