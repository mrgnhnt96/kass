"""Named writing styles and the apps assigned to them (docs/plans/PER_APP_STYLE.md).

A style holds the settings that shape how dictation is written: punctuation,
technical terms, and the user's own description of how they write in its
apps (docs/plans/TEACH_BY_REPLYING.md).
Its calibration, habits, examples and rules are kept per style by
``writing_style``, ``personal_examples`` and ``correction_notes``. An app the
user hasn't assigned uses the default style.

A correction teaches the style its app is in, unless the user left it behind
when the app moved (``Capture.teaches_style_id``, ``correction_style``).

The Rust server owns the rows and edits them; the worker reads them fresh
(``snapshot``) for training and the cleanup prompt.
"""

from dataclasses import dataclass, field

from sqlalchemy.orm import Session

from ..database.models import AppStyle, WritingStyle
from .refinement import PUNCTUATION_STYLES, RefinementFlags

# Each style keeps its own prompt cache in the cleanup model
# (qwen_llm_backend.MAX_PROMPT_CACHES), a few hundred MB on 4B.
MAX_STYLES = 6

# The one style a new install starts with, and the default. Existing global
# settings and learned data move into it (docs/plans/PER_APP_STYLE.md, Migration).
MIGRATED_STYLE = "personal"
MIGRATED_NAME = "Personal"


@dataclass(frozen=True)
class Style:
    id: str
    name: str
    position: int
    is_default: bool
    punctuation_style: str
    preserve_technical: bool
    # How the user says they write in the style's apps, for the cleanup prompt.
    description: str = ""


# Before the database exists (tests, tools), every app uses one Standard style.
_FALLBACK = Style(MIGRATED_STYLE, MIGRATED_NAME, 0, True, "standard", True)


@dataclass(frozen=True)
class Snapshot:
    styles: tuple[Style, ...]
    # Bundle id -> style id, for the apps the user assigned.
    apps: dict[str, str] = field(default_factory=dict)
    app_names: dict[str, str] = field(default_factory=dict)

    @property
    def default(self) -> Style:
        return next((s for s in self.styles if s.is_default), self.styles[0])

    def get(self, style_id: str | None) -> Style | None:
        return next((s for s in self.styles if s.id == style_id), None)

    def for_app(self, bundle_id: str | None) -> Style:
        """The style ``bundle_id`` is assigned to, or the default."""
        return self.get(self.apps.get(bundle_id or "")) or self.default

    def resolve(self, style_id: str | None) -> Style:
        """``style_id``'s style, or the default for None or a deleted style."""
        return self.get(style_id) or self.default


def _style(row: WritingStyle) -> Style:
    return Style(
        id=row.id,
        name=row.name,
        position=row.position,
        is_default=row.is_default,
        punctuation_style=row.punctuation_style if row.punctuation_style in PUNCTUATION_STYLES else "standard",
        preserve_technical=row.preserve_technical,
        description=row.description or "",
    )


def load(db: Session) -> Snapshot:
    rows = [_style(row) for row in db.query(WritingStyle).order_by(WritingStyle.position, WritingStyle.created_at)]
    if not rows:
        return Snapshot((_FALLBACK,))
    assigned = db.query(AppStyle).all()
    return Snapshot(
        tuple(rows),
        {row.bundle_id: row.style_id for row in assigned},
        {row.bundle_id: row.app_name for row in assigned if row.app_name},
    )


def snapshot() -> Snapshot:
    """The styles and assignments as the database has them now. Read each
    time: the Rust server edits them without telling the worker."""
    from ..database import session as database_session

    if database_session.SessionLocal is None:
        return Snapshot((_FALLBACK,))
    with database_session.SessionLocal() as db:
        return load(db)


def default_id() -> str:
    return snapshot().default.id


def flags_for(style: Style, settings) -> RefinementFlags:
    """The refinement flags for dictation in ``style``; self-correction stays global.

    Filler removal and a capitalized first word are always on: the base prompt
    removes fillers, and learned punctuation covers lowercase starts.
    """
    return RefinementFlags(
        self_correction=settings.self_correction,
        preserve_technical=style.preserve_technical,
        punctuation_style=style.punctuation_style,
        style=style.id,
    )


def correction_style(styles: Snapshot, app_bundle_id: str | None, teaches_style_id: str | None) -> str:
    """The style a capture's corrections teach: the one they were left with,
    while it exists, else the style of the app."""
    left = styles.get(teaches_style_id)
    return left.id if left else styles.for_app(app_bundle_id).id
