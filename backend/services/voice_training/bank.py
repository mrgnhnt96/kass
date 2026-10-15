"""The voice bank: the user's clean takes, kept for voice training.

While the Mac is idle, ``scan`` reads dictations it hasn't seen yet:

- A clean take (quiet room, clear speech, 1–28 s) is copied into the bank as
  16 kHz mono audio with its transcript, or the user's own fix of the
  transcript when they made one. The user mostly fixes the cleaned-up text
  instead; the misheard terms fixed there are carried back to the transcript
  (``corrected_transcript``). History retention doesn't delete these
  copies; the bank keeps the newest ``BANK_SECONDS`` of takes.
- From a noisy take, the stretches without speech are kept as room noise
  (``ROOM_SECONDS`` at most), so training hears the rooms the user really
  dictates in.

Deleting a capture removes its take (``forget``), and turning on "discard
audio" empties the bank (``purge``). Takes are split once, by id, into
training (85%) and test (15%) takes; test takes are never trained on.
"""

import hashlib
import json
import logging
import re
import shutil
import threading
import wave
from datetime import datetime
from pathlib import Path

import numpy as np

from . import root

logger = logging.getLogger(__name__)

BANK_SECONDS = 2 * 3600
ROOM_SECONDS = 10 * 60
MIN_SECONDS, MAX_SECONDS = 1.0, 28.0
MAX_TEXT = 600
SCAN_BATCH = 400
TEST_PERCENT = 15
# A clean take: room noise below -55 dBFS and speech at least 25 dB above it.
CLEAN_FLOOR_DB, CLEAN_SNR_DB = -55.0, 25.0
# Room noise is taken from takes whose noise floor is above -50 dBFS.
NOISY_FLOOR_DB = -50.0
CUE_SECONDS = 0.35  # the start chime, which Silero can hear as a voice

_lock = threading.RLock()


def _dir() -> Path:
    return root() / "bank"


def _room_dir() -> Path:
    return root() / "room"


def _manifest_path() -> Path:
    return _dir() / "bank.json"


def _load() -> dict:
    try:
        data = json.loads(_manifest_path().read_text())
        if data.get("version") == 1:
            return data
    except (OSError, ValueError):
        pass
    return {"version": 1, "scanned_until": None, "takes": [], "room": []}


def _save(data: dict) -> None:
    _dir().mkdir(parents=True, exist_ok=True)
    temporary = _manifest_path().with_suffix(".tmp")
    temporary.write_text(json.dumps(data, indent=1))
    temporary.replace(_manifest_path())


def split(take_id: str) -> str:
    return "test" if int(hashlib.sha256(take_id.encode()).hexdigest()[:8], 16) % 100 < TEST_PERCENT else "train"


def _write_wav(path: Path, y: np.ndarray) -> None:
    pcm = np.clip(y * 32768, -32768, 32767).astype(np.int16)
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(16000)
        out.writeframes(pcm.tobytes())


def read_wav(path: Path) -> np.ndarray:
    with wave.open(str(path)) as source:
        return np.frombuffer(source.readframes(source.getnframes()), dtype=np.int16).astype(np.float32) / 32768


def _read_capture_audio(path: Path) -> np.ndarray:
    import soundfile

    from ...backends import whisper_audio

    audio, rate = soundfile.read(str(path), dtype="float32", always_2d=False)
    return whisper_audio.to_16k_mono_float32(audio, rate)


def analyze(y: np.ndarray) -> dict | None:
    """Noise floor, signal-to-noise ratio and where speech ends, from Silero.

    None when the voice detector isn't available.
    """
    from ..speech_detect import window_probabilities

    probabilities = window_probabilities(y)
    if probabilities is None or not len(probabilities):
        return None
    frames = y[: len(probabilities) * 512].reshape(-1, 512)
    level = 10 * np.log10((frames**2).mean(axis=1) + 1e-12)
    voiced = probabilities >= 0.5
    voiced[: int(CUE_SECONDS * 16000 / 512)] = False
    if voiced.sum() < 3:
        return {"voiced": 0.0, "probabilities": probabilities}
    speech = float(np.percentile(level[voiced], 90))
    quiet = level[~voiced]
    floor = float(np.percentile(quiet, 20)) if len(quiet) else float(np.percentile(level, 5))
    last = int(np.where(voiced)[0][-1])
    return {
        "voiced": float(voiced.sum() * 0.032),
        "floor": floor,
        "snr": speech - floor,
        "end": min(len(y) / 16000, (last + 1) * 0.032 + 0.15),
        "probabilities": probabilities,
    }


def _room_stretches(y: np.ndarray, probabilities: np.ndarray) -> np.ndarray:
    """The parts of a take without anyone speaking, in runs of at least 0.5 s."""
    quiet = probabilities < 0.1
    quiet[: int(0.4 * 16000 / 512)] = False
    pieces, start = [], None
    for index, value in enumerate([*quiet, False]):
        if value and start is None:
            start = index
        elif not value and start is not None:
            if index - start >= 16:
                pieces.append(y[start * 512 : index * 512])
            start = None
    return np.concatenate(pieces) if pieces else np.zeros(0, np.float32)


def corrected_transcript(raw: str, output: str, expected: str) -> str | None:
    """``raw`` with the misheard terms the user fixed in the cleaned-up text.

    The user corrects the cleaned-up text, not the transcript, so a fix like
    "Pikto" -> "Picto" is carried back to where the transcript has "Pikto".
    Wording and formatting changes stay out: they are the cleanup's job. A
    heard form that isn't in the transcript exactly once is skipped. None
    when nothing carries over.
    """
    from ..dictionary import misheard_terms

    text = raw
    for heard, written in misheard_terms(output, expected):
        pattern = re.compile(rf"(?<!\w){re.escape(heard)}(?!\w)", re.IGNORECASE)
        matches = list(pattern.finditer(text))
        if len(matches) == 1:
            text = text[: matches[0].start()] + written + text[matches[0].end() :]
    return text if text != raw else None


def _corrections(db, capture_ids: list[str]) -> dict[str, str]:
    """The user's newest fix of each raw transcript, else the transcript with
    the terms they fixed in the cleaned-up text. Re-dictations don't count."""
    from ...database.models import CaptureFeedback

    fixes: dict[str, tuple] = {}
    if not capture_ids:
        return {}
    rows = db.query(CaptureFeedback).filter(CaptureFeedback.capture_id.in_(capture_ids)).all()
    for row in rows:
        if row.source not in CaptureFeedback.EXPLICIT_SOURCES:
            continue
        if row.target == "raw":
            text = row.expected_text.strip()
        else:
            snapshot = json.loads(row.snapshot or "{}")
            text = corrected_transcript(
                snapshot.get("transcript_raw") or "", snapshot.get("transcript_refined") or "", row.expected_text
            )
            if text is None:
                continue
        # A fix of the transcript itself outranks one carried over from the cleaned-up text.
        rank = (row.target == "raw", row.created_at or datetime.min)
        if row.capture_id not in fixes or rank > fixes[row.capture_id][0]:
            fixes[row.capture_id] = (rank, text)
    return {key: value[1] for key, value in fixes.items()}


def _evict(data: dict) -> None:
    takes = sorted(data["takes"], key=lambda take: take["created"])
    total = sum(take["seconds"] for take in takes)
    while takes and total > BANK_SECONDS:
        old = takes.pop(0)
        total -= old["seconds"]
        (_dir() / f"{old['id']}.wav").unlink(missing_ok=True)
    data["takes"] = takes
    room = sorted(data["room"], key=lambda piece: piece["created"])
    total = sum(piece["seconds"] for piece in room)
    while room and total > ROOM_SECONDS:
        old = room.pop(0)
        total -= old["seconds"]
        (_room_dir() / f"{old['id']}.wav").unlink(missing_ok=True)
    data["room"] = room


def scan(db, settings, limit: int = SCAN_BATCH) -> dict:
    """Add dictations made since the last scan. Returns ``summary()``."""
    from ... import config
    from ...database.models import Capture

    if settings.discard_audio:
        purge()
        return summary()
    with _lock:
        data = _load()
        query = db.query(Capture).filter(Capture.source == "dictation")
        if data["scanned_until"]:
            query = query.filter(Capture.created_at > datetime.fromisoformat(data["scanned_until"]))
        rows = query.order_by(Capture.created_at).limit(limit).all()
        known = {take["id"] for take in data["takes"]}
        _dir().mkdir(parents=True, exist_ok=True)
        _room_dir().mkdir(parents=True, exist_ok=True)
        for row in rows:
            if row.created_at:
                data["scanned_until"] = row.created_at.isoformat()
            text = (row.transcript_raw or "").strip()
            path = config.resolve_storage_path(row.audio_path)
            if row.id in known or not text or len(text) > MAX_TEXT or path is None or not path.is_file():
                continue
            try:
                y = _read_capture_audio(path)
            except Exception:
                logger.warning("Voice bank couldn't read %s", path, exc_info=True)
                continue
            seconds = len(y) / 16000
            if not MIN_SECONDS <= seconds <= MAX_SECONDS:
                continue
            found = analyze(y)
            if found is None:
                break  # no voice detector: try again later
            created = (row.created_at or datetime.now()).isoformat()
            clean = found["voiced"] >= 0.5 and found["floor"] < CLEAN_FLOOR_DB and found["snr"] > CLEAN_SNR_DB
            if clean:
                _write_wav(_dir() / f"{row.id}.wav", y)
                data["takes"].append(
                    {
                        "id": row.id,
                        "text": text,
                        "transcript": text,
                        "end": round(found["end"] / 0.02) * 0.02,
                        "seconds": round(seconds, 2),
                        "created": created,
                        "split": split(row.id),
                    }
                )
            elif found.get("floor", -120) > NOISY_FLOOR_DB:
                room = _room_stretches(y, found["probabilities"])
                if len(room) >= 8000:
                    _write_wav(_room_dir() / f"{row.id}.wav", room)
                    data["room"].append({"id": row.id, "seconds": round(len(room) / 16000, 2), "created": created})
        fixes = _corrections(db, [take["id"] for take in data["takes"]])
        for take in data["takes"]:
            take["text"] = fixes.get(take["id"], take["transcript"])
            take["corrected"] = take["text"] != take["transcript"]
        _evict(data)
        _save(data)
        return summary(data)


def forget(capture_id: str) -> None:
    """Remove a deleted capture's take and room noise."""
    with _lock:
        data = _load()
        if not any(item["id"] == capture_id for item in data["takes"] + data["room"]):
            return
        data["takes"] = [take for take in data["takes"] if take["id"] != capture_id]
        data["room"] = [piece for piece in data["room"] if piece["id"] != capture_id]
        (_dir() / f"{capture_id}.wav").unlink(missing_ok=True)
        (_room_dir() / f"{capture_id}.wav").unlink(missing_ok=True)
        _save(data)


def purge() -> None:
    """Delete every kept take and room recording."""
    with _lock:
        for directory in (_dir(), _room_dir()):
            if directory.exists():
                shutil.rmtree(directory, ignore_errors=True)


def takes() -> list[dict]:
    """Every take, with the path of its audio."""
    with _lock:
        return [dict(take, audio=str(_dir() / f"{take['id']}.wav")) for take in _load()["takes"]]


def room_paths() -> list[str]:
    with _lock:
        return [str(_room_dir() / f"{piece['id']}.wav") for piece in _load()["room"]]


def digest() -> str:
    """Changes whenever a take, its label or the room noise changes."""
    with _lock:
        data = _load()
        items = sorted((take["id"], take["text"]) for take in data["takes"])
        items += sorted(("room", piece["id"]) for piece in data["room"])
        return hashlib.sha256(json.dumps(items).encode()).hexdigest()


def summary(data: dict | None = None) -> dict:
    with _lock:
        data = data or _load()
        test = sum(take["split"] == "test" for take in data["takes"])
        return {
            "takes": len(data["takes"]),
            "train": len(data["takes"]) - test,
            "test": test,
            "minutes": round(sum(take["seconds"] for take in data["takes"]) / 60, 1),
            "room_minutes": round(sum(piece["seconds"] for piece in data["room"]) / 60, 1),
        }
