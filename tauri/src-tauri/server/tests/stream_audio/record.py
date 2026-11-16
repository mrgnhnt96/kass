"""Record what Python's dictation stream does with incoming audio, for tests/stream_audio.rs.

Plays takes made from the parity recordings into backend/services/capture_stream.py's
StreamingCapture, with the real Silero voice detector, at several sample
rates and frame sizes. Recognition and cleanup are stubbed out: only the
audio side is recorded. For each take, after every frame: how long it has
been quiet, where the voice started and where the last pause cut was. At the
end: the voiced windows, the phrase windows sent to recognition and whether
the take fell back to recognizing the whole recording.

Run from the repository root:

    backend/venv/bin/python tauri/src-tauri/server/tests/stream_audio/record.py
"""

import asyncio
import json
import struct
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT))

from backend import config  # noqa: E402
from backend.models import CaptureSettingsResponse  # noqa: E402
from backend.services import capture_stream, speech_detect  # noqa: E402

AUDIO = ROOT / "scripts" / "parity" / "audio"
OUT = Path(__file__).resolve().parent / "python.json"


def clip(name: str) -> np.ndarray:
    with wave.open(str(AUDIO / name)) as w:
        assert w.getframerate() == 16000 and w.getnchannels() == 1 and w.getsampwidth() == 2
        return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2")


def at_rate(audio: np.ndarray, rate: int) -> np.ndarray:
    """16 kHz audio held sample by sample at ``rate``: integer math the Rust test repeats exactly."""
    count = len(audio) * rate // 16000
    return audio[(np.arange(count, dtype=np.int64) * 16000) // rate]


def take(parts: list) -> np.ndarray:
    """Clips (a file name) and silences (seconds) one after another, at 16 kHz."""
    pieces = [clip(p) if isinstance(p, str) else np.zeros(round(p * 16000), dtype="<i2") for p in parts]
    return np.concatenate(pieces)


CLIPS = sorted(path.name for path in AUDIO.glob("*.wav"))
CASES = [dict(name=f"clip {name}", parts=[name], rate=16000, frame_ms=100) for name in CLIPS]
CASES += [
    dict(name="pauses at 48 kHz", parts=[CLIPS[0], 1.0, CLIPS[1], 0.8, CLIPS[2], 0.4, CLIPS[3]], rate=48000, frame_ms=100),
    dict(name="pauses at 44.1 kHz", parts=[CLIPS[4], 1.2, CLIPS[5], 0.75, CLIPS[6]], rate=44100, frame_ms=100),
    dict(name="pauses at 24 kHz, odd frames", parts=[CLIPS[7], 0.9, CLIPS[8], 1.5, CLIPS[9]], rate=24000, frame_ms=37),
    dict(name="pauses at 32 kHz, small frames", parts=[CLIPS[10], 0.7, CLIPS[11], 2.0, CLIPS[12]], rate=32000, frame_ms=20),
    dict(name="silence then speech", parts=[1.5, CLIPS[13], 1.0], rate=16000, frame_ms=100),
    dict(name="only silence", parts=[3.0], rate=48000, frame_ms=100),
    dict(name="start cue ignored", parts=[CLIPS[14], 0.8, CLIPS[15]], rate=48000, frame_ms=100, start_cue_ms=300),
    dict(
        name="style cue ignored",
        parts=[CLIPS[16], 1.0, CLIPS[17]],
        rate=48000,
        frame_ms=100,
        # (frame index after which it arrives, start sample, end sample)
        cues=[(10, 24000, 52000), (30, 150000, 198000)],
    ),
    dict(
        name="no voice detector, long speech",
        parts=[CLIPS[0], 0.8, CLIPS[1], 0.8, CLIPS[2], CLIPS[3], CLIPS[4], CLIPS[5], CLIPS[6], CLIPS[12]],
        rate=16000,
        frame_ms=100,
        model=False,
    ),
    dict(
        name="long speech without a pause",
        parts=[CLIPS[18], CLIPS[19], CLIPS[0], CLIPS[1], CLIPS[2], CLIPS[3]],
        rate=16000,
        frame_ms=100,
    ),
]


async def run(case: dict, data_dir: Path) -> dict:
    config._data_dir = data_dir
    if case.get("model", True):
        speech_detect._unavailable = False
        speech_detect.load()
        assert speech_detect._session is not None
    else:
        speech_detect._session, speech_detect._unavailable = None, True
    rate = case["rate"]
    audio = at_rate(take(case["parts"]), rate)
    start = dict(type="start", protocol_version=1, sample_rate=rate, channels=1, encoding="pcm_s16le")
    if "start_cue_ms" in case:
        start["start_cue_ms"] = case["start_cue_ms"]

    async def send(_event):
        pass

    session = capture_stream.StreamingCapture(start, CaptureSettingsResponse(auto_refine=False), send)
    windows, peeks, full = [], [], []

    async def recognize(pcm, start=None, measure=True):
        offset = session.offset if start is None else start
        size = len(pcm) // 2
        if not measure:
            peeks.append([offset, size])
        else:
            windows.append(dict(start=offset, size=size, heard=session.speech.heard(offset, offset + size)))
        return ""

    async def accept(_text, paused=False):
        windows[-1]["paused"] = paused

    async def reconcile_full_audio():
        full.append(dict(samples=session.samples, heard=session.speech.heard(0, session.samples)))
        session.covered = session.samples

    session.recognize = recognize
    session.accept = accept
    session.reconcile_full_audio = reconcile_full_audio
    worker = asyncio.create_task(session.run())
    frame = rate * case["frame_ms"] // 1000
    cues = {at: (low, high) for at, low, high in case.get("cues", [])}
    frames = []
    for index, offset in enumerate(range(0, len(audio), frame)):
        pcm = audio[offset : offset + frame]
        session.append(struct.pack("<II", session.sequence, session.samples) + pcm.tobytes())
        if index in cues:
            session.ignore_cue(*cues[index])
        for _ in range(8):
            await asyncio.sleep(0)
        frames.append([session.speech.quiet(), session.speech.first_voice(), session.last_cut])
    session.finish()
    await worker
    session.close()
    return dict(
        name=case["name"],
        parts=case["parts"],
        rate=rate,
        frame=frame,
        start_cue_ms=case.get("start_cue_ms", 0),
        cues=case.get("cues", []),
        model=case.get("model", True),
        samples=session.samples,
        voiced=list(session.speech.voiced) if session.speech.session is not None else None,
        analyzed=session.speech.analyzed,
        frames=frames,
        windows=windows,
        peeks=peeks,
        full_audio=full,
        covered=session.covered,
        offset=session.offset,
        degraded_reason=session.degraded_reason,
    )


async def main() -> None:
    cases = []
    with tempfile.TemporaryDirectory() as data_dir:
        for case in CASES:
            cases.append(await run(case, Path(data_dir)))
    OUT.write_text(json.dumps(dict(clips=CLIPS, cases=cases), indent=None, separators=(",", ":")) + "\n")
    print(f"Recorded {len(cases)} takes to {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    asyncio.run(main())
