"""Change the capture settings row in tests, as the Rust server's PUT /settings/captures does."""

from backend.services.settings import get_capture_settings


def save_settings(db, **values):
    row = get_capture_settings(db)
    if "history_retention_days" in values:
        # Choosing a window confirms it (app/server/src/settings.rs).
        values.setdefault("history_retention_confirmed", True)
    for name, value in values.items():
        setattr(row, name, value)
    db.commit()
    db.refresh(row)
    return row
