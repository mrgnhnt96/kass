"""Column-level migrations for the kass SQLite database.

Why not Alembic?  kass is a single-user desktop app shipping as a
PyInstaller binary.  Every user has exactly one SQLite file.  Alembic's
strengths -- migration tracking across environments, rollback, team
coordination -- don't apply here and would add bundling complexity
(alembic.ini, env.py, versions/ directory all need to survive
PyInstaller).  The column-existence checks below are idempotent, run in
<50 ms on startup, and have worked reliably across 12 schema changes.
If the project ever moves to a server-based deployment or Postgres, this
decision should be revisited.

Adding a new migration:
    1. Append a new ``_migrate_*`` helper at the bottom of this file.
    2. Call it from ``run_migrations()`` in the appropriate spot.
    3. The helper should check column/table existence before acting
       (idempotent) and print a short message when it does real work.

Tables from removed features (voice profiles, generations, stories, effects,
audio channels, MCP bindings) are no longer created or migrated, but existing
databases keep them untouched: nothing here drops user data.
"""

import json
import logging

from sqlalchemy import inspect, text

from ..services.commands import default_transforms
from ..utils.capture_chords import (
    default_command_chord,
    default_push_to_talk_chord,
    default_toggle_to_talk_chord,
)

logger = logging.getLogger(__name__)


def run_migrations(engine) -> None:
    """Run all schema migrations.  Safe to call on every startup."""
    inspector = inspect(engine)
    tables = set(inspector.get_table_names())

    _migrate_capture_settings(engine, inspector, tables)
    _migrate_captures(engine, inspector, tables)
    _migrate_capture_feedback(engine, inspector, tables)
    _migrate_dictionary_entries(engine, inspector, tables)
    _migrate_writing_styles(engine, inspector, tables)
    _rename_own_bundle_id(engine, inspector, tables)


# -- helpers ---------------------------------------------------------------


def _get_columns(inspector, table: str) -> set[str]:
    return {col["name"] for col in inspector.get_columns(table)}


def _add_column(engine, table: str, column_sql: str, label: str) -> None:
    """Add a column if it doesn't already exist."""
    with engine.connect() as conn:
        conn.execute(text(f"ALTER TABLE {table} ADD COLUMN {column_sql}"))
        conn.commit()
    logger.info("Added %s column to %s", label, table)


# -- per-table migrations --------------------------------------------------


def _migrate_captures(engine, inspector, tables: set[str]) -> None:
    if "captures" not in tables:
        return
    if "refinement_review" not in _get_columns(inspector, "captures"):
        _add_column(engine, "captures", "refinement_review TEXT", "refinement_review")
    columns = _get_columns(inspector, "captures")
    for column in ("app_bundle_id", "app_name"):
        if column not in columns:
            _add_column(engine, "captures", f"{column} VARCHAR", column)
    for column, kind in (
        ("command_selection", "TEXT"),
        ("command_instruction", "TEXT"),
        ("command_transform", "VARCHAR"),
        ("style_id", "VARCHAR"),
        ("app_category", "VARCHAR"),
        ("teaches_style_id", "VARCHAR"),
        ("audio_deleted", "BOOLEAN NOT NULL DEFAULT 0"),
        ("prosody", "TEXT"),
        ("mac_id", "VARCHAR"),
        ("mac_name", "VARCHAR"),
    ):
        if column not in columns:
            _add_column(engine, "captures", f"{column} {kind}", column)


def _migrate_capture_feedback(engine, inspector, tables: set[str]) -> None:
    if "capture_feedback" not in tables:
        return
    columns = _get_columns(inspector, "capture_feedback")
    for column in ("app_bundle_id", "teaches_style_id"):
        if column not in columns:
            _add_column(engine, "capture_feedback", f"{column} VARCHAR", column)
    if "source" not in columns:
        _add_column(engine, "capture_feedback", "source VARCHAR NOT NULL DEFAULT 'manual'", "source")
    if "filed_by" not in columns:
        _add_column(engine, "capture_feedback", "filed_by VARCHAR", "filed_by")


def _migrate_writing_styles(engine, inspector, tables: set[str]) -> None:
    if "writing_styles" not in tables:
        return
    if "description" not in _get_columns(inspector, "writing_styles"):
        _add_column(engine, "writing_styles", "description TEXT", "description")


def _migrate_dictionary_entries(engine, inspector, tables: set[str]) -> None:
    # One entry can apply in several places (docs/plans/DICTIONARIES.md).
    if "dictionary_entries" not in tables:
        return
    columns = _get_columns(inspector, "dictionary_entries")
    if "group_id" not in columns:
        _add_column(engine, "dictionary_entries", "group_id VARCHAR", "group_id")
    # Existing entries keep matching by sound, as they always have.
    if "match_sound" not in columns:
        _add_column(engine, "dictionary_entries", "match_sound BOOLEAN NOT NULL DEFAULT 1", "match_sound")
    if "source" not in columns:
        _add_column(engine, "dictionary_entries", "source VARCHAR", "source")
    if "added_by" not in columns:
        _add_column(engine, "dictionary_entries", "added_by VARCHAR", "added_by")
    if "phrase" not in columns:
        _add_column(engine, "dictionary_entries", "phrase BOOLEAN NOT NULL DEFAULT 0", "phrase")


# Kass was Herga (com.mrgnhnt.herga), and before that Voicebox, with bundle
# id sh.voicebox.app and then, briefly, com.mrgnhnt.voicebox. Data recorded
# against its own window keeps meaning Kass under the new one.
_OLD_BUNDLES = ("sh.voicebox.app", "com.mrgnhnt.voicebox", "com.mrgnhnt.herga")
_BUNDLE = "com.mrgnhnt.kass"


def _rename_own_bundle_id(engine, inspector, tables: set[str]) -> None:
    with engine.connect() as conn:
        for old in _OLD_BUNDLES:
            params = {"old": old, "new": _BUNDLE}
            for table in ("captures", "capture_feedback", "retired_captures"):
                if table in tables and "app_bundle_id" in _get_columns(inspector, table):
                    conn.execute(
                        text(f"UPDATE {table} SET app_bundle_id = :new WHERE app_bundle_id = :old"),
                        params,
                    )
            # Keyed tables: a row already under the new id wins over the old one.
            if "app_styles" in tables:
                conn.execute(
                    text("UPDATE OR IGNORE app_styles SET bundle_id = :new WHERE bundle_id = :old"),
                    params,
                )
                conn.execute(text("DELETE FROM app_styles WHERE bundle_id = :old"), params)
            if "dictionary_entries" in tables:
                conn.execute(
                    text(
                        "UPDATE OR IGNORE dictionary_entries SET scope_id = :new"
                        " WHERE scope = 'app' AND scope_id = :old"
                    ),
                    params,
                )
                conn.execute(
                    text("DELETE FROM dictionary_entries WHERE scope = 'app' AND scope_id = :old"),
                    params,
                )
        conn.commit()


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _migrate_capture_settings(engine, inspector, tables: set[str]) -> None:
    if "capture_settings" not in tables:
        return
    columns = _get_columns(inspector, "capture_settings")
    push_default = json.dumps(default_push_to_talk_chord())
    toggle_default = json.dumps(default_toggle_to_talk_chord())
    if "allow_auto_paste" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "allow_auto_paste BOOLEAN NOT NULL DEFAULT 1",
            "allow_auto_paste",
        )
    if "input_device_id" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "input_device_id VARCHAR",
            "input_device_id",
        )
    if "chord_push_to_talk_keys" not in columns:
        _add_column(
            engine,
            "capture_settings",
            f"chord_push_to_talk_keys TEXT NOT NULL DEFAULT '{push_default}'",
            "chord_push_to_talk_keys",
        )
    if "chord_toggle_to_talk_keys" not in columns:
        _add_column(
            engine,
            "capture_settings",
            f"chord_toggle_to_talk_keys TEXT NOT NULL DEFAULT '{toggle_default}'",
            "chord_toggle_to_talk_keys",
        )
    if "hotkey_enabled" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "hotkey_enabled BOOLEAN NOT NULL DEFAULT 0",
            "hotkey_enabled",
        )
    if "live_text" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "live_text BOOLEAN NOT NULL DEFAULT 0",
            "live_text",
        )
    if "voice_edits" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "voice_edits BOOLEAN NOT NULL DEFAULT 1",
            "voice_edits",
        )
    if "expressive" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "expressive BOOLEAN NOT NULL DEFAULT 1",
            "expressive",
        )
    if "sound_cues" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "sound_cues BOOLEAN NOT NULL DEFAULT 1",
            "sound_cues",
        )
    if "sound_cue_volume" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "sound_cue_volume FLOAT NOT NULL DEFAULT 0.5",
            "sound_cue_volume",
        )
    if "chord_command_keys" not in columns:
        _add_column(
            engine,
            "capture_settings",
            f"chord_command_keys TEXT NOT NULL DEFAULT {_sql_literal(json.dumps(default_command_chord()))}",
            "chord_command_keys",
        )
    if "command_transforms" not in columns:
        _add_column(
            engine,
            "capture_settings",
            f"command_transforms TEXT NOT NULL DEFAULT {_sql_literal(json.dumps(default_transforms()))}",
            "command_transforms",
        )
    if "punctuation_style" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "punctuation_style VARCHAR NOT NULL DEFAULT 'standard'",
            "punctuation_style",
        )
    if "history_retention_days" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "history_retention_days INTEGER NOT NULL DEFAULT 30",
            "history_retention_days",
        )
    if "history_retention_confirmed" not in columns:
        # Existing installs are asked before their first deletion.
        _add_column(
            engine,
            "capture_settings",
            "history_retention_confirmed BOOLEAN NOT NULL DEFAULT 0",
            "history_retention_confirmed",
        )
    if "onboarding_completed" not in columns:
        # A settings row from before onboarding existed belongs to an install
        # that was already set up, so it never sees onboarding.
        _add_column(
            engine,
            "capture_settings",
            "onboarding_completed BOOLEAN NOT NULL DEFAULT 1",
            "onboarding_completed",
        )
    if "discard_audio" not in columns:
        _add_column(
            engine,
            "capture_settings",
            "discard_audio BOOLEAN NOT NULL DEFAULT 0",
            "discard_audio",
        )
    if "share_usage" not in columns:
        _add_column(engine, "capture_settings", "share_usage BOOLEAN NOT NULL DEFAULT 1", "share_usage")
    if "usage_device_id" not in columns:
        _add_column(engine, "capture_settings", "usage_device_id VARCHAR", "usage_device_id")
    if "usage_sent_through" not in columns:
        _add_column(engine, "capture_settings", "usage_sent_through VARCHAR", "usage_sent_through")
    if "shared_adapters" not in columns:
        _add_column(engine, "capture_settings", "shared_adapters BOOLEAN NOT NULL DEFAULT 1", "shared_adapters")
