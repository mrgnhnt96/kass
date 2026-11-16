"""Casing at a phrase's start, and the names a text capitalizes mid-sentence.

Whisper capitalizes a phrase cut off at a pause as if it began a sentence. A
lowercase first word from Whisper reliably continues the sentence; a capital
does not reliably start one. ``continue_phrase`` lowercases it only for a
common word (``_common_word``). Names stay capitalized, including any the user
capitalizes mid-sentence elsewhere (``mid_sentence_capitals``).

Streamed dictation, which joins phrases at pauses, runs in the Rust app
(kass-text); these serve cleanup run again on a saved capture and the known
names.
"""

import functools
import itertools
import json
import re

# fmt: off
_SENTENCE_OPENERS = frozenset((
    "a", "an", "the", "this", "that", "these", "those", "there", "here", "it", "its", "i", "we", "you", "he", "she",
    "they", "my", "our", "your", "his", "her", "their", "and", "but", "so", "or", "yet", "also", "then", "now",
    "well", "okay", "ok", "yeah", "yes", "no", "oh", "anyway", "plus", "still", "though", "although", "because",
    "since", "if", "when", "while", "after", "before", "once", "until", "unless", "as", "maybe", "perhaps",
    "actually", "honestly", "basically", "hopefully", "apparently", "otherwise", "meanwhile", "instead", "besides",
    "however", "what", "why", "how", "who", "whom", "whose", "where", "which", "is", "are", "was", "were", "am",
    "be", "been", "do", "does", "did", "don", "doesn", "didn", "have", "has", "had", "haven", "hasn", "can",
    "can't", "cannot", "could", "couldn", "will", "won", "would", "wouldn", "should", "shouldn", "shall", "may",
    "might", "must", "let", "let's", "not", "just", "only", "even", "please", "thanks", "thank", "sure", "all",
    "some", "any", "every", "each", "both", "either", "neither", "one", "two", "first", "next", "last", "lastly",
    "finally",
))
# fmt: on


# macOS's word list: common words in lowercase, names capitalized.
_WORD_LIST = "/usr/share/dict/words"
_SUFFIXES = ("'s", "\u2019s", "ies", "es", "s", "ed", "d", "ing", "ly")
# The byte-level BPE marker for a leading space.
_SPACE = "\u0120"


@functools.cache
def _vocabulary() -> dict[str, int]:
    """Whisper's BPE vocabulary, whose ids rise roughly as tokens get rarer.

    Empty when no Whisper model is downloaded; the word list decides then.
    """
    from huggingface_hub import try_to_load_from_cache

    from ..backends import WHISPER_HF_REPOS

    for repo in WHISPER_HF_REPOS.values():
        try:
            path = try_to_load_from_cache(repo, "vocab.json")
            if isinstance(path, str):
                with open(path, encoding="utf-8") as vocab:
                    return json.load(vocab)
        except (OSError, ValueError):
            continue
    return {}


@functools.cache
def _lexicon() -> tuple[frozenset[str], frozenset[str]]:
    """Words the word list has in lowercase, and words it has capitalized."""
    try:
        with open(_WORD_LIST, encoding="utf-8") as words:
            entries = words.read().split()
    except OSError:
        return frozenset(), frozenset()
    return (
        frozenset(w for w in entries if w.islower()),
        frozenset(w.casefold() for w in entries if w[:1].isupper()),
    )


def _common_word(word: str) -> bool:
    """Whether ``word`` is mostly written in lowercase mid-sentence, so not a name.

    Whisper's vocabulary says which casing is the more frequent word after a
    space: " move" comes long before " Move", and " Sarah" has no lowercase
    token at all. A word with neither is looked up in the system word list
    (and "tests" as "test"), which lists names capitalized.
    """
    vocab = _vocabulary()
    lower, capital = vocab.get(_SPACE + word.lower()), vocab.get(_SPACE + word[0].upper() + word[1:].lower())
    if lower is not None or capital is not None:
        return lower is not None and (capital is None or lower < capital)
    words, capitalized = _lexicon()
    key = word.casefold()
    stems = [key] + [
        key[: -len(s)] + ("y" if s == "ies" else "") for s in _SUFFIXES if key.endswith(s) and len(key) > len(s) + 2
    ]
    return any(stem in words and stem not in capitalized for stem in stems)


def _first_word(text: str) -> str:
    match = re.match(r"\W*([^\W\d_][\w']*)", text)
    return match.group(1) if match else ""


def _keeps_capital(word: str) -> bool:
    """ "I" and its contractions, acronyms, and words like "GitHub" or "iPhone"."""
    return word == "I" or word.startswith(("I'", "I\u2019")) or any(c.isupper() for c in word[1:])


def _lower_first(text: str) -> str:
    word = _first_word(text)
    if not word or _keeps_capital(word):
        return text
    index = text.index(word)
    return text[:index] + word[0].lower() + text[index + 1 :]


def mid_sentence_capitals(text: str) -> set[str]:
    """Words ``text`` capitalizes where no sentence starts: names and the like.

    Each line starts a sentence of its own.
    """
    kept = set()
    for line in text.splitlines():
        words = line.split()
        for before, word in itertools.pairwise(words):
            first = _first_word(word)
            if first and first[0].isupper() and not re.search(r"[.?!:\u2026-]$", before):
                kept.add(first)
    return kept


def continue_phrase(phrase: str, earlier: str, names: frozenset[str] = frozenset()) -> str:
    """Lowercase the capital Whisper gives a phrase that continues a sentence.

    Whisper capitalizes a phrase cut off at a pause, or started in the middle
    of someone's sentence, as if it began a sentence. Only a common word
    (``_common_word``, or a sentence opener) is lowercased. ``I``, acronyms,
    ``names`` and words capitalized mid-sentence in ``earlier`` (the text
    before this phrase) or later in the phrase itself keep their capitals:
    those are names, not sentence starts.
    """
    word = _first_word(phrase)
    if not word or not word[0].isupper():
        return phrase
    if word in names | mid_sentence_capitals(earlier) | mid_sentence_capitals(phrase):
        return phrase
    key = word.casefold().replace("\u2019", "'")
    if key in _SENTENCE_OPENERS or key.split("'")[0] in _SENTENCE_OPENERS or _common_word(word):
        return _lower_first(phrase)
    return phrase
