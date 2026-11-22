"""The models read the tables the app makes (app/server/src/db/schema.rs).

The app's Rust server creates and migrates kass.db; this worker only uses it.
`app/server/tests/kass_db.rs` checks the Rust schema against the
one recorded in `kass_db/history.json`, and this checks the models still make
that same schema, so a model change without the matching Rust one fails here.
"""

import json
import sqlite3
from pathlib import Path

import pytest
from sqlalchemy import create_engine

from backend.database import session
from backend.database.models import Base

HISTORY = Path(__file__).resolve().parents[2] / "app/server/tests/kass_db/history.json"


def schema(path: Path) -> list[dict]:
    with sqlite3.connect(path) as conn:
        rows = conn.execute("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name").fetchall()
    return [{"type": t, "name": n, "table": tb, "sql": s} for t, n, tb, s in rows]


def test_the_models_make_the_schema_the_app_creates(tmp_path):
    path = tmp_path / "kass.db"
    engine = create_engine(f"sqlite:///{path}")
    Base.metadata.create_all(engine)
    engine.dispose()
    assert schema(path) == json.loads(HISTORY.read_text())["current"], (
        "models.py changed the schema: make the same change in app/server/src/db/schema.rs"
        ' (a new install\'s table, and the ALTER for an existing kass.db), and in the "current" entries'
        " of app/server/tests/kass_db/history.json, which both tests compare against"
    )


def test_the_worker_refuses_a_database_the_app_has_not_prepared(tmp_path):
    path = tmp_path / "kass.db"
    engine = create_engine(f"sqlite:///{path}")
    with pytest.raises(session.MissingSchemaError, match="cargo xtask db-init"):
        session.check_schema(engine, path)
    Base.metadata.create_all(engine)
    session.check_schema(engine, path)
