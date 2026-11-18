"""Named writing styles and the apps assigned to them (docs/plans/PER_APP_STYLE.md).

A style holds the settings that shape how dictation is written: punctuation,
technical terms, and the user's own description of how they write in its
apps (docs/plans/TEACH_BY_REPLYING.md).
Its calibration, habits, examples and rules are kept per style by
``writing_style``, ``personal_examples`` and ``correction_notes``. An app the
user hasn't assigned uses the default style.

A correction teaches the style its app is in, unless the user left it behind
when the app moved (``Capture.teaches_style_id``, ``correction_style``).

A dictation that opens by asking for a style by name ("use formal mode",
"make this more formal", "I want this to be personal") is written in that
style instead of its app's (``spoken_style``), and its corrections teach that
style.

Dictation reads styles on every take, so the rows are kept in memory as a
``Snapshot`` and reloaded only after a change.
"""

import re
import threading
import time
from dataclasses import dataclass, field

from sqlalchemy.orm import Session

from ..database.models import AppStyle, WritingStyle
from .refinement import PUNCTUATION_STYLES, RefinementFlags

MAX_NAME_CHARS = 40
# Each style keeps its own prompt cache in the cleanup model
# (qwen_llm_backend.MAX_PROMPT_CACHES), a few hundred MB on 4B.
MAX_STYLES = 6

# The one style a new install starts with, and the default. Existing global
# settings and learned data move into it (docs/plans/PER_APP_STYLE.md, Migration).
MIGRATED_STYLE = "personal"
MIGRATED_NAME = "Personal"

# Kass's own window. Dictation there follows the style being taught while
# a teach session is open (docs/plans/TEACH_BY_REPLYING.md).
KASS_BUNDLE = "com.mrgnhnt.kass"

SETTINGS = ("punctuation_style", "preserve_technical")
MAX_DESCRIPTION_CHARS = 600


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
        """The style ``bundle_id`` is assigned to, or the default.

        Kass's own window uses the style being taught, while one is.
        """
        if bundle_id == KASS_BUNDLE and (taught := self.get(teaching_style())):
            return taught
        return self.get(self.apps.get(bundle_id or "")) or self.default

    def resolve(self, style_id: str | None) -> Style:
        """``style_id``'s style, or the default for None or a deleted style."""
        return self.get(style_id) or self.default


_lock = threading.RLock()
_snapshot: Snapshot | None = None
# (style id, monotonic deadline) while a teach session is open.
_teaching: tuple[str, float] | None = None


def teach_in_kass(style_id: str | None, ttl_seconds: float) -> None:
    """Clean up dictation in Kass's window in ``style_id`` for the next
    ``ttl_seconds``; None stops."""
    global _teaching
    with _lock:
        _teaching = (style_id, time.monotonic() + ttl_seconds) if style_id else None


def teaching_style() -> str | None:
    with _lock:
        if _teaching and time.monotonic() < _teaching[1]:
            return _teaching[0]
        return None


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
    """The styles and assignments, read from the database once per change."""
    global _snapshot
    with _lock:
        if _snapshot is None:
            from ..database import session as database_session

            if database_session.SessionLocal is None:
                return Snapshot((_FALLBACK,))
            with database_session.SessionLocal() as db:
                _snapshot = load(db)
        return _snapshot


def invalidate() -> None:
    global _snapshot
    with _lock:
        _snapshot = None


def _changed(examples: bool = False) -> None:
    """After a change: reload the snapshot, and regroup examples when apps moved."""
    invalidate()
    if examples:
        from . import personal_examples

        personal_examples.invalidate()


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


def flags_for_app(bundle_id: str | None, settings) -> RefinementFlags:
    return flags_for(snapshot().for_app(bundle_id), settings)


def correction_style(styles: Snapshot, app_bundle_id: str | None, teaches_style_id: str | None) -> str:
    """The style a capture's corrections teach: the one they were left with,
    while it exists, else the style of the app."""
    left = styles.get(teaches_style_id)
    return left.id if left else styles.for_app(app_bundle_id).id


# Words that ask for the same register: "make this professional" picks a
# style called Formal, "more informal" one called Casual.
_SYNONYMS = (
    frozenset({"formal", "professional"}),
    frozenset({"casual", "informal", "relaxed"}),
)
_FILLER = r"(?:(?:um|uh|okay|ok|so|please)\W+)*"
_MORE = r"(?:a\W+)?(?:little\W+)?(?:bit\W+)?(?:more\W+)?"
_MODE = r"(?:\W+(?:mode|style))"
# Ways to ask for a style at the start of a dictation, and whether a sentence
# break must follow. "Use formal mode" is never text, so it needs none; the
# rest could open a real sentence ("formal mode is off") unless they stand
# alone.
_LEAD_INS = (
    (r"use\W+(?:the\W+)?{name}" + _MODE + r"\b", False),
    (r"(?:switch|change)\W+(?:over\W+)?to\W+(?:the\W+)?{name}" + _MODE + "?", True),
    (r"(?:in\W+)?{name}" + _MODE, True),
    (r"(?:let'?s\W+)?(?:make|write)\W+(?:this|it)\W+(?:sound\W+)?" + _MORE + "{name}" + _MODE + "?", True),
    (r"i\W*(?:want|would\W+like|d\W+like)\W+(?:this|it)\W+to\W+(?:be|sound)\W+" + _MORE + "{name}" + _MODE + "?", True),
)
_BREAK = r"(?=\s*(?:[.,:;!?\u2026]|$))"
_AFTER = r"[\s.,:;!?\-\u2013\u2014\u2026]*"


def _spoken_names(name: str) -> str | None:
    """A regex for ``name`` as Whisper may write it, with its synonyms. A name
    that already ends in "mode" or "style" isn't said twice."""
    words = [word.casefold() for word in re.findall(r"\w+", name)]
    if len(words) > 1 and words[-1] in ("mode", "style"):
        words = words[:-1]
    if not words:
        return None
    variants = []
    for word in words:
        alike = next((group for group in _SYNONYMS if word in group), {word})
        variants.append("(?:" + "|".join(re.escape(each) for each in sorted(alike)) + ")")
    return r"\W+".join(variants)


def _spoken_pattern(name: str) -> re.Pattern | None:
    said = _spoken_names(name)
    if said is None:
        return None
    lead_ins = "|".join(
        f"(?:{lead_in.format(name=said)}{_BREAK if needs_break else ''})" for lead_in, needs_break in _LEAD_INS
    )
    return re.compile(rf"^\W*{_FILLER}(?:{lead_ins}){_AFTER}", re.IGNORECASE)


def spoken_style(text: str, styles: Snapshot) -> tuple[Style | None, str]:
    """The style ``text`` asks for in its opening words ("use formal mode",
    "make this more formal", "I want this to be personal"...), and the rest
    of ``text``; else None and ``text`` unchanged.

    Longer names are tried first, so "use work email mode" isn't taken as
    a style called Work.
    """
    for style in sorted(styles.styles, key=lambda s: -len(s.name)):
        pattern = _spoken_pattern(style.name)
        if pattern and (found := pattern.match(text)):
            rest = text[found.end() :]
            # "Use formal mode, dear team" starts the text at "dear".
            first = rest.split(maxsplit=1)[0] if rest.strip() else ""
            if first.islower():
                rest = rest[0].upper() + rest[1:]
            return style, rest
    return None, text


# --- Setup -----------------------------------------------------------------------


def ensure_styles(db: Session) -> None:
    """Create the Personal style on first run, with the global settings in it.

    Also repairs a database left without exactly one default. Idempotent.
    """
    rows = db.query(WritingStyle).order_by(WritingStyle.position).all()
    if not rows:
        from ..database.models import CaptureSettings

        saved = db.query(CaptureSettings).first()
        # Dictation keeps working exactly as it did before styles.
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
    elif sum(row.is_default for row in rows) != 1:
        keep = next((row for row in rows if row.is_default), rows[0])
        for row in rows:
            row.is_default = row is keep
        db.commit()
    invalidate()


# --- Changes ----------------------------------------------------------------------


def _clean_name(db: Session, name: object, exclude: str | None = None) -> str:
    if not isinstance(name, str) or not name.strip():
        raise ValueError("Give the style a name")
    cleaned = " ".join(name.split())[:MAX_NAME_CHARS]
    taken = db.query(WritingStyle).filter(WritingStyle.id != (exclude or "")).all()
    if any(row.name.casefold() == cleaned.casefold() for row in taken):
        raise ValueError(f"There's already a style called {cleaned}")
    return cleaned


def _clean_description(text: object) -> str:
    if not isinstance(text, str):
        raise ValueError("The description must be text")
    # Keep the user's line breaks; trim trailing spaces on each line.
    cleaned = "\n".join(line.rstrip() for line in text.strip().splitlines())
    if len(cleaned) > MAX_DESCRIPTION_CHARS:
        raise ValueError(f"Keep the description under {MAX_DESCRIPTION_CHARS} characters")
    return cleaned


def create_style(db: Session, name: str) -> Style:
    """A new style, starting from the default's settings with nothing learned."""
    if db.query(WritingStyle).count() >= MAX_STYLES:
        raise ValueError(f"Kass keeps at most {MAX_STYLES} styles, one cached prompt each")
    base = snapshot().default
    last = db.query(WritingStyle).order_by(WritingStyle.position.desc()).first()
    row = WritingStyle(
        name=_clean_name(db, name),
        position=(last.position + 1) if last else 0,
        is_default=False,
        **{setting: getattr(base, setting) for setting in SETTINGS},
    )
    db.add(row)
    db.commit()
    db.refresh(row)
    _changed()
    return _style(row)


def update_style(db: Session, style_id: str, patch: dict) -> Style | None:
    """Rename a style, change its settings or description, or make it the default."""
    row = db.get(WritingStyle, style_id)
    if row is None:
        return None
    if patch.get("name") is not None:
        row.name = _clean_name(db, patch["name"], exclude=style_id)
    if patch.get("punctuation_style") is not None:
        if patch["punctuation_style"] not in PUNCTUATION_STYLES:
            raise ValueError("Unknown punctuation style")
        row.punctuation_style = patch["punctuation_style"]
    if patch.get("preserve_technical") is not None:
        row.preserve_technical = bool(patch["preserve_technical"])
    if patch.get("description") is not None:
        row.description = _clean_description(patch["description"])
    if patch.get("is_default"):
        db.query(WritingStyle).filter(WritingStyle.id != style_id).update({"is_default": False})
        row.is_default = True
    db.commit()
    db.refresh(row)
    # Unassigned apps follow the default, and their corrections with them.
    _changed(examples=bool(patch.get("is_default")))
    return _style(row)


def delete_style(db: Session, style_id: str) -> bool:
    """Delete a style and what it learned; its apps move to the default."""
    row = db.get(WritingStyle, style_id)
    if row is None:
        return False
    if row.is_default:
        raise ValueError("Make another style the default before deleting this one")
    db.query(AppStyle).filter(AppStyle.style_id == style_id).delete()
    db.delete(row)
    db.commit()
    _changed(examples=True)
    from . import dictionary, teach

    dictionary.move_style(db, style_id, default_id())
    # What the style learned is forgotten by the Rust server (learned.rs).
    teach.forget_style(style_id)
    return True


def app_corrections(db: Session) -> dict[str, int]:
    """Per app, the captures whose corrections teach the style the app is in now."""
    from sqlalchemy import func

    from ..database.models import Capture, CaptureFeedback

    styles = snapshot()
    # A correction whose capture history retention deleted keeps its own copy.
    rows = (
        db.query(
            CaptureFeedback.capture_id,
            func.coalesce(Capture.app_bundle_id, CaptureFeedback.app_bundle_id),
            func.coalesce(Capture.teaches_style_id, CaptureFeedback.teaches_style_id),
        )
        .outerjoin(Capture, Capture.id == CaptureFeedback.capture_id)
        .filter(CaptureFeedback.target == "refined", CaptureFeedback.teaches_style())
        .distinct()
        .all()
    )
    counts: dict[str, int] = {}
    for _, bundle_id, teaches in rows:
        if bundle_id is None:
            continue
        if correction_style(styles, bundle_id, teaches) == styles.for_app(bundle_id).id:
            counts[bundle_id] = counts.get(bundle_id, 0) + 1
    return counts


def assign_app(db: Session, bundle_id: str, app_name: str | None, style_id: str, corrections: str = "bring") -> None:
    """Put ``bundle_id`` in ``style_id``; this also confirms a new app's style.

    ``corrections`` says what happens to the corrections the app's captures
    teach its current style: "bring" moves them to the new style with the
    app, "leave" keeps them teaching the current one. Examples, habits and
    rules follow the choice; the Rust server recounts the habits.
    """
    from ..database.models import Capture, CaptureFeedback

    if db.get(WritingStyle, style_id) is None:
        raise KeyError(style_id)
    if corrections not in ("bring", "leave"):
        raise ValueError("corrections must be bring or leave")
    current = snapshot().for_app(bundle_id).id
    if current != style_id:
        # Corrections whose capture history retention deleted carry their own app and style.
        for model in (Capture, CaptureFeedback):
            rows = db.query(model).filter(model.app_bundle_id == bundle_id)
            if corrections == "leave":
                rows.filter(model.teaches_style_id.is_(None)).update(
                    {"teaches_style_id": current}, synchronize_session=False
                )
            else:
                rows.filter(model.teaches_style_id == current).update(
                    {"teaches_style_id": None}, synchronize_session=False
                )
    row = db.get(AppStyle, bundle_id)
    if row is None:
        db.add(AppStyle(bundle_id=bundle_id, app_name=app_name, style_id=style_id))
    else:
        row.style_id = style_id
        row.app_name = app_name or row.app_name
    db.commit()
    _changed(examples=True)


def confirm_apps(db: Session, apps: list[tuple[str, str | None]]) -> None:
    """Keep each new app in the style it already uses, confirming it.

    One commit for the lot, so confirming many apps at once doesn't recount
    the habits per app. Apps that already have a style are left alone.
    """
    current = snapshot()
    added = False
    for bundle_id, app_name in apps:
        if db.get(AppStyle, bundle_id) is None:
            db.add(AppStyle(bundle_id=bundle_id, app_name=app_name, style_id=current.for_app(bundle_id).id))
            added = True
    if added:
        db.commit()
        _changed()
