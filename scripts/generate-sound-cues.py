#!/usr/bin/env python3
"""Generate the dictation sound cues bundled with the desktop app.

Writes start.wav, stop.wav, error.wav and style.wav (16-bit mono, 44.1 kHz) into
app/sounds/, where `sound_cues.rs` embeds them. Standard library
only; rerun after changing a cue and commit the WAVs:

    python3 scripts/generate-sound-cues.py [output-dir]
"""

import math
import struct
import sys
import wave
from pathlib import Path

RATE = 44100
OUT_DIR = Path(__file__).resolve().parents[1] / "app" / "sounds"


def tone(freqs, seconds, volume=0.35, harmonic=0.15):
    """A chord of sine tones with a 5 ms attack and a soft exponential decay."""
    samples = []
    for i in range(int(RATE * seconds)):
        t = i / RATE
        envelope = min(1, t / 0.005) * math.exp(-t * 9)
        s = sum(math.sin(2 * math.pi * f * t) + harmonic * math.sin(4 * math.pi * f * t) for f in freqs) / len(freqs)
        samples.append(s * envelope * volume)
    return samples


def reversed_flutter(seconds, shift, peak=0.21, attack=0.022):
    """A flutter played backwards, so it swells in and settles at the end.

    Forwards it is a low note and the fifth above alternating 25 ms apart
    (A4 and E5, moved `shift` half-steps), each tap easing in over `attack`,
    the earlier taps ringing longer than the last. The default peak plays back
    at the level chosen by ear at the app's default cue volume (0.5).
    """
    low = 440.0 * 2 ** (shift / 12)
    fifth = low * 2 ** (7 / 12)
    taps = [(low, 0.0, 12), (fifth, 0.025, 12), (low, 0.05, 12), (fifth, 0.075, 30)]
    n = int(RATE * seconds)
    samples = [0.0] * n
    for f, at, decay in taps:
        for i in range(int(RATE * at), n):
            t = i / RATE - at
            ease = 0.5 - 0.5 * math.cos(math.pi * min(1, t / attack))
            s = math.sin(2 * math.pi * f * t) + 0.05 * math.sin(4 * math.pi * f * t)
            samples[i] += 0.7 * s * ease * math.exp(-t * decay)
    top = max(abs(s) for s in samples)
    fade = 0.04 * RATE
    return [s / top * peak * min(1, (n - i) / fade) for i, s in enumerate(samples)][::-1]


def ping(notes, spacing, seconds, peak=0.12, decay=38):
    """Bell-like taps `spacing` seconds apart, each a sine with a faint octave
    and a fast decay: small and bright, well above the cues' flutters."""
    n = int(RATE * seconds)
    samples = [0.0] * n
    for index, f in enumerate(notes):
        at = index * spacing
        for i in range(int(RATE * at), n):
            t = i / RATE - at
            envelope = min(1, t / 0.003) * math.exp(-t * decay)
            samples[i] += (math.sin(2 * math.pi * f * t) + 0.2 * math.sin(4 * math.pi * f * t)) * envelope
    top = max(abs(s) for s in samples)
    fade = 0.02 * RATE
    return [s / top * peak * min(1, (n - i) / fade) for i, s in enumerate(samples)]


def sequence(*parts, gap=0.0):
    samples = []
    for part in parts:
        samples += part + [0.0] * int(RATE * gap)
    return samples


def save(path, samples):
    with wave.open(str(path), "w") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1, min(1, s)) * 32767)) for s in samples))


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else OUT_DIR
    out.mkdir(parents=True, exist_ok=True)
    save(out / "start.wav", reversed_flutter(0.22, -2))  # G4-D5
    save(out / "stop.wav", reversed_flutter(0.22, -7))  # D4-A4
    save(out / "error.wav", sequence(tone([330], 0.1), tone([262], 0.18), gap=0.02))
    # The writing style changed by voice: a soft double tap up an octave, A5
    # then A6, to go with the chip's name rolling up above the pill.
    save(out / "style.wav", ping([880.0, 1760.0], 0.06, 0.2, peak=0.13, decay=28))
    print(f"wrote start.wav, stop.wav, error.wav, style.wav to {out}")


if __name__ == "__main__":
    main()
