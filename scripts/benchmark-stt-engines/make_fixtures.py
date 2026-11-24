"""Engine-comparison fixtures (item 6). Synthesized with macOS `say`, 16 kHz mono PCM16."""

import random
import struct
import subprocess
import sys
import wave
from pathlib import Path

root = Path(sys.argv[1])
root.mkdir(parents=True, exist_ok=True)
random.seed(7)


def speak(stem, text):
    aiff, wav = root / f"_{stem}.aiff", root / f"_{stem}.wav"
    subprocess.run(["say", "-v", "Samantha", "-r", "165", "-o", str(aiff), text], check=True)
    subprocess.run(["afconvert", "-f", "WAVE", "-d", "LEI16@16000", "-c", "1", str(aiff), str(wav)], check=True)
    with wave.open(str(wav)) as a:
        pcm = a.readframes(a.getnframes())
    aiff.unlink()
    wav.unlink()
    return pcm


def write(name, text, pcm):
    (root / f"{name}.txt").write_text(text + "\n")
    with wave.open(str(root / f"{name}.wav"), "wb") as a:
        a.setnchannels(1)
        a.setsampwidth(2)
        a.setframerate(16000)
        a.writeframes(pcm)
    print(name, round(len(pcm) / 32000, 2), "s")


def tail(seconds):
    return b"".join(struct.pack("<h", random.randint(-30, 30)) for _ in range(round(seconds * 16000)))


SHORT = "Send the draft to the team tonight."
SIX = "Please move the planning meeting to Thursday afternoon and invite the design team, then send everyone the updated agenda."
FIFTEEN = (
    "Please move the planning meeting to Thursday afternoon. We need to review the project timeline "
    "and confirm the next release date. Add a reminder to check the documentation before sending the update. "
    "The new version should explain the recording settings clearly."
)
PAUSED = (
    "Please move the planning meeting to Thursday afternoon. We need to review the project timeline and confirm the next release date. "
    "Add a reminder to check the documentation before sending the update to the team. The new version should include a clear "
    "explanation of the recording settings and microphone selection. During the review, compare the original transcript with the "
    "final output and make sure that every sentence is preserved. We should also test a longer recording with several pauses and "
    "verify that the final words arrive promptly. After the meeting, send a short summary of the decisions and assign each "
    "remaining task to the person responsible for completing it."
)
TECH = [
    "Set the retry count to seven",
    "Do not disable authentication",
    "The account balance is minus five dollars",
    "Open the package file before changing the port to eight thousand",
]

write("short", SHORT, speak("short", SHORT) + tail(0.35))
write("six", SIX, speak("six", SIX))
write("fifteen", FIFTEEN, speak("fifteen", FIFTEEN))
parts = PAUSED.split(". ")
frames = []
for i, p in enumerate(parts):
    frames.append(speak(f"p{i}", p))
    if i < len(parts) - 1:
        frames.append(bytes(32000))
write("paused40", PAUSED, b"".join(frames))
frames = []
for i, p in enumerate(TECH):
    frames.append(speak(f"t{i}", p))
    if i < len(TECH) - 1:
        frames.append(bytes(32000))
write("tech", ". ".join(TECH) + ".", b"".join(frames))
