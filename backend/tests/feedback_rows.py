"""File a correction in tests, as the Rust server's POST /captures/{id}/feedback saves it."""

import json

from backend.database.models import Capture, CaptureFeedback

_SNAPSHOT = (
    "id",
    "audio_path",
    "source",
    "duration_ms",
    "transcript_raw",
    "transcript_refined",
    "app_bundle_id",
    "app_name",
)


def file_report(db, capture_id, expected, target="refined", source="manual"):
    capture = db.get(Capture, capture_id)
    row = CaptureFeedback(
        capture_id=capture_id,
        target=target,
        expected_text=expected,
        notes="",
        snapshot=json.dumps({name: getattr(capture, name) for name in _SNAPSHOT}),
        source=source,
    )
    db.add(row)
    db.commit()
    db.refresh(row)
    return row
