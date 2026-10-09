"""The user's dictionaries: words dictation should get right (docs/plans/DICTIONARIES.md).

An entry is a term ("Kubernetes"), which Whisper is prompted with and which
is written the user's way after cleanup, even where Whisper heard it a little
wrong ("Kubernetis", "cuber netes"), or a replacement, which writes ``written``
wherever ``spoken`` was said. A phrase is a replacement whose ``written`` is
text to insert exactly ("my email" → an address, a sign-off over two lines):
it is never a term, and nothing respells it. An entry with ``match_sound`` off is only
prompted and recased where it is spelled exactly: a name spelled aloud to fix
it ("Meghan") never respells another that sounds like it ("Megan"). Entries belong to every app ("global"), to a
writing style, or to one app; a dictation merges its app's, its style's and
the global ones, and the most specific wins.

Dictation never reads the database: entries are read once per change, and
each app's merged dictionary is kept until the next one.
"""

import logging
import re
import threading
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from datetime import datetime
from difflib import SequenceMatcher

logger = logging.getLogger(__name__)

SCOPES = ("app", "style", "global")
# Who added an entry: the user, or a word spelled aloud to fix it.
SOURCES = ("user", "spoken_fix")
MAX_ENTRIES = 1000
MAX_LENGTH = 200
# What a phrase writes: an address or a sign-off, longer than a word.
MAX_PHRASE_LENGTH = 1000
# Whisper keeps the last 223 tokens of its prompt; terms take at most this
# many, and the earlier text the rest.
PROMPT_TOKENS = 64

# Words, as correction_rules.py matches them.
_WORD = re.compile(r"\w+(?:['\u2019-]\w+)*", re.UNICODE)
# Between the words of a phrase: Whisper writes "voice box" and "voice-box".
_BETWEEN = r"[\s-]+"
# Spellings that sound the same, for matching what Whisper heard to a term.
_SOUNDS = (("ph", "f"), ("ck", "k"), ("q", "k"), ("c", "k"), ("z", "s"), ("y", "i"))
# How close one uncommon word must be to a term's letters to be that term
# misheard ("Kubernetis"): one letter in six may differ, so a five-letter
# name ("Helga") is not another ("Kass"). Below the shortest term that
# counts, only a word that sounds the same does ("Sagar" for "Saggar").
_HEARD_RATIO = 0.82
_HEARD_MIN = 5
_MEMO = 4096


class DuplicateEntryError(ValueError):
    """The scope already has an entry for the same word said."""


@dataclass(frozen=True)
class Entry:
    id: str
    scope: str
    scope_id: str | None
    app_name: str | None
    written: str
    spoken: str | None
    created_at: datetime | None
    # The entry this row is one place of (its own id when it applies in one).
    group_id: str | None = None
    # Off: never swapped in for a word that only sounds like it.
    match_sound: bool = True
    # Text inserted exactly where ``spoken`` is said; never a term.
    phrase: bool = False


@dataclass(frozen=True)
class Place:
    scope: str
    scope_id: str | None = None
    app_name: str | None = None


@dataclass(frozen=True)
class Group:
    """An entry as the user edits it: one word, in every place it applies."""

    id: str
    written: str
    spoken: str | None
    places: tuple[Place, ...]
    created_at: datetime | None
    match_sound: bool = True
    source: str = "user"
    phrase: bool = False


def _key(written: str, spoken: str | None) -> str:
    return " ".join((spoken or written).split()).casefold()


def _phrase(text: str) -> str:
    """``text`` as a pattern that also matches it with other spaces or hyphens between words."""
    return _BETWEEN.join(re.escape(part) for part in re.split(_BETWEEN, text.strip()))


def _bounded(alternatives: Iterable[str]) -> re.Pattern | None:
    # Longest first, so "voice box app" wins over "voice box" where both match.
    ordered = sorted(set(alternatives), key=len, reverse=True)
    if not ordered:
        return None
    # Grouped by first letter, so a position is turned down after one letter
    # instead of after trying every term: a flat list of hundreds of terms
    # cost milliseconds a take. Terms that start differently never match at
    # the same place, and each group keeps the longest-first order. Only an
    # ASCII letter or digit, which re.escape leaves alone and case folds
    # predictably, starts a group.
    groups: dict[str, list[str]] = {}
    rest: list[str] = []
    for pattern in ordered:
        first = pattern[0]
        if first.isascii() and first.isalnum():
            groups.setdefault(first.lower(), []).append(pattern[1:])
        else:
            rest.append(pattern)
    grouped = [re.escape(first) + "(?:" + "|".join(tails) + ")" for first, tails in groups.items()]
    return re.compile(r"(?<![\w'\u2019-])(?:" + "|".join([*grouped, *rest]) + r")(?![\w'\u2019-])", re.IGNORECASE)


def _normal(text: str) -> str:
    return " ".join(re.split(_BETWEEN, text.strip())).casefold()


def _sound(text: str) -> str:
    """``text``'s letters as they sound: no spaces or marks, one of each doubled letter.

    Digits are kept as they are: "7" and "77" are different words.
    """
    key = re.sub(r"[\W_]+", "", text.casefold())
    for spelled, sounds in _SOUNDS:
        key = key.replace(spelled, sounds)
    return re.sub(r"([^\W\d_])\1+", r"\1", key)


@dataclass
class _Heard:
    """The terms a transcript may have heard wrong, keyed by how they sound."""

    by_sound: dict[str, str]
    # Every term as written, lowercased: a word that is one is already right.
    exact: frozenset[str]
    # Terms long enough to match approximately, by first sound.
    by_start: dict[str, list[tuple[str, str]]]
    lengths: frozenset[int]
    words: int
    common: Callable[[str], bool]
    sounds: dict[str, str] = field(default_factory=dict)
    single: dict[str, str | None] = field(default_factory=dict)

    @classmethod
    def of(cls, terms: Iterable[str], common: Callable[[str], bool], exact: Iterable[str] = ()) -> "_Heard | None":
        """``exact``: more terms, only ever kept as they are, never matched to."""
        by_sound: dict[str, str] = {}
        by_start: dict[str, list[tuple[str, str]]] = {}
        terms = list(terms)
        words = 0
        for term in terms:
            key = _sound(term)
            if not key or key in by_sound:
                continue
            by_sound[key] = term
            words = max(words, len(_WORD.findall(term)))
            if len(key) >= _HEARD_MIN:
                by_start.setdefault(key[0], []).append((key, term))
        if not by_sound:
            return None
        exact = frozenset(_normal(term) for term in (*terms, *exact))
        # Whisper splits a word it doesn't know: "cuber netes".
        return cls(by_sound, exact, by_start, frozenset(len(key) for key in by_sound), words + 1, common)

    def sound(self, word: str) -> str:
        if (key := self.sounds.get(word)) is None:
            if len(self.sounds) >= _MEMO:
                self.sounds.clear()
            key = self.sounds[word] = _sound(word)
        return key

    def one(self, word: str) -> str | None:
        """The term one word is, heard wrong; never a common word, which the user likely said."""
        if word in self.single:
            return self.single[word]
        found = None
        key = self.sound(word)
        if key and word.casefold() not in self.exact and not self.common(word):
            found = self.by_sound.get(key)
            if found is None and len(key) >= _HEARD_MIN - 1:
                # The closest; the first, most specific, of equally close ones.
                best = _HEARD_RATIO
                for term_key, term in self.by_start.get(key[0], ()):
                    matcher = SequenceMatcher(None, key, term_key, autojunk=False)
                    if (
                        matcher.real_quick_ratio() >= best
                        and (ratio := matcher.ratio()) >= best
                        and (found is None or ratio > best)
                    ):
                        found, best = term, ratio
        if len(self.single) >= _MEMO:
            self.single.clear()
        self.single[word] = found
        return found

    def fix(self, text: str) -> str:
        found = list(_WORD.finditer(text))
        parts: list[str] = []
        last = i = 0
        while i < len(found):
            term, width = self.at(text, found, i)
            if term is not None:
                start, end = found[i].start(), found[i + width - 1].end()
                parts += [text[last:start], term]
                last, i = end, i + width
            else:
                i += 1
        return "".join([*parts, text[last:]]) if parts else text

    def at(self, text: str, found: list[re.Match], i: int) -> tuple[str | None, int]:
        """The term the words from ``found[i]`` are, longest first, and how many words."""
        key = ""
        joined: list[tuple[str, int]] = []
        for n in range(min(self.words, len(found) - i)):
            word = found[i + n]
            # Words of one phrase: only spaces between, and no "tail's scale".
            if n and text[found[i + n - 1].end() : word.start()].strip(" "):
                break
            if "'" in word.group() or "\u2019" in word.group():
                break
            part = self.sound(word.group())
            key += part[1:] if key and part and key[-1] == part[0] and part[0].isalpha() else part
            joined.append((key, n + 1))
        for key, width in reversed(joined):
            if width == 1:
                word = found[i].group()
                term = self.one(word)
                if term is not None and word != term:
                    return term, 1
            elif (
                len(key) in self.lengths
                and (term := self.by_sound.get(key)) is not None
                and _normal(text[found[i].start() : found[i + width - 1].end()]) not in self.exact
            ):
                return term, width
        return None, 0


@dataclass
class Dictionary:
    """One app's merged dictionary, as dictation uses it."""

    # Whisper prompt candidates, most specific first: terms, then what replacements write.
    terms: tuple[str, ...] = ()
    # Capitalized terms, which dictation keeps capitalized mid-sentence.
    names: frozenset[str] = frozenset()
    replacements: dict[str, str] = field(default_factory=dict)
    # Terms whose case is fixed after cleanup, by lowercased form.
    spellings: dict[str, str] = field(default_factory=dict)
    # Of those, the ones never matched by sound, by lowercased form.
    exact: frozenset[str] = frozenset()
    # The most words a match covers, which provisional text holds back.
    span: int = 0
    _compiled: tuple | None = field(default=None, repr=False)

    def _patterns(self):
        # Built on first use, after dictation has loaded the word data
        # _common_word reads, never while a take starts.
        if self._compiled is None:
            from .phrase_seams import _common_word

            recased = {
                key: term
                for key, term in self.spellings.items()
                if not all(_common_word(word) for word in _WORD.findall(term))
            }
            self._compiled = (
                _bounded(_phrase(spoken) for spoken in self.replacements),
                _bounded(_phrase(term) for term in recased.values()),
                recased,
                # Every term: only the heard word must not be a common one.
                _Heard.of(
                    (term for key, term in self.spellings.items() if key not in self.exact),
                    _common_word,
                    (self.spellings[key] for key in self.exact),
                ),
            )
        return self._compiled

    def apply(self, text: str) -> str:
        """Replacements, then terms written the user's way.

        What a replacement wrote is left as written: no term respells it.
        A term is fixed where Whisper wrote it in other capitals, and where
        it heard it a little wrong: an uncommon word spelled or sounding
        close ("Kubernetis"), or the term split into words ("cuber netes").
        A term that is a common word ("Mark", "Slack") keeps whatever case
        the text gave it: it may be the word ("mark this") and not the name.
        A common word is never taken for a term, and a term with
        ``match_sound`` off is only ever recased, never matched by sound.
        """
        if not text or not (self.replacements or self.spellings):
            return text
        replace, recase, recased, heard = self._patterns()

        def fix(part: str) -> str:
            if recase is not None:
                part = recase.sub(lambda m: recased.get(_normal(m.group()), m.group()), part)
            if heard is not None:
                part = heard.fix(part)
            return part

        if replace is None:
            return fix(text)
        parts: list[str] = []
        last = 0
        for match in replace.finditer(text):
            written = self.replacements.get(_normal(match.group()))
            if written is None:
                continue
            parts += [fix(text[last : match.start()]), written]
            last = match.end()
        return "".join([*parts, fix(text[last:])]) if parts else fix(text)


EMPTY = Dictionary()


def resolve(entries: Iterable[Entry], bundle_id: str | None, style_id: str | None) -> list[tuple[Entry, bool]]:
    """The entries a dictation in ``bundle_id`` uses, most specific first, and
    whether a more specific one overrides each (the same word said)."""
    rank = {"app": 0, "style": 1, "global": 2}
    applies = [
        entry
        for entry in entries
        if entry.scope == "global"
        or (entry.scope == "style" and style_id and entry.scope_id == style_id)
        or (entry.scope == "app" and bundle_id and entry.scope_id == bundle_id)
    ]
    # Newest first within a scope: a word just added is likely the one in use.
    applies.sort(key=lambda e: e.created_at or datetime.min, reverse=True)
    applies.sort(key=lambda e: rank[e.scope])
    seen: set[str] = set()
    resolved = []
    for entry in applies:
        key = _key(entry.written, entry.spoken)
        resolved.append((entry, key in seen))
        seen.add(key)
    return resolved


def build(resolved: list[tuple[Entry, bool]], exact_spelling: bool = True) -> Dictionary:
    """The dictionary for ``resolved``. Without ``exact_spelling``, every
    term is matched by sound."""
    active = [entry for entry, overridden in resolved if not overridden]
    if not active:
        return EMPTY
    terms: list[str] = []
    spellings: dict[str, str] = {}
    exact: set[str] = set()
    replacements: dict[str, str] = {}
    for entry in active:
        if entry.spoken:
            replacements.setdefault(_normal(entry.spoken), entry.written)
    # Terms before what replacements write, which Whisper needs less: the
    # replacement fixes those whatever Whisper hears. What a phrase writes is
    # text to insert, not a word Whisper hears or a spelling to fix.
    for entry in sorted((e for e in active if not e.phrase), key=lambda e: bool(e.spoken)):
        written = entry.written.strip()
        if _normal(written) in spellings:
            continue
        spellings[_normal(written)] = written
        # The most specific entry that writes it decides.
        if exact_spelling and not entry.match_sound:
            exact.add(_normal(written))
        terms.append(written)
    names = frozenset(word for term in terms for word in _WORD.findall(term) if word[:1].isupper())
    # A term may be heard as one word more than it has ("cuber netes").
    span = max(
        [
            *(len(re.split(_BETWEEN, text)) for text in replacements),
            *(len(re.split(_BETWEEN, text)) + 1 for text in spellings),
        ],
        default=0,
    )
    return Dictionary(tuple(terms), names, replacements, spellings, frozenset(exact), span)


def fit_terms(
    terms: Iterable[str], count: Callable[[str], int], budget: int | None = None
) -> tuple[list[str], list[str]]:
    """The terms that fit Whisper's prompt in order, and the rest.

    ``count`` is the tokens of " <term>"; each term also takes a separator.
    The first that does not fit ends the list, so a lower scope never
    displaces a higher one.
    """
    budget = PROMPT_TOKENS if budget is None else budget
    fit, dropped, used = [], [], 1
    for term in terms:
        cost = count(term) + 1
        if dropped or used + cost > budget:
            dropped.append(term)
            continue
        fit.append(term)
        used += cost
    return fit, dropped


def prompt(terms: Iterable[str]) -> str:
    """The terms as Whisper reads them: a plain list that ends a sentence."""
    terms = list(terms)
    return ", ".join(terms) + "." if terms else ""


# -- the entries, read once per change --------------------------------------

_lock = threading.Lock()
_entries: tuple[Entry, ...] | None = None
_by_app: dict[tuple[str, str, bool], Dictionary] = {}


def _entry(row) -> Entry:
    return Entry(
        id=row.id,
        scope=row.scope,
        scope_id=row.scope_id or None,
        app_name=row.app_name,
        written=row.written,
        spoken=row.spoken,
        created_at=row.created_at,
        group_id=row.group_id or row.id,
        match_sound=row.match_sound is not False,
        phrase=bool(row.phrase),
    )


def entries() -> tuple[Entry, ...]:
    global _entries
    with _lock:
        if _entries is not None:
            return _entries
    from ..database import session as database_session
    from ..database.models import DictionaryEntry

    if database_session.SessionLocal is None:
        return ()
    with database_session.SessionLocal() as db:
        loaded = tuple(_entry(row) for row in db.query(DictionaryEntry).all())
    with _lock:
        _entries = loaded
    return loaded


def invalidate() -> None:
    global _entries
    with _lock:
        _entries = None
        _by_app.clear()


def resolved_for(bundle_id: str | None) -> list[tuple[Entry, bool]]:
    from .styles import snapshot as styles_snapshot

    return resolve(entries(), bundle_id, styles_snapshot().for_app(bundle_id).id)


def for_app(bundle_id: str | None, style_id: str | None = None) -> Dictionary:
    """The dictionary a dictation in ``bundle_id`` uses (None: an unknown app),
    written in ``style_id`` when one was asked for, else the app's style."""
    from .styles import snapshot as styles_snapshot

    style_id = style_id or styles_snapshot().for_app(bundle_id).id
    cache_key = (bundle_id or "", style_id)
    with _lock:
        cached = _by_app.get(cache_key)
    if cached is not None:
        return cached
    built = build(resolve(entries(), bundle_id, style_id))
    with _lock:
        _by_app[cache_key] = built
    return built


# -- editing -----------------------------------------------------------------


def _clean(text: object, what: str) -> str:
    if not isinstance(text, str) or not (cleaned := " ".join(text.split())):
        raise ValueError(f"{what} can't be empty")
    if len(cleaned) > MAX_LENGTH:
        raise ValueError(f"{what} can be at most {MAX_LENGTH} characters")
    return cleaned


def _clean_phrase_text(text: object) -> str:
    """What a phrase writes: its lines kept, each without stray spaces, and
    no blank lines at either end."""
    if not isinstance(text, str):
        raise ValueError("What to write can't be empty")
    lines = [" ".join(line.split()) for line in text.splitlines()]
    while lines and not lines[0]:
        lines.pop(0)
    while lines and not lines[-1]:
        lines.pop()
    if not (cleaned := "\n".join(lines)):
        raise ValueError("What to write can't be empty")
    if len(cleaned) > MAX_PHRASE_LENGTH:
        raise ValueError(f"What to write can be at most {MAX_PHRASE_LENGTH} characters")
    return cleaned


def _clean_written(text: object, phrase: bool) -> str:
    return _clean_phrase_text(text) if phrase else _clean(text, "What to write")


def _clean_spoken(spoken: object) -> str | None:
    if spoken is None or (isinstance(spoken, str) and not spoken.strip()):
        return None
    cleaned = _clean(spoken, "What you say")
    if not _WORD.search(cleaned):
        raise ValueError("What you say needs at least one word")
    return cleaned


def _place(place) -> Place:
    """A place from the API, checked: global, an existing style, or an app."""
    from .styles import snapshot as styles_snapshot

    if isinstance(place, Place):
        scope, scope_id, app_name = place.scope, place.scope_id, place.app_name
    else:
        scope, scope_id, app_name = place.get("scope"), place.get("scope_id"), place.get("app_name")
    if scope not in SCOPES:
        raise ValueError("Unknown dictionary scope")
    if scope == "global":
        return Place("global")
    if not isinstance(scope_id, str) or not scope_id.strip():
        raise ValueError("Choose the style or app this entry is for")
    scope_id = scope_id.strip()
    if scope == "style":
        if styles_snapshot().get(scope_id) is None:
            raise ValueError("That writing style doesn't exist")
        return Place("style", scope_id)
    return Place("app", scope_id, app_name if isinstance(app_name, str) and app_name.strip() else None)


def _places(places) -> list[Place]:
    """Checked, without repeats; everywhere replaces every other place."""
    cleaned: list[Place] = []
    for place in places or ():
        found = _place(place)
        if found.scope == "global":
            return [found]
        if all((p.scope, p.scope_id) != (found.scope, found.scope_id) for p in cleaned):
            cleaned.append(found)
    if not cleaned:
        raise ValueError("Choose where this word applies")
    return cleaned


def _place_name(place: Place) -> str:
    from .styles import snapshot as styles_snapshot

    if place.scope == "global":
        return "everywhere"
    if place.scope == "style":
        style = styles_snapshot().get(place.scope_id)
        return f"the {style.name} style" if style else "that style"
    return place.app_name or place.scope_id


def _check_unique(db, place: Place, key: str, group_id: str | None = None) -> None:
    from sqlalchemy import func

    from ..database.models import DictionaryEntry

    query = db.query(DictionaryEntry).filter(
        DictionaryEntry.scope == place.scope,
        DictionaryEntry.scope_id == (place.scope_id or ""),
        DictionaryEntry.key == key,
    )
    if group_id:
        query = query.filter(func.coalesce(DictionaryEntry.group_id, DictionaryEntry.id) != group_id)
    if query.first() is not None:
        raise DuplicateEntryError(f"That word is already in the dictionary for {_place_name(place)}")


def _rows(db, group_id: str):
    from sqlalchemy import func

    from ..database.models import DictionaryEntry

    return (
        db.query(DictionaryEntry)
        .filter(func.coalesce(DictionaryEntry.group_id, DictionaryEntry.id) == group_id)
        .order_by(DictionaryEntry.created_at)
        .all()
    )


def _group(rows) -> Group:
    first = rows[0]
    return Group(
        id=first.group_id or first.id,
        written=first.written,
        spoken=first.spoken,
        places=tuple(Place(row.scope, row.scope_id or None, row.app_name) for row in rows),
        created_at=min((row.created_at for row in rows if row.created_at), default=None),
        match_sound=first.match_sound is not False,
        source=first.source or "user",
        phrase=bool(first.phrase),
    )


def _row(
    group_id: str,
    place: Place,
    written: str,
    spoken: str | None,
    key: str,
    created_at=None,
    match_sound: bool = True,
    source: str | None = None,
    added_by: str | None = None,
    phrase: bool = False,
):
    from ..database.models import DictionaryEntry

    return DictionaryEntry(
        scope=place.scope,
        scope_id=place.scope_id or "",
        app_name=place.app_name,
        written=written,
        spoken=spoken,
        key=key,
        group_id=group_id,
        match_sound=match_sound,
        source=source,
        added_by=added_by,
        phrase=phrase,
        created_at=created_at or datetime.utcnow(),
    )


def list_groups(db) -> list[Group]:
    """Every entry, newest first."""
    from ..database.models import DictionaryEntry

    grouped: dict[str, list] = {}
    for row in db.query(DictionaryEntry).order_by(DictionaryEntry.created_at).all():
        grouped.setdefault(row.group_id or row.id, []).append(row)
    groups = [_group(rows) for rows in grouped.values()]
    groups.sort(key=lambda g: g.created_at or datetime.min, reverse=True)
    return groups


def add_group(
    db,
    written: str,
    spoken: str | None,
    places,
    match_sound: bool = True,
    source: str = "user",
    added_by: str | None = None,
    phrase: bool = False,
) -> Group:
    """``phrase``: ``written`` is text to insert where ``spoken`` is said."""
    import uuid

    phrase = bool(phrase)
    written, spoken, places = _clean_written(written, phrase), _clean_spoken(spoken), _places(places)
    if phrase and spoken is None:
        raise ValueError("Say what writes this phrase")
    if len(list_groups(db)) >= MAX_ENTRIES:
        raise ValueError(f"Dictionaries can hold at most {MAX_ENTRIES} entries")
    key = _key(written, spoken)
    for place in places:
        _check_unique(db, place, key)
    if source not in SOURCES:
        raise ValueError("Unknown dictionary source")
    group_id, now = str(uuid.uuid4()), datetime.utcnow()
    for place in places:
        db.add(
            _row(
                group_id,
                place,
                written,
                spoken,
                key,
                now,
                bool(match_sound),
                None if source == "user" else source,
                added_by,
                phrase,
            )
        )
    db.commit()
    invalidate()
    return _group(_rows(db, group_id))


# Words every user's dictionary starts with: the app's own name, which
# Whisper otherwise writes "Cass" or "Kas".
DEFAULTS = ("Kass",)


def ensure_defaults(db, marker) -> None:
    """Add the default words once, as global entries the user can edit or
    delete; ``marker`` is the file recording that they were added, so a
    deleted one stays deleted. Idempotent."""
    from ..database.models import DictionaryEntry

    if marker.exists():
        return
    written = {row.written.casefold() for row in db.query(DictionaryEntry.written).all()}
    for word in DEFAULTS:
        if word.casefold() in written:
            continue
        try:
            add_group(db, word, None, [Place("global")])
        # A full dictionary, or an entry already said that way, stays as it is.
        except ValueError:
            db.rollback()
    marker.touch()


def update_group(db, group_id: str, patch: dict) -> Group | None:
    """Change an entry's words everywhere it applies, where it applies, and
    whether it matches by sound. An entry the user edits is theirs from then on."""
    rows = _rows(db, group_id)
    if not rows:
        return None
    current = _group(rows)
    written = _clean_written(patch["written"], current.phrase) if patch.get("written") is not None else current.written
    spoken = _clean_spoken(patch["spoken"]) if "spoken" in patch else current.spoken
    if current.phrase and spoken is None:
        raise ValueError("Say what writes this phrase")
    places = _places(patch["places"]) if patch.get("places") is not None else list(current.places)
    match_sound = bool(patch["match_sound"]) if patch.get("match_sound") is not None else current.match_sound
    key = _key(written, spoken)
    for place in places:
        _check_unique(db, place, key, group_id)
    wanted = {(p.scope, p.scope_id or ""): p for p in places}
    for row in rows:
        place = wanted.pop((row.scope, row.scope_id or ""), None)
        if place is None:
            db.delete(row)
            continue
        row.written, row.spoken, row.key, row.group_id = written, spoken, key, group_id
        row.match_sound, row.source, row.added_by = match_sound, None, None
        row.app_name = place.app_name or row.app_name
    for place in wanted.values():
        db.add(_row(group_id, place, written, spoken, key, current.created_at, match_sound, phrase=current.phrase))
    db.commit()
    invalidate()
    return _group(_rows(db, group_id))


def delete_group(db, group_id: str) -> bool:
    rows = _rows(db, group_id)
    if not rows:
        return False
    for row in rows:
        db.delete(row)
    db.commit()
    invalidate()
    return True


def delete_added_by(db, capture_id: str) -> None:
    """Remove the words a voice edit capture spelled into the dictionary,
    except those the user has edited since (they are theirs then)."""
    from ..database.models import DictionaryEntry

    rows = (
        db.query(DictionaryEntry)
        .filter(DictionaryEntry.added_by == capture_id, DictionaryEntry.source == "spoken_fix")
        .all()
    )
    if not rows:
        return
    for row in rows:
        db.delete(row)
    db.commit()
    invalidate()


def list_entries(db) -> list[Entry]:
    """Every row, one per place, newest first."""
    from ..database.models import DictionaryEntry

    rows = db.query(DictionaryEntry).order_by(DictionaryEntry.created_at.desc()).all()
    return [_entry(row) for row in rows]


def move_style(db, style_id: str, to_style_id: str) -> None:
    """A deleted style's entries join ``to_style_id``'s; one it already has is dropped."""
    from ..database.models import DictionaryEntry

    rows = db.query(DictionaryEntry).filter(DictionaryEntry.scope == "style", DictionaryEntry.scope_id == style_id)
    taken = {
        key
        for (key,) in db.query(DictionaryEntry.key).filter(
            DictionaryEntry.scope == "style", DictionaryEntry.scope_id == to_style_id
        )
    }
    for row in rows.all():
        if row.key in taken:
            db.delete(row)
        else:
            row.scope_id = to_style_id
            taken.add(row.key)
    db.commit()
    invalidate()


# -- words spelled aloud to fix them -----------------------------------------------


def spelled_word(letters: str, heard: str | None = None) -> str:
    """How to write a word spelled aloud, from its joined letters (``join_spelling``).

    Whisper's case for spelled letters is arbitrary ("M-E-G-H-A-N", "m-r-g-n"),
    so letters all in one case are written as a name ("Meghan"), unless the
    word heard alongside them is the same letters in capitals of its own
    ("NASA", "iPhone"). Letters in mixed case were spelled that way ("capital
    C"), and a word with digits or marks ("mrgnhnt96") is left as spelled.
    """
    word = " ".join(letters.split()).rstrip(".,;:?!")
    heard = " ".join((heard or "").split()).strip(".,;:?!\"'“”")
    if heard and heard.casefold() == word.casefold() and heard != heard.lower():
        return heard
    if word.isalpha() and (word.isupper() or word.islower()):
        return word[:1].upper() + word[1:].lower()
    return word


def add_spelled_word(
    letters: str,
    bundle_id: str | None = None,
    heard: str | None = None,
    db=None,
    added_by: str | None = None,
) -> Group | None:
    """Keep a word the user spelled aloud to fix it, with no confirmation.

    ``letters`` are the spelled letters as ``join_spelling`` joined them
    ("MEGHAN"); ``heard`` is the word the fix was for, when known. The word is
    added everywhere, since a name is the same name in every app, spelling
    only: it is prompted to Whisper, recased where spelled exactly and counts
    as a known name, but never replaces a word that sounds like it (a real
    "Megan"). ``bundle_id`` is the app it was spelled in: nothing is added
    when that app's dictionary already writes the word, and an entry the
    user made is never changed. ``added_by`` is the voice edit capture that
    spelled it, so deleting that capture removes the word. Returns the entry that now has the word, or
    None when there's no word, the dictionary is full, or there's no database.
    """
    from ..database import session as database_session

    if db is None:
        if database_session.SessionLocal is None:
            return None
        with database_session.SessionLocal() as own:
            return add_spelled_word(letters, bundle_id, heard, own, added_by)
    from .styles import snapshot as styles_snapshot

    try:
        written = _clean(spelled_word(letters, heard), "What to write")
    except ValueError:
        return None
    wanted = _normal(written)
    style_id = styles_snapshot().for_app(bundle_id).id
    for entry, overridden in resolve(list_entries(db), bundle_id, style_id):
        if not overridden and not entry.phrase and _normal(entry.written) == wanted:
            return _group(_rows(db, entry.group_id or entry.id))
    try:
        return add_group(
            db, written, None, [Place("global")], match_sound=False, source="spoken_fix", added_by=added_by
        )
    except DuplicateEntryError:
        # Everywhere already has an entry said this way that writes something
        # else; it's the user's, so it stays.
        return None
    except ValueError:
        logger.warning("Could not add the spelled word %r to the dictionary", written, exc_info=True)
        return None
