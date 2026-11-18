"""Background sounds for voice training: other people talking, and rooms.

The server downloads them once, when the user asks in Settings, and keeps
them in ``voice-training/sounds`` (server/src/voice_sounds.rs): LibriSpeech
test-clean utterances in ``talk.npz`` and DEMAND room recordings in
``noise.npz``, 16 kHz 16-bit. Training mixes them under the user's takes.
"""

import json

import numpy as np

from . import root

VERSION = 1


def _dir():
    return root() / "sounds"


def ready() -> bool:
    try:
        info = json.loads((_dir() / "sounds.json").read_text())
        return info.get("version") == VERSION and (_dir() / "talk.npz").is_file() and (_dir() / "noise.npz").is_file()
    except (OSError, ValueError):
        return False


def load() -> tuple[list[np.ndarray], list[np.ndarray]]:
    """(talk utterances, room recordings) as float32 at 16 kHz."""
    with np.load(_dir() / "talk.npz") as archive:
        talk = [archive[key].astype(np.float32) / 32768 for key in archive.files if key != "speakers"]
    with np.load(_dir() / "noise.npz") as archive:
        beds = [archive[key].astype(np.float32) / 32768 for key in archive.files]
    return talk, beds
