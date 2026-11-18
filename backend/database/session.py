"""Engine creation, initialization, and session management.

The app creates kass.db and brings an older one up to date before it starts
this worker (tauri/src-tauri/server/src/db/schema.rs); in development,
``cargo xtask db-init`` does. This only uses the tables it finds.
"""

import logging

from sqlalchemy import create_engine, inspect
from sqlalchemy.orm import sessionmaker

from .. import config
from .models import Base

logger = logging.getLogger(__name__)

# Initialized by init_db()
engine = None
SessionLocal = None
_db_path = None


class MissingSchemaError(RuntimeError):
    """kass.db lacks tables the models use: the app hasn't prepared it."""


def init_db() -> None:
    """Open kass.db, which the app has created and migrated."""
    global engine, SessionLocal, _db_path

    _db_path = config.get_db_path()

    engine = create_engine(
        f"sqlite:///{_db_path}",
        connect_args={"check_same_thread": False},
        # A failed statement's error otherwise lists its values, which hold
        # what the user dictated, and the logs are what users send us.
        hide_parameters=True,
    )
    check_schema(engine, _db_path)

    SessionLocal = sessionmaker(autocommit=False, autoflush=False, bind=engine)

    # Apps on other Macs hear about capture changes from the server (beta `network`).
    from ..services import capture_events

    capture_events.install()
    # Each capture records the Mac that dictated it.
    from ..utils import macs

    macs.install()

    # The preset writing styles, with the global settings moved into the default.
    from ..services.dictionary import ensure_defaults
    from ..services.styles import ensure_styles

    with SessionLocal() as db:
        ensure_styles(db)
        ensure_defaults(db, _db_path.with_name("dictionary-defaulted"))

    # From here on, a stand-in for a shared Kass records what it changes,
    # to send there (services/sync).
    from ..services.sync import changes

    changes.install()


def check_schema(engine, db_path) -> None:
    """Fail clearly when kass.db lacks tables or columns the models use."""
    inspector = inspect(engine)
    tables = set(inspector.get_table_names())
    missing = []
    for table in Base.metadata.sorted_tables:
        if table.name not in tables:
            missing.append(table.name)
            continue
        columns = {column["name"] for column in inspector.get_columns(table.name)}
        missing += [f"{table.name}.{c.name}" for c in table.columns if c.name not in columns]
    if missing:
        raise MissingSchemaError(
            f"{db_path} is missing {', '.join(missing)}. The app creates and migrates kass.db"
            " before it starts the worker; for a worker started by hand, run cargo xtask db-init first."
        )


def get_db():
    """Yield a database session (FastAPI dependency)."""
    db = SessionLocal()
    try:
        yield db
    finally:
        db.close()
