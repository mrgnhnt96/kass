"""Writing-style rows as the Rust server saves them (server/src/styles.rs), for tests."""

from backend.database.models import AppStyle, CaptureSettings, WritingStyle
from backend.services import personal_examples
from backend.services.styles import MIGRATED_NAME, MIGRATED_STYLE


def seed_styles(db) -> None:
    """The Personal default with the global settings in it, as db::seed makes it."""
    if db.query(WritingStyle).count():
        return
    saved = db.query(CaptureSettings).first()
    db.add(
        WritingStyle(
            id=MIGRATED_STYLE,
            name=MIGRATED_NAME,
            position=0,
            is_default=True,
            punctuation_style=(saved.punctuation_style if saved else None) or "standard",
            preserve_technical=saved.preserve_technical if saved else True,
        )
    )
    db.commit()


def add_style(db, name: str, **settings) -> str:
    row = WritingStyle(name=name, position=db.query(WritingStyle).count(), is_default=False, **settings)
    db.add(row)
    db.commit()
    return row.id


def put_app(db, bundle_id: str, style_id: str, app_name: str | None = None) -> None:
    row = db.get(AppStyle, bundle_id)
    if row is None:
        db.add(AppStyle(bundle_id=bundle_id, app_name=app_name, style_id=style_id))
    else:
        row.style_id = style_id
    db.commit()
    personal_examples.invalidate()


def drop_style(db, style_id: str) -> None:
    db.query(AppStyle).filter(AppStyle.style_id == style_id).delete()
    db.query(WritingStyle).filter(WritingStyle.id == style_id).delete()
    db.commit()
    personal_examples.invalidate()
