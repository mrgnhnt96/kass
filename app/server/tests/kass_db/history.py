"""Write history.json: kass.db as each older Kass created it, and what the
Python worker's last migrations made of it, for `tests/kass_db.rs`.

Python created and migrated kass.db until the Rust server took that over
(`src/db.rs`). This records what Python did, so the Rust version can be
checked against it without Python:

- ``current``: the schema SQLAlchemy's ``create_all`` made on a new install.
- ``versions``: for every release tag and every commit that changed
  ``backend/database/models.py``, the schema that version created on a new
  install, two rows in each table, and the schema and rows after the last
  Python migrations (``run_migrations`` then ``create_all``) opened it. A
  migrated schema entry that is the same as a new install's is its name.

Both the migrations and the models are read from git at ``LAST_PYTHON``, the
last commit where Python migrated kass.db, so this gives the same file
whatever the working tree holds. The worker no longer uses SQLAlchemy, so
install it first. Run from the repo root:

    backend/venv/bin/pip install "sqlalchemy>=2.0"
    backend/venv/bin/python app/server/tests/kass_db/history.py
"""

import ast
import json
import sqlite3
import subprocess
import sys
import tempfile
import types
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT))

from sqlalchemy import create_engine  # noqa: E402

# The last commit where the Python worker created and migrated kass.db.
LAST_PYTHON = "19662139"
MODELS = "backend/database/models.py"
OUT = Path(__file__).with_name("history.json")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def versions() -> list[tuple[str, str]]:
    """(revision, label), oldest first: release tags and models.py commits."""
    found = {}
    for tag in git("tag").split():
        if tag.startswith("v") and git("ls-tree", "--name-only", tag, MODELS).strip():
            found[git("rev-parse", f"{tag}^{{commit}}").strip()] = tag
    log = git("log", "--format=%H %s", LAST_PYTHON, "--", MODELS)
    for line in log.splitlines():
        sha, subject = line.split(" ", 1)
        found.setdefault(sha, subject)
    dated = [(int(git("show", "-s", "--format=%ct", sha).strip()), sha[:8], label) for sha, label in found.items()]
    return [(sha, label) for _, sha, label in sorted(dated)]


def _stub(*_args, **_kwargs):
    return []


def load_models(revision: str):
    """The declarative Base of models.py at `revision`, its own imports stubbed.

    Package imports (``from ..services import ...``) only give Python-side
    defaults, which never reach the schema, so each name they import is a
    function returning an empty list.
    """
    source = git("show", f"{revision}:{MODELS}")
    tree = ast.parse(source)
    body = []
    for node in tree.body:
        if isinstance(node, ast.ImportFrom) and node.level > 0:
            for alias in node.names:
                body.append(
                    ast.copy_location(
                        ast.Assign(
                            targets=[ast.Name(id=alias.asname or alias.name, ctx=ast.Store())],
                            value=ast.Name(id="_stub", ctx=ast.Load()),
                        ),
                        node,
                    )
                )
        else:
            body.append(node)
    tree.body = body
    module = types.ModuleType(f"models_{revision}")
    module._stub = _stub
    exec(compile(ast.fix_missing_locations(tree), f"{revision}:{MODELS}", "exec"), module.__dict__)
    return module.Base


def load_migrations():
    """backend/database/migrations.py at LAST_PYTHON, in the backend package."""
    import backend.database  # noqa: F401 -- the package its relative imports resolve in

    source = git("show", f"{LAST_PYTHON}:backend/database/migrations.py")
    module = types.ModuleType("backend.database._last_python_migrations")
    module.__package__ = "backend.database"
    exec(compile(source, "migrations.py", "exec"), module.__dict__)
    return module.run_migrations


def schema(conn: sqlite3.Connection) -> list[dict]:
    rows = conn.execute("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name").fetchall()
    return [{"type": t, "name": n, "table": tb, "sql": s} for t, n, tb, s in rows]


def rows(conn: sqlite3.Connection) -> dict:
    tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")]
    out = {}
    for table in tables:
        cursor = conn.execute(f'SELECT * FROM "{table}" ORDER BY rowid')
        out[table] = {
            "columns": [d[0] for d in cursor.description],
            "rows": [list(r) for r in cursor.fetchall()],
        }
    return out


# Kass's old bundle ids, which the migrations rename to com.mrgnhnt.kass.
OLD_BUNDLE = "com.mrgnhnt.herga"
BUNDLE = "com.mrgnhnt.kass"


def value(table: str, column: str, kind: str, n: int):
    """A value for a column of row `n`: row 1 is under Kass's old bundle id and
    row 2 its new one, so the rename meets a row already there."""
    bundle = OLD_BUNDLE if n == 1 else BUNDLE
    kind = kind.upper()
    if column in ("app_bundle_id", "bundle_id", "scope_id"):
        return bundle
    if column == "scope":
        return "app"
    if column == "key":
        return "zed"
    if kind.startswith("INT"):
        return n
    if kind.startswith(("BOOL",)):
        return 1
    if kind.startswith(("FLOAT", "REAL", "NUMERIC")):
        return 0.25 * n
    if kind.startswith("DATETIME"):
        return f"2026-01-0{n} 03:04:05.000000"
    if kind.startswith("JSON"):
        return '["MetaRight"]'
    if kind.startswith("BLOB"):
        return bytes([n])
    return f"{table}.{column}.{n}"


def inserts(conn: sqlite3.Connection) -> list[str]:
    """Two rows in every table, every column given, as SQL."""
    statements = []
    tables = [r[0] for r in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")]
    for table in tables:
        columns = conn.execute(f'PRAGMA table_info("{table}")').fetchall()
        for n in (1, 2):
            names = ", ".join(f'"{c[1]}"' for c in columns)
            values = ", ".join(literal(value(table, c[1], c[2], n)) for c in columns)
            statements.append(f'INSERT INTO "{table}" ({names}) VALUES ({values})')
    return statements


def literal(v) -> str:
    if isinstance(v, bytes):
        return "X'" + v.hex() + "'"
    if isinstance(v, (int, float)):
        return repr(v)
    return "'" + v.replace("'", "''") + "'"


def main() -> None:
    run_migrations = load_migrations()
    current_base = load_models(LAST_PYTHON)
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "current.db"
        current_base.metadata.create_all(create_engine(f"sqlite:///{path}"))
        with sqlite3.connect(path) as conn:
            current = schema(conn)

        kept = {entry["table"] for entry in current}
        out_versions = []
        seen = set()
        for revision, label in versions():
            path = Path(tmp) / f"{revision}.db"
            engine = create_engine(f"sqlite:///{path}")
            load_models(revision).metadata.create_all(engine)
            engine.dispose()
            with sqlite3.connect(path) as conn:
                created = schema(conn)
                # Tables of removed features stay as they are, so only the
                # ones Kass has now tell versions apart.
                key = json.dumps([entry for entry in created if entry["table"] in kept])
                if key in seen:
                    continue
                seen.add(key)
                statements = inserts(conn)
                for statement in statements:
                    conn.execute(statement)
            conn.close()

            engine = create_engine(f"sqlite:///{path}")
            run_migrations(engine)
            current_base.metadata.create_all(bind=engine)
            engine.dispose()
            with sqlite3.connect(path) as conn:
                # An entry the same as a new install's is its name only.
                migrated = {
                    "schema": [entry["name"] if entry in current else entry for entry in schema(conn)],
                    "rows": rows(conn),
                }
            conn.close()
            out_versions.append(
                {"revision": revision, "label": label, "schema": created, "inserts": statements, "migrated": migrated}
            )
            print(f"{revision} {label}", file=sys.stderr)

    # One version per line, so a change shows which versions it touched.
    lines = ",\n".join(json.dumps(version) for version in out_versions)
    OUT.write_text(
        f'{{"last_python": {json.dumps(LAST_PYTHON)},\n"current": {json.dumps(current)},\n"versions": [\n{lines}\n]}}\n'
    )
    print(f"{len(out_versions)} versions -> {OUT.relative_to(ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
