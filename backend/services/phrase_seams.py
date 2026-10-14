"""Join dictation phrases that were cut at pauses.

Streaming dictation recognizes and refines each phrase on its own, so every
phrase would otherwise come back punctuated as a finished sentence. A pause is
a breath, not a sentence end: phrases stay open while dictation continues and
the seam between two phrases is decided once the next one arrives.

Whisper is given the earlier text when it recognizes a phrase, so the casing of
its first word says whether the speaker continued the sentence. A capitalized
first word is only treated as a new sentence when it is a common sentence
opener; anything else is assumed to be a name continuing the sentence.

A lowercase first word from Whisper reliably continues the sentence; a capital
does not reliably start one. ``continue_phrase`` lowercases it only for a
common word (``_common_word``). Names stay capitalized, including any the user
capitalizes mid-sentence elsewhere.

Streaming cleanup now sees a whole open sentence at a time
(sentence_tail.py), so joins happen only where a long tail is settled
mid-sentence. Before cleanup, ``strip_pause_mark``, ``continue_after_seam``
and ``continue_phrase`` remove the ending and capital Whisper gives a phrase
only because the audio paused.
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

_TERMINAL = ".?!:;"

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


def load_word_data() -> None:
    """Read the vocabulary and word list now, so the first dictation doesn't wait. Blocking."""
    _vocabulary()
    _lexicon()


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


def _starts_sentence(raw: str) -> bool:
    word = _first_word(raw)
    if not word or not word[0].isupper():
        return False
    key = word.casefold().replace("\u2019", "'")
    return key in _SENTENCE_OPENERS or key.split("'")[0] in _SENTENCE_OPENERS


def _keeps_capital(word: str) -> bool:
    """ "I" and its contractions, acronyms, and words like "GitHub" or "iPhone"."""
    return word == "I" or word.startswith(("I'", "I\u2019")) or any(c.isupper() for c in word[1:])


def _lower_first(text: str) -> str:
    word = _first_word(text)
    if not word or _keeps_capital(word):
        return text
    index = text.index(word)
    return text[:index] + word[0].lower() + text[index + 1 :]


def open_phrase(text: str, raw: str) -> str:
    """Drop end punctuation the phrase got only because the audio paused.

    A closing period always goes; a question or exclamation mark goes only when
    Whisper, which heard the earlier text, didn't end the phrase with it.
    A phrase that ends with a spoken line break isn't open: it stays as it is.
    """
    if "\n" in text[len(text.rstrip()) :]:
        return text
    text = text.rstrip()
    heard = raw.rstrip()[-1:]
    if re.search(r"[\w)\"'\u201d][?!]$", text) and text[-1] != heard:
        return text[:-1]
    return re.sub(r"(?<=[\w)\"'\u201d])\.$", "", text)


def strip_pause_mark(text: str) -> str:
    """Drop the ending Whisper gives a phrase only because the audio paused.

    Cut off at a pause, Whisper ends the phrase as if it were finished: a
    period, a trailing dash ("if I pause-"), or dots ("How..."). None of that
    was said. A question or exclamation mark stays until the next phrase
    shows whether the sentence went on (``continue_after_seam``). So does the
    period of an abbreviation ("U.S."), which isn't an ending.
    """
    stripped = text.rstrip()
    last = stripped.split()[-1] if stripped else ""
    if re.search(r"[\w)\"'\u201d](?:\.{2,}|\u2026|[-\u2013\u2014]+)$", stripped):
        return re.sub(r"(?:\.{2,}|\u2026|[-\u2013\u2014]+)$", "", stripped)
    if re.search(r"[\w)\"'\u201d]\.$", stripped) and "." not in last[:-1]:
        return stripped[:-1]
    return stripped


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


def continues_sentence(before: str) -> bool:
    """Whether text inserted after ``before`` (the field up to the caret) is mid-sentence.

    An empty field, a line break, or a finished sentence (``.``, ``?``, ``!``,
    ``:``) start a new one. So does anything that isn't a word, a comma or a
    dash, such as a list bullet.
    """
    trailing = before[len(before.rstrip()) :]
    text = before.rstrip().rstrip("\"'\u201d\u2019)]")
    if not text or "\n" in trailing:
        return False
    return text[-1].isalnum() or text[-1] in ",;\u2014\u2013"


def match_raw_start(text: str, raw: str) -> str:
    """Give ``text``, cleaned up from ``raw``, the casing of raw's first word.

    Cleanup writes every text as the start of a sentence. Where ``raw``
    continues one, the same word must stay lowercase. The word is looked for
    among raw's first few words, since cleanup drops fillers ("um").
    """
    word = _first_word(text)
    if not word or not word[0].isupper():
        return text
    for heard in re.findall(r"[^\W\d_][\w'\u2019]*", raw)[:4]:
        if heard.casefold() == word.casefold():
            return _lower_first(text) if heard[0].islower() else text
    return text


def continue_after_seam(before: str, phrase: str, earlier: str, names: frozenset[str] = frozenset()) -> tuple[str, str]:
    """Join ``phrase`` after ``before``, text that ended where the audio paused.

    Returns both, adjusted. Whisper ends a phrase cut at a pause as if it were
    finished, and a question or exclamation mark there is as often the pause
    as the speaker (``pause? for a second``). Cleanup takes a mark it is given
    as a sentence end, so the mark goes. The capital Whisper gave the next
    phrase stays instead: a weaker hint that a new sentence may start, which
    cleanup drops when the words run on. A word that is always capitalized
    ("I", a name) can't be that hint, so there the mark stays. Without such a
    mark the phrase continues as ``continue_phrase`` decides.
    """
    if not re.search(r"[\w)\"'”][?!]$", before):
        return before, continue_phrase(phrase, earlier, names)
    word = _first_word(phrase)
    if word and word[0].isupper() and continue_phrase(phrase, earlier, names) == phrase:
        return before, phrase
    return before[:-1], phrase


# A period after these ends an abbreviation, not a sentence.
_ABBREVIATIONS = frozenset(("mr", "mrs", "ms", "dr", "st", "vs", "etc", "jr", "sr", "prof", "inc", "ltd", "approx"))
# "I" and its contractions, as a whole word: not "i.e." or "i18n".
_LOWER_I = re.compile(r"(?<![\w'\u2019.@/-])i(?=(?:['\u2019](?:m|d|ll|ve|re))?(?![\w'\u2019-])(?!\.\w))")
_SENTENCE_END = re.compile(r"[^\W\d_][\"')\]\u201d\u2019]*[.?!][\"')\]\u201d\u2019]*$")


def _is_name(word: str, names: frozenset[str]) -> bool:
    if word in names or _keeps_capital(word):
        return True
    key = word.casefold().replace("\u2019", "'")
    return key not in _SENTENCE_OPENERS and key.split("'")[0] not in _SENTENCE_OPENERS and not _common_word(word)


def _ends_sentence(token: str) -> bool:
    """Whether ``token`` ends a sentence: not "...", "U.S." or "etc."."""
    if not _SENTENCE_END.search(token) or re.search(r"\.\.|\u2026", token):
        return False
    word = token.rstrip("\"')]\u201d\u2019.?!")
    return "." not in word and word.casefold() not in _ABBREVIATIONS


def keep_raw_capitals(text: str, raw: str, names: frozenset[str] = frozenset()) -> str:
    """Undo lowercasing that cleanup or a seam did against ``raw``, the transcript.

    Told a text that continues a sentence, cleanup may write the rest of it in
    lowercase, "i" included, and after a sentence it ends itself. Whatever the
    cause: "I" stays capitalized wherever the transcript has it, a name the
    transcript capitalizes mid-sentence keeps its capital everywhere, and a
    sentence after ``.``, ``?`` or ``!`` starts with a capital. A style that
    starts sentences in lowercase applies after this.
    """
    if re.search(r"(?<![\w'\u2019])I(?![\w-])", raw) and not _LOWER_I.search(raw):
        text = _LOWER_I.sub("I", text)
    for name in mid_sentence_capitals(raw):
        if name != "I" and not name.startswith(("I'", "I\u2019")) and _is_name(name, names):
            lower = re.escape(name.lower())
            text = re.sub(rf"(?<![\w'\u2019.@/-]){lower}(?![\w-])(?!\.\w)", name, text)
    parts = re.split(r"(\s+)", text)
    for index in range(2, len(parts), 2):
        token = parts[index]
        word = _first_word(token)
        if (
            word
            and word.islower()
            and token.lstrip("\"'(\u201c\u2018").startswith(word)
            and _ends_sentence(parts[index - 2])
        ):
            parts[index] = token.replace(word, word[0].upper() + word[1:], 1)
    return "".join(parts)


def close_phrase(text: str) -> str:
    """End a finished dictation that was left open at its last pause.

    A spoken line break at the end stays, after the period.
    """
    stripped = text.rstrip()
    if re.search(r"[\w)\"'\u201d]$", stripped):
        trailing = text[len(stripped) :]
        return stripped + "." + (trailing if "\n" in trailing else "")
    return text


def join_phrases(previous: str, phrase: str, raw_phrase: str, style: str) -> str:
    """Attach ``phrase`` to ``previous`` across a pause.

    ``raw_phrase`` is what Whisper heard for ``phrase``; its casing decides
    whether the speaker continued the sentence or began a new one.
    """
    before = previous.rstrip()
    if not before:
        return phrase
    if not phrase:
        return previous
    # Spoken formatting may end a phrase with a line or paragraph break.
    whitespace = previous[len(before) :] if "\n" in previous[len(before) :] else " "
    if phrase[0] == "\n":
        # The phrase starts with a spoken line break: it is the whole seam.
        return f"{before}{whitespace.strip(' ')}{phrase}"
    new_sentence = _starts_sentence(raw_phrase)
    casual = style == "casual"

    if whitespace != " ":
        return f"{before}{whitespace}{phrase}"
    if before[-1] in _TERMINAL:
        if casual and new_sentence and before[-1] == "." and not before.endswith(".."):
            return f"{before[:-1]}, {_lower_first(phrase)}"
        return f"{before} {phrase}"
    if before[-1] == ",":
        return f"{before} {_lower_first(phrase) if casual or not new_sentence else phrase}"
    if not re.search(r"[\w)\"'\u201d]$", before):
        return f"{before} {phrase}"
    if not new_sentence:
        first = _first_word(raw_phrase)
        return f"{before} {_lower_first(phrase) if first and first[0].islower() else phrase}"
    if casual:
        return f"{before}, {_lower_first(phrase)}"
    return f"{before}. {phrase}"
