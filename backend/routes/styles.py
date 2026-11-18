"""What the Rust server needs from the worker for writing styles (docs/plans/PER_APP_STYLE.md).

The ``/writing-styles`` routes and the style and app rows in kass.db live in
the Rust server. The worker keeps reading those rows for dictation and hears
from the server what to reload.
"""

import logging

from fastapi import APIRouter, Depends
from pydantic import BaseModel
from sqlalchemy.orm import Session

from ..database import get_db
from ..services import styles as styles_service

router = APIRouter(prefix="/worker/styles", tags=["worker"])
logger = logging.getLogger(__name__)


class StylesChanged(BaseModel):
    # Apps moved or the default changed: regroup the examples by style.
    examples: bool = False
    # A style that was deleted: end its teach sessions. What it learned is
    # forgotten by the Rust server, which owns the learned files.
    deleted: str | None = None


@router.post("/changed")
def changed(request: StylesChanged):
    styles_service.invalidate()
    if request.examples:
        from ..services import personal_examples

        personal_examples.invalidate()
    if request.deleted:
        from ..services import teach

        teach.forget_style(request.deleted)
    return {"ok": True}


def _cache_mb(db: Session) -> int | None:
    """What one style's cached prompt takes in the cleanup model, in MB: the
    largest current style's, since a style's prompt grows as it learns."""
    from ..services.refinement import style_cache_bytes
    from ..services.settings import get_capture_settings

    saved = get_capture_settings(db)
    sizes = []
    for style in styles_service.snapshot().styles:
        try:
            sizes.append(style_cache_bytes(styles_service.flags_for(style, saved), saved.llm_model))
        except Exception:
            logger.warning("Could not estimate a style's cache size", exc_info=True)
    sizes = [size for size in sizes if size]
    return round(max(sizes) / 1e6) if sizes else None


@router.post("/state")
def state(db: Session = Depends(get_db)):
    """What only the worker knows: each style's prompt cache size, and the
    style a teach session is open on, which Kass's own window uses meanwhile."""
    return {"cache_mb_per_style": _cache_mb(db), "teaching_style": styles_service.teaching_style()}
