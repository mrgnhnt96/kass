"""The rows a stand-in records to send to a shared Kass (changes.py); the
Rust server (kass-server's sync module) sends and copies them."""

from ...database.models import (
    AppStyle,
    Capture,
    CaptureFeedback,
    CaptureSettings,
    DictionaryEntry,
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


def kind_of(row) -> str | None:
    for kind, model in KINDS.items():
        if isinstance(row, model):
            return kind
    return None


def ref_id(row) -> str:
    """The row's id as text: its primary key (one column for every kind)."""
    (column,) = row.__table__.primary_key.columns
    return str(getattr(row, column.key))
