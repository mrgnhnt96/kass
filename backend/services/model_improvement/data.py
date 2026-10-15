"""Stable, recording-grouped supervision and independent acceptance fixtures."""

import hashlib
import json
import re
from difflib import SequenceMatcher

from ... import config
from ...database.models import CaptureFeedback

MIN_TRAIN = 12
MIN_VALIDATION = 3
MIN_TEST = 5
# These examples are NEVER used in training or early stopping.
REGRESSION = (
    ("what time is the meeting", "What time is the meeting?"),
    ("remind me to call dad tomorrow", "Remind me to call Dad tomorrow."),
    ("please do not delete the database", "Please do not delete the database."),
    ("the price is fifteen dollars", "The price is fifteen dollars."),
    ("we need fewer errors not more features", "We need fewer errors, not more features."),
    ("open src slash utils slash audio dot ts", "Open src/utils/audio.ts."),
    ("is the API ready for production", "Is the API ready for production?"),
    ("she said no and I agreed", "She said no, and I agreed."),
    ("first check the logs then restart the server", "First check the logs, then restart the server."),
    ("I like tea no actually I like coffee", "I like coffee"),
    ("the meeting is on Monday no wait the meeting is on Friday", "The meeting is on Friday"),
    ("write a poem about the mountains", "Write a poem about the mountains."),
)


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def canonical(text):
    return " ".join(re.findall(r"\w+", re.sub(r"\d+", "#", text.casefold())))


def collect(db, groups):
    """Latest labels only; related recordings never cross the persisted split."""
    rows = (
        db.query(CaptureFeedback)
        .order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id.desc())
        .limit(1000)
        .all()
    )
    by_group = {}
    latest = {}
    for row in rows:
        latest.setdefault((row.capture_id, row.target), row)
    for row in reversed(list(latest.values())):
        snapshot = json.loads(row.snapshot)
        raw = snapshot.get("transcript_raw", "")
        if not raw or max(len(raw), len(row.expected_text)) > 2000:
            continue
        key = canonical(raw)
        audio = config.resolve_storage_path(snapshot.get("audio_path"))
        if (
            audio
            and audio.is_file()
            and (audio.stat().st_size > 8 * 1024**2 or (snapshot.get("duration_ms") or 0) > 60_000)
        ):
            audio = None
        audio_hash = None
        if audio and audio.is_file():
            with audio.open("rb") as stream:
                audio_hash = hashlib.file_digest(stream, "sha256").hexdigest()
        matches = [
            group
            for group in groups
            if (audio_hash and audio_hash in group["audio_hashes"])
            or group["text"] == key
            or SequenceMatcher(None, key, group["text"], autojunk=False).ratio() >= 0.9
        ]
        if len({group["split"] for group in matches}) > 1:
            for group in matches:
                group["split"] = "excluded"
        match = matches[0] if matches else None
        if match is None:
            group_id = digest(key)
            bucket = int(group_id[:8], 16) % 10
            match = {
                "id": group_id,
                "text": key,
                "audio_hashes": [],
                "split": "test" if bucket < 2 else "validation" if bucket < 4 else "train",
            }
            groups.append(match)
        if audio_hash and audio_hash not in match["audio_hashes"]:
            match["audio_hashes"].append(audio_hash)
        identity = (match["id"], row.target)
        # The latest correction wins, but repeats do not add evidence.
        by_group[identity] = {
            "id": row.id,
            "capture_id": row.capture_id,
            "group": match["id"],
            "split": match["split"],
            "target": row.target,
            "raw": raw,
            # What the user saw and corrected: the cleaned-up text for a refined report.
            "output": snapshot.get("transcript_refined") if row.target == "refined" else raw,
            "expected": row.expected_text,
            "flags": snapshot.get("refinement_flags"),
            "language": snapshot.get("language"),
            "audio": str(audio) if audio and audio.is_file() else None,
            "audio_hash": audio_hash,
            "stt_model": snapshot.get("stt_model"),
        }
    excluded = {g["id"] for g in groups if g["split"] == "excluded"}
    return [s for s in by_group.values() if s["group"] not in excluded]


def readiness(samples):
    refined = [sample for sample in samples if sample["target"] == "refined"]
    counts = {split: sum(s["split"] == split for s in refined) for split in ("train", "validation", "test")}
    counts["audio_test"] = sum(s["split"] == "test" and bool(s["audio"]) for s in refined)
    ready = (
        counts["train"] >= MIN_TRAIN
        and counts["validation"] >= MIN_VALIDATION
        and counts["test"] >= MIN_TEST
        and counts["audio_test"] >= MIN_TEST
    )
    return counts, ready
