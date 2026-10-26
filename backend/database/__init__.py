"""Database package — ORM models, session management, and migrations.

Re-exports all public symbols so that ``from .database import get_db``
and ``from .database import Capture as DBCapture`` work without importers
reaching into submodules.
"""

from .models import (
    AppStyle,
    Base,
    Capture,
    CaptureFeedback,
    CaptureSettings,
    DictionaryEntry,
    KnownName,
    RetiredCapture,
    SyncChange,
    TakeReport,
    WritingStyle,
)
from .session import SessionLocal, _db_path, engine, get_db, init_db

__all__ = [
    "AppStyle",
    "Base",
    "Capture",
    "CaptureFeedback",
    "CaptureSettings",
    "DictionaryEntry",
    "KnownName",
    "RetiredCapture",
    "SessionLocal",
    "SyncChange",
    "TakeReport",
    "WritingStyle",
    "_db_path",
    "engine",
    "get_db",
    "init_db",
]
