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

The Rust server owns the entries and edits them; the worker reads them for
training (a fresh process each run) and model startup.
"""

import re
import threading
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from datetime import datetime
from difflib import SequenceMatcher

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


# -- words fixed in a correction (voice training's bank) ---------------------------

# A word written drawn out ("wayyy") is how it was said, not vocabulary.
_DRAWN = re.compile(r"([^\W\d_])\1\1", re.IGNORECASE)


# How alike a heard form and the term written for it must sound to be that
# term misheard: "cloud" and "Claude" are 0.73, "midnight" and "Midna" 0.62,
# a reword like "him" -> "Morgan" 0.22.
SOUNDS_ALIKE = 0.5


def sounds_alike(heard: str, written: str) -> bool:
    return SequenceMatcher(None, _sound(heard), _sound(written), autojunk=False).ratio() >= SOUNDS_ALIKE


def misheard_terms(original: str, corrected: str) -> list[tuple[str, str]]:
    """Each (heard, written) pair where the user fixed a misheard term.

    The written term is one or two words in place of up to three heard ones,
    and isn't all common words, digits or a drawn-out spelling: vocabulary,
    not a wording or formatting change.
    """
    from .phrase_seams import _common_word

    heard, wrote = list(_WORD.finditer(original)), list(_WORD.finditer(corrected))
    matcher = SequenceMatcher(None, [m.group() for m in heard], [m.group() for m in wrote], autojunk=False)
    found = []
    for tag, a, b, c, d in matcher.get_opcodes():
        if tag != "replace" or not (1 <= b - a <= 3 and 1 <= d - c <= 2):
            continue
        term = corrected[wrote[c].start() : wrote[d - 1].end()]
        said = original[heard[a].start() : heard[b - 1].end()]
        words = _WORD.findall(term)
        if any(char.isdigit() for char in term) or _DRAWN.search(term) or all(_common_word(w) for w in words):
            continue
        # Words only dropped ("I. Don't" -> "don't") were heard right. A
        # recased name ("github" -> "GitHub") is still the user's term.
        heard_words = _WORD.findall(said)
        if len(words) < len(heard_words) and {w.casefold() for w in words} <= {w.casefold() for w in heard_words}:
            continue
        found.append((said, term))
    return found
