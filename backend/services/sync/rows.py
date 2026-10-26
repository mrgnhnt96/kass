"""The rows a stand-in sends to a shared Kass and copies from it, as JSON."""

from datetime import datetime

from sqlalchemy import DateTime

from ...database.models import (
    AppStyle,
    Capture,
    CaptureFeedback,
    CaptureSettings,
    DictionaryEntry,
    KnownName,
    TakeReport,
    WritingStyle,
)

# What a stand-in records and sends, by kind, in the order the shared Kass
# applies them: a capture's style before the capture, a report's capture
# before the report.
KINDS: dict[str, type] = {
    "style": WritingStyle,
    "app_style": AppStyle,
    "settings": CaptureSettings,
    "dictionary": DictionaryEntry,
    "capture": Capture,
    "feedback": CaptureFeedback,
    "take": TakeReport,
}

# Settings that belong to one Mac, never copied between them.
OWN_SETTINGS = frozenset({"id", "input_device_id", "usage_device_id", "usage_sent_through"})

# Columns of a kind that stay on each Mac: changing only these changes
# nothing the shared Kass keeps.
OWN_COLUMNS: dict[str, frozenset[str]] = {"settings": OWN_SETTINGS | {"updated_at"}}

# One row each, made with defaults the first time it's read: making it
# isn't something the user changed.
SINGLETONS = frozenset({"settings"})

# Copied from a shared Kass whole, replacing the stand-in's own.
MIRRORED: dict[str, type] = {
    "styles": WritingStyle,
    "app_styles": AppStyle,
    "dictionary": DictionaryEntry,
    "known_names": KnownName,
}


def kind_of(row) -> str | None:
    for kind, model in KINDS.items():
        if isinstance(row, model):
            return kind
    return None


def ref_id(row) -> str:
    """The row's id as text: its primary key (one column for every kind)."""
    (column,) = row.__table__.primary_key.columns
    return str(getattr(row, column.key))


def to_json(row) -> dict:
    out = {}
    for column in row.__table__.columns:
        value = getattr(row, column.key)
        out[column.key] = value.isoformat() if isinstance(value, datetime) else value
    return out


def from_json(model: type, data: dict, skip: frozenset[str] = frozenset()) -> dict:
    """Column values for ``model`` from :func:`to_json` output, dropping unknown keys."""
    values = {}
    for column in model.__table__.columns:
        if column.key not in data or column.key in skip:
            continue
        value = data[column.key]
        if isinstance(column.type, DateTime) and isinstance(value, str):
            value = datetime.fromisoformat(value)
        values[column.key] = value
    return values


def edited_at(row) -> datetime | None:
    """When the row last changed, for the later edit to win; None when it doesn't say."""
    return getattr(row, "updated_at", None) or getattr(row, "created_at", None)
