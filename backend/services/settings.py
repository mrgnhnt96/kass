"""
Server-side user settings — singleton rows persisted in SQLite so every
client window, API consumer, and headless flow reads the same preferences.

Capture/refine defaults live here: ``get_capture_settings`` lazily creates
the row with defaults. The Rust server's ``PUT /settings/captures`` changes
it (app/server/src/settings.rs).
"""

from sqlalchemy.orm import Session

from ..database import CaptureSettings as DBCaptureSettings
from ..utils.capture_chords import (
    default_push_to_talk_chord,
    default_toggle_to_talk_chord,
)

SINGLETON_ID = 1


def _get_or_create_capture_row(db: Session) -> DBCaptureSettings:
    row = db.query(DBCaptureSettings).filter(DBCaptureSettings.id == SINGLETON_ID).first()
    if row is None:
        row = DBCaptureSettings(
            id=SINGLETON_ID,
            chord_push_to_talk_keys=default_push_to_talk_chord(),
            chord_toggle_to_talk_keys=default_toggle_to_talk_chord(),
        )
        db.add(row)
        db.commit()
        db.refresh(row)
    return row


def get_capture_settings(db: Session) -> DBCaptureSettings:
    """Return the capture settings row, creating it with defaults if missing."""
    return _get_or_create_capture_row(db)
