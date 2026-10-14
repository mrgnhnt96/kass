"""How something was said: pitch, loudness and pace (docs/plans/EXPRESSIVE_DICTATION.md).

``PitchTrack`` follows the voice as the audio arrives, 10 ms frames analysed
in 100 ms chunks (well under a millisecond each), so nothing waits on it after
release. ``measure`` puts it together with the words' times (word_timing.py):
per sentence how high, how varied and how loud the voice was, and the words
that were drawn out ("wayyy").

Everything is relative to the speaker. A sentence's ``energy`` is how far
above their own statements its pitch level, pitch range and loudness are, in
spreads of each (``Baseline``); stretched words are measured against the
recording's own pace.

Phase 1 only measures: the numbers are saved with the capture for review and
tuning, and the text is never changed.
"""

from __future__ import annotations

import asyncio
import difflib
import itertools
import json
import logging
import re
from dataclasses import dataclass

import numpy as np

logger = logging.getLogger(__name__)

# Frames every 10 ms, each 40 ms of audio resampled to 16 kHz.
HOP = 0.01
ANALYSIS_RATE = 16000
WINDOW = 640
MIN_HZ, MAX_HZ = 70.0, 500.0
_MIN_LAG = int(ANALYSIS_RATE / MAX_HZ)
_MAX_LAG = int(np.ceil(ANALYSIS_RATE / MIN_HZ))
_SUMMED = WINDOW - _MAX_LAG
_FFT = 1024
# YIN: the first lag whose normalized difference dips below this is the period.
YIN_THRESHOLD = 0.1
# A frame is voiced when its best lag's normalized difference is under this.
MAX_APERIODICITY = 0.35
# Quieter frames are never voiced (dB relative to full scale).
SILENCE_DB = -55.0
# Voiced runs shorter than this are noise, not syllables.
MIN_RUN_FRAMES = 3

# A word is drawn out (all must hold): this many times the recording's time
# per syllable, this much louder than its sentence, a steady vowel this long,
# and neither first nor last in its sentence nor a closed-class word.
# The default pace is past a thinking drawl ("naaame", "cooode" said while
# finding the next words), which sounds just like emphasis: real drawls reached
# 6.4. A speaker who writes their own stretches teaches a closer rule.
STRETCH_PACE = 7.0
STRETCH_LOUDER_DB = 1.5
STRETCH_PLATEAU_S = (0.3, 0.6)


@dataclass(frozen=True)
class StretchRule:
    """When a word counts as drawn out (all must hold).

    A closed-class word never does, but for ``words``: the ones this speaker
    has written drawn out themselves ("sooo").
    """

    pace: float = STRETCH_PACE
    louder_db: float = STRETCH_LOUDER_DB
    plateau: tuple[float, float] = STRETCH_PLATEAU_S
    words: frozenset[str] = frozenset()

    def holds(self, shape: dict) -> bool:
        word = _key(shape["word"])
        return (
            (word not in FUNCTION_WORDS or word in self.words)
            and shape["stretch"] >= self.pace
            and shape["louder"] >= self.louder_db
            and self.plateau[0] <= shape["plateau"] <= self.plateau[1]
        )


# Pitch steps under this (semitones per frame) keep a vowel steady.
STEADY_STEP = 0.35
# The question hint looks at the last stretch of each sentence's voice.
FINAL_S = 0.3

# Hesitations ("the...", "a...") are drawn out too, and never written "theee".
FUNCTION_WORDS = frozenset(
    [
        "a",
        "an",
        "the",
        "and",
        "or",
        "but",
        "nor",
        "so",
        "yet",
        "for",
        "of",
        "to",
        "in",
        "on",
        "at",
        "by",
        "with",
        "from",
        "into",
        "onto",
        "upon",
        "about",
        "as",
        "than",
        "then",
        "that",
        "this",
        "these",
        "those",
        "there",
        "here",
        "i",
        "me",
        "my",
        "mine",
        "you",
        "your",
        "yours",
        "he",
        "him",
        "his",
        "she",
        "her",
        "hers",
        "it",
        "its",
        "we",
        "us",
        "our",
        "ours",
        "they",
        "them",
        "their",
        "theirs",
        "who",
        "whom",
        "whose",
        "which",
        "what",
        "when",
        "where",
        "why",
        "how",
        "is",
        "am",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "do",
        "does",
        "did",
        "have",
        "has",
        "had",
        "will",
        "would",
        "shall",
        "should",
        "can",
        "could",
        "may",
        "might",
        "must",
        "not",
        "no",
        "if",
        "um",
        "uh",
        "er",
        "ah",
        "oh",
        "like",
        "just",
    ]
)
_VOWELS = "aeiouy"


@dataclass(frozen=True)
class Frames:
    """The voice frame by frame: pitch in Hz (NaN where unvoiced) and loudness in dB."""

    hz: np.ndarray
    db: np.ndarray


class PitchTrack:
    """Pitch and loudness of a recording, measured as its audio arrives.

    ``feed`` takes PCM at the recording's rate; each 10 ms frame is analysed
    once its 40 ms window has arrived. Only the last window of audio is kept.
    """

    def __init__(self, rate: int):
        self.rate = rate
        self._audio = np.zeros(0, dtype=np.float32)
        # Recording sample index of ``_audio[0]``, and samples received.
        self._start = 0
        self._received = 0
        self._hz: list[np.ndarray] = []
        self._db: list[np.ndarray] = []
        self.count = 0
        # A boxcar over this many source samples keeps 48 kHz audio from aliasing at 16 kHz.
        self._smooth = max(1, round(rate / ANALYSIS_RATE))
        self._offsets = (np.arange(WINDOW) - WINDOW / 2) * (rate / ANALYSIS_RATE)

    def feed(self, pcm: np.ndarray) -> None:
        """Add int16 samples and analyse every frame they complete."""
        self._audio = np.concatenate([self._audio, pcm.astype(np.float32) / 32768.0])
        self._received += len(pcm)
        # Frame k is centered at k * HOP; it needs audio to its window's end.
        last = int(np.floor((self._received - self._offsets[-1] - self._smooth) / (HOP * self.rate)))
        if last < self.count:
            return
        centers = np.arange(self.count, last + 1) * HOP * self.rate
        frames = self._resample(centers)
        hz, db = analyse(frames)
        self._hz.append(hz)
        self._db.append(db)
        self.count = last + 1
        # Keep from the next frame's window on.
        keep_from = int(self.count * HOP * self.rate + self._offsets[0]) - self._smooth - 1
        if keep_from > self._start:
            self._audio = self._audio[keep_from - self._start :]
            self._start = keep_from

    def _resample(self, centers: np.ndarray) -> np.ndarray:
        positions = centers[:, None] + self._offsets[None, :] - self._start
        source = np.arange(len(self._audio))
        shifts = np.arange(self._smooth) - (self._smooth - 1) / 2
        total = np.zeros(positions.shape, dtype=np.float32)
        for shift in shifts:
            total += np.interp(positions + shift, source, self._audio, left=0.0, right=0.0).astype(np.float32)
        return total / len(shifts)

    def frames(self) -> Frames:
        if not self._hz:
            return Frames(np.zeros(0), np.zeros(0))
        return Frames(np.concatenate(self._hz), np.concatenate(self._db))


def analyse(frames: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Pitch (YIN; Hz, NaN where unvoiced) and loudness (dB) of 16 kHz frames."""
    db = 10 * np.log10(np.mean(frames**2, axis=1) + 1e-10)
    squares = np.concatenate([np.zeros((len(frames), 1), np.float32), np.cumsum(frames**2, axis=1)], axis=1)
    lags = np.arange(_MAX_LAG + 1)
    head = squares[:, _SUMMED][:, None]
    shifted = squares[:, lags + _SUMMED] - squares[:, lags]
    spectrum = np.fft.rfft(frames, _FFT) * np.conj(np.fft.rfft(frames[:, :_SUMMED], _FFT))
    correlation = np.fft.irfft(spectrum, _FFT)[:, : _MAX_LAG + 1]
    difference = np.maximum(head + shifted - 2 * correlation, 0.0)
    running = np.cumsum(difference[:, 1:], axis=1)
    normalized = np.ones_like(difference)
    normalized[:, 1:] = difference[:, 1:] * lags[1:] / np.maximum(running, 1e-12)

    window = normalized[:, _MIN_LAG : _MAX_LAG + 1]
    below = window < YIN_THRESHOLD
    lag = np.where(below.any(axis=1), below.argmax(axis=1), window.argmin(axis=1))
    rows = np.arange(len(frames))
    lag = _slide_down(window, rows, lag)
    # A strong second harmonic dips first, at half the period: take the
    # period itself when it is the better fit.
    doubled = 2 * (lag + _MIN_LAG) - _MIN_LAG
    fits = doubled < window.shape[1] - 2
    near = _slide_down(window, rows, np.clip(doubled - 2, 0, window.shape[1] - 1))
    lag = np.where(fits & (window[rows, near] < window[rows, lag]), near, lag)
    aperiodicity = window[rows, lag]
    # Parabolic interpolation between neighbouring lags.
    left = window[rows, np.maximum(lag - 1, 0)]
    right = window[rows, np.minimum(lag + 1, window.shape[1] - 1)]
    curve = left - 2 * aperiodicity + right
    with np.errstate(divide="ignore", invalid="ignore"):
        offset = np.where(np.abs(curve) > 1e-9, 0.5 * (left - right) / curve, 0.0)
    offset = np.clip(offset, -0.5, 0.5)
    hz = ANALYSIS_RATE / (lag + _MIN_LAG + offset)
    voiced = (aperiodicity < MAX_APERIODICITY) & (db > SILENCE_DB)
    return np.where(voiced, hz, np.nan), db


def _slide_down(window: np.ndarray, rows: np.ndarray, lag: np.ndarray) -> np.ndarray:
    """From each lag, step to larger lags while the difference keeps falling."""
    last = window.shape[1] - 1
    for _ in range(last):
        step = (lag < last) & (window[rows, np.minimum(lag + 1, last)] < window[rows, lag])
        if not step.any():
            break
        lag = lag + step
    return lag


def semitones(hz: np.ndarray) -> np.ndarray:
    """Pitch in semitones above 100 Hz, octave jumps folded back and lightly smoothed.

    A tracker sometimes locks onto twice or half the true period for a few
    frames. Within one voiced stretch pitch doesn't move 7 semitones from its
    median, so a frame that far moves by octaves toward it. Whole stretches
    are never moved: on the user's recordings, stretches far above their
    norm were real (Praat agreed), the very excitement this measures.
    Voiced stretches under 30 ms are dropped.
    """
    with np.errstate(invalid="ignore"):
        st = 12 * np.log2(hz / 100.0)
    voiced = ~np.isnan(st)
    smoothed = np.full_like(st, np.nan)
    if not voiced.any():
        return smoothed
    edges = np.flatnonzero(np.diff(np.concatenate([[0], voiced.astype(np.int8), [0]])))
    runs = [(start, end) for start, end in zip(edges[::2], edges[1::2], strict=True) if end - start >= MIN_RUN_FRAMES]
    for start, end in runs:
        run = st[start:end]
        away = run - np.median(run)
        jumped = np.abs(away) > 7
        run[jumped] -= 12 * np.round(away[jumped] / 12)
        # A 5-frame median within the stretch.
        padded = np.pad(run, 2, mode="edge")
        smoothed[start:end] = np.median(np.lib.stride_tricks.sliding_window_view(padded, 5), axis=1)
    return smoothed


@dataclass(frozen=True)
class Baseline:
    """The speaker's statements: median and spread of pitch level, range and loudness."""

    level: tuple[float, float]
    span: tuple[float, float]
    loudness: tuple[float, float]

    # Fewer statements than this say too little about the speaker.
    MIN_SENTENCES = 20

    @classmethod
    def of(cls, sentences: list[dict]) -> Baseline | None:
        """From saved sentences; only statements (ending with a period) count."""
        statements = [s for s in sentences if s.get("text", "").rstrip().endswith(".") and s.get("level") is not None]
        if len(statements) < cls.MIN_SENTENCES:
            return None

        def spread(key, floor):
            values = np.array([s[key] for s in statements], dtype=float)
            median = float(np.median(values))
            return median, max(floor, 1.4826 * float(np.median(np.abs(values - median))))

        return cls(spread("level", 0.5), spread("span", 0.5), spread("loudness", 1.0))

    def energy(self, level: float, span: float, loudness: float) -> float:
        """Mean of how many spreads above the speaker's statements each measure is."""
        return float(
            np.mean(
                [
                    (level - self.level[0]) / self.level[1],
                    (span - self.span[0]) / self.span[1],
                    (loudness - self.loudness[0]) / self.loudness[1],
                ]
            )
        )


def syllables(word: str) -> int:
    """Vowel groups in a word, at least one; a silent final e doesn't count."""
    letters = re.sub(r"[^a-z]", "", word.lower())
    if len(letters) > 2 and letters.endswith("e") and letters[-2] not in _VOWELS + "l":
        letters = letters[:-1]
    return max(1, len(re.findall(r"[aeiouy]+", letters)))


def stretch_word(word: str, letters: int) -> str:
    """``word`` with its last vowel written ``letters`` times ("way" → "wayyy").

    A silent final e is skipped ("five" → "fiiive"). Punctuation around the
    word is kept.
    """
    match = re.fullmatch(r"(\W*)(.*?)(\W*)", word)
    before, core, after = match.groups()
    lower = core.lower()
    end = len(core)
    if end > 3 and lower.endswith("e") and lower[-2] not in _VOWELS:
        end -= 1
    index = max((i for i in range(end) if lower[i] in _VOWELS), default=None)
    if index is None:
        return word
    return before + core[:index] + core[index] * letters + core[index + 1 :] + after


def _theil_sen(times: np.ndarray, values: np.ndarray) -> float:
    if len(times) < 2:
        return 0.0
    i, j = np.triu_indices(len(times), 1)
    dt = times[j] - times[i]
    keep = dt > 0
    return float(np.median((values[j] - values[i])[keep] / dt[keep])) if keep.any() else 0.0


def _longest_steady(st: np.ndarray) -> int:
    """Frames in the longest run of voiced pitch with steps under STEADY_STEP."""
    best = run = 0
    for previous, current in itertools.pairwise(st):
        if not np.isnan(previous) and not np.isnan(current) and abs(current - previous) < STEADY_STEP:
            run += 1
            best = max(best, run)
        else:
            run = 0
    return best + 1 if best else 0


def sentences(words: list) -> list[list]:
    """Words grouped into sentences, each ending at a word ending in . ! or ?."""
    grouped, current = [], []
    for word in words:
        current.append(word)
        if re.search(r"[.!?][\"')\]]*$", word.text):
            grouped.append(current)
            current = []
    if current:
        grouped.append(current)
    return grouped


class Voice:
    """A recording's words and its voice, ready to measure any stretch of it."""

    def __init__(self, frames: Frames, words: list):
        self.words = words
        self.st = semitones(frames.hz.copy())
        self.db = frames.db

    def frames_in(self, start: float, end: float) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        """Pitch over ``start``–``end`` (seconds), its voiced frames' loudness and their indices."""
        part = slice(max(0, int(start / HOP)), min(len(self.st), int(np.ceil(end / HOP))))
        values = self.st[part]
        return values, self.db[part][~np.isnan(values)], np.flatnonzero(~np.isnan(values)) + part.start

    def energy(self, start: float, end: float, baseline: Baseline) -> float | None:
        """``Baseline.energy`` of the voice from ``start`` to ``end``: ``stats``' level, span and loudness only.

        Used after release, so kept to one pass over the frames.
        """
        values, loud, _ = self.frames_in(start, end)
        pitched = values[~np.isnan(values)]
        if len(pitched) < 10:
            return None
        low, level, high = np.percentile(pitched, [10, 50, 90])
        return baseline.energy(float(level), float(high - low), float(np.median(loud)))

    def stats(self, start: float, end: float) -> dict | None:
        """How high, how varied and how loud the voice was from ``start`` to ``end``.

        None with under 10 voiced frames (0.1 s): too little to say.
        """
        values, loud, indices = self.frames_in(start, end)
        pitched = values[~np.isnan(values)]
        if len(pitched) < 10:
            return None
        level = float(np.median(pitched))
        final = indices[indices * HOP >= indices[-1] * HOP - FINAL_S]
        return dict(
            level=level,
            span=float(np.percentile(pitched, 90) - np.percentile(pitched, 10)),
            loudness=float(np.median(loud)),
            slope=_theil_sen(final * HOP, self.st[final]),
            rise=float(np.median(self.st[final])) - level,
        )


def measure(frames: Frames, words: list, baseline: Baseline | None) -> dict:
    """Per-sentence pitch, loudness and energy, and drawn-out words.

    ``words`` have times in seconds from the recording's start (word_timing.Word).
    """
    return measure_voice(Voice(frames, words), baseline)


def measure_voice(voice: Voice, baseline: Baseline | None, rule: StretchRule | None = None) -> dict:
    """``measure`` of a voice already worked out.

    ``shapes`` are how drawn out each content word was, against the
    recording's pace: what a speaker's own stretches are learned from.
    """
    rule = rule or StretchRule()
    measured, shapes = [], []
    pace, offset = _pace(voice), 0
    for number, sentence in enumerate(sentences(voice.words)):
        start, end = sentence[0].start, sentence[-1].end
        entry = dict(text=" ".join(word.text for word in sentence), start=round(start, 2), end=round(end, 2))
        if (stats := voice.stats(start, end)) is not None:
            entry.update({key: round(value, 2) for key, value in stats.items()})
            entry["energy"] = (
                round(baseline.energy(stats["level"], stats["span"], stats["loudness"]), 2) if baseline else None
            )
        measured.append(entry)
        if pace and "loudness" in entry:
            shapes += _shapes(sentence, number, offset, entry["loudness"], pace, voice)
        offset += len(sentence)
    return dict(
        version=1,
        pace=round(pace, 3) if pace else None,
        voiced=round(float(np.count_nonzero(~np.isnan(voice.st)) * HOP), 2),
        sentences=measured,
        stretched=[
            dict(shape, written=stretch_word(shape["word"], _letters(shape["stretch"])))
            for shape in shapes
            if rule.holds(shape)
        ],
        shapes=[[shape["word"], shape["stretch"], shape["louder"], shape["plateau"]] for shape in shapes],
    )


def _pace(voice: Voice) -> float | None:
    """The recording's time per syllable, from its voiced frames."""
    per_syllable = []
    for word in voice.words:
        seconds = len(voice.frames_in(word.start, word.end)[2]) * HOP
        if seconds:
            per_syllable.append(seconds / syllables(word.text))
    return float(np.median(per_syllable)) if per_syllable else None


def _letters(stretch: float) -> int:
    """How many times a drawn-out word's vowel is written."""
    return 5 if stretch >= 6 else 4 if stretch >= 4.5 else 3


# A sentence said with at least this much energy (spreads above the
# speaker's statements) ends with "!". On the user's labelled takes, excited
# sentences scored 2.96–4.07 and everything else 2.03 or less; none of 198
# ordinary sentences reached it. Later plain sentences reached 2.51 and 2.59, so
# it sits just under the excited takes. Strict on purpose: it adapts per user
# from their edits, never by lowering this (docs/plans/EXPRESSIVE_DICTATION.md).
EXCLAIM_ENERGY = 2.9
# At least this share of a written sentence's words must be found among the
# words heard, or it isn't measured (cleanup rewrote it).
MIN_FOUND = 0.5
# A period after one of these ends the word, not the sentence.
_ABBREVIATIONS = frozenset(["mr", "mrs", "ms", "dr", "st", "vs", "etc", "jr", "sr", "prof", "inc", "ltd", "eg", "ie"])
_CLOSERS = re.escape("\"')]\u201d\u2019")
_ENDS_SENTENCE = re.compile(f"[.!?][{_CLOSERS}]*$")
# A period after a letter: not an ellipsis, a number ("3.") or a file name.
_PERIOD = re.compile(f"(?<=[^\\W\\d_])\\.(?=[{_CLOSERS}]*$)")


def _key(word: str) -> str:
    return re.sub(r"[\W_]", "", word.lower())


def _said_period(words: list, index: int) -> bool:
    """Whether the speaker said the period after ``words[index]``: what they said wins."""
    after = [_key(word.text) for word in words[index + 1 : index + 3]]
    return after[:1] == ["period"] or after == ["full", "stop"]


def _heard(text: str, voice: Voice) -> tuple[list[re.Match], dict[int, int]]:
    """``text``'s words, and for each one found among the words heard, which it is."""
    tokens = list(re.finditer(r"\S+", text))
    matcher = difflib.SequenceMatcher(
        None, [_key(word.text) for word in voice.words], [_key(token.group()) for token in tokens], autojunk=False
    )
    heard_at = {}
    for block in matcher.get_matching_blocks():
        for offset in range(block.size):
            heard_at[block.b + offset] = block.a + offset
    return tokens, heard_at


def stretch(text: str, voice: Voice, stretched: list[dict]) -> str:
    """``text`` with each drawn-out word (``measure_voice``'s ``stretched``) written drawn out.

    Only where the written word is the word heard: one the cleanup changed
    stays as written.
    """
    if not stretched:
        return text
    tokens, heard_at = _heard(text, voice)
    written_at = {heard: index for index, heard in heard_at.items()}
    changed = text
    # From the end, so earlier positions stay put.
    for shape in sorted(stretched, key=lambda shape: shape["at"], reverse=True):
        index = written_at.get(shape["at"])
        if index is None:
            continue
        token = tokens[index]
        changed = (
            changed[: token.start()] + stretch_word(token.group(), _letters(shape["stretch"])) + changed[token.end() :]
        )
    return changed


def written_sentences(text: str, voice: Voice, baseline: Baseline | None) -> list[dict]:
    """The sentences of ``text`` that were said and measured, with their energy.

    Sentences are the written text's (the cleanup's), each found in the
    recording by its words, so a sentence Whisper ran into the next one is
    still measured on its own. One too rewritten to find, or with too little
    voice, is left out. Each has its ``text``, where it ends in ``text``
    (``end``), its ``energy`` and whether the speaker said its period.
    """
    if baseline is None or not voice.words:
        return []
    tokens, heard_at = _heard(text, voice)
    found, first = [], 0
    for index, token in enumerate(tokens):
        word = token.group()
        if not _ENDS_SENTENCE.search(word) or (word.endswith(".") and _key(word) in _ABBREVIATIONS):
            continue
        sentence, first = range(first, index + 1), index + 1
        heard = [heard_at[i] for i in sentence if i in heard_at and _key(tokens[i].group())]
        if not heard or len(heard) < MIN_FOUND * len(sentence):
            continue
        energy = voice.energy(voice.words[min(heard)].start, voice.words[max(heard)].end, baseline)
        if energy is None:
            continue
        found.append(
            dict(
                text=text[tokens[sentence[0]].start() : token.end()],
                end=token.end(),
                energy=energy,
                said_period=_said_period(voice.words, max(heard)),
            )
        )
    return found


def exclaim(text: str, voice: Voice, baseline: Baseline | None, cutoff: float = EXCLAIM_ENERGY) -> str:
    """``text`` with "!" ending each sentence said with clearly more energy than the speaker's statements.

    Only a period changes, and not one the speaker said: a question, a
    sentence left without an ending and one too rewritten to find keep theirs.
    """
    changed = list(text)
    for sentence in written_sentences(text, voice, baseline):
        period = _PERIOD.search(sentence["text"])
        if period is not None and sentence["energy"] >= cutoff and not sentence["said_period"]:
            changed[sentence["end"] - len(sentence["text"]) + period.start()] = "!"
    return "".join(changed)


def _shapes(sentence, number, offset, sentence_loudness, pace, voice: Voice) -> list[dict]:
    """How drawn out each word of a sentence that could be was.

    Not the first or last word, a number or an initialism ("UI", said letter
    by letter). Closed-class words are measured too, though only a speaker's
    own rule may draw one out: "the...", "a..." are mostly hesitations.
    """
    found = []
    for index, word in enumerate(sentence[1:-1], start=1):
        core = re.sub(r"[^\w']", "", word.text)
        if not core or not core.isalpha() or (len(core) > 1 and core.isupper()):
            continue
        values, loud, indices = voice.frames_in(word.start, word.end)
        found.append(
            dict(
                word=word.text,
                sentence=number,
                index=index,
                at=offset + index,
                stretch=round(len(indices) * HOP / syllables(word.text) / pace, 2),
                louder=round(float(np.median(loud)) - sentence_loudness if len(loud) else 0.0, 2),
                plateau=round(_longest_steady(values) * HOP, 2),
            )
        )
    return found


def _placed(found: list, start: float, words: list) -> list:
    """``found`` and a phrase's ``words``, which began ``start`` seconds into the recording.

    Where a forced cut overlaps the phrase before, the overlap's words are kept once.
    """
    from ..backends.word_timing import Word

    reached = found[-1].end if found else 0.0
    return found + [
        Word(word.text, start + word.start, start + word.end) for word in words if start + word.start >= reached - 0.05
    ]


class Expression:
    """One dictation's measurements, gathered while the user speaks.

    The voice is tracked as audio arrives; each phrase's word times are
    worked out on a worker thread as soon as it is recognized. A phrase
    recognized after release waits until the final text is sent, so nothing
    here ever adds to the wait for it, unless the text is written how it was
    said (``eager``): then its word times are worked out on the CPU while
    cleanup runs on the GPU, and are ready before it ends.
    """

    def __init__(self, rate: int, eager: bool = False):
        self.rate = rate
        self.eager = eager
        self.track = PitchTrack(rate)
        self.failed = False
        # (start in seconds, alignment or its words being worked out)
        self._phrases: list[tuple[float, object]] = []

    def feed(self, pcm: np.ndarray) -> None:
        if self.failed:
            return
        try:
            self.track.feed(pcm)
        except Exception:
            # Measuring never fails a dictation.
            logger.exception("Pitch tracking failed; this dictation won't be measured")
            self.failed = True

    def phrase(self, alignment, start_sample: int, released: bool) -> None:
        """A recognized phrase's alignment (word_timing.Alignment), which began at ``start_sample``."""
        if alignment is None or self.failed:
            return
        if released and not self.eager:
            self._phrases.append((start_sample / self.rate, alignment))
            return
        words = asyncio.get_running_loop().run_in_executor(None, alignment.words)
        self._phrases.append((start_sample / self.rate, words))

    async def words(self) -> list:
        """Every phrase's words, in seconds from the recording's start."""
        found = []
        for start, pending in self._phrases:
            if isinstance(pending, asyncio.Future):
                words = await pending
            else:
                words = await asyncio.to_thread(pending.words)
            found = _placed(found, start, words)
        return found

    async def measure(self, baseline: Baseline | None) -> dict | None:
        """The measurements to save, or None when there are no words to place them."""
        if self.failed:
            return None
        words = await self.words()
        if not words:
            return None
        return await asyncio.to_thread(measure, self.track.frames(), words, baseline)


# Captures the speaker's baseline is read from.
BASELINE_CAPTURES = 200


def load_baseline() -> Baseline | None:
    """The speaker's baseline, from the sentences saved with recent captures."""
    from ..database import Capture, session as database_session

    with database_session.SessionLocal() as db:
        saved = (
            db.query(Capture.prosody)
            .filter(Capture.prosody.isnot(None))
            .order_by(Capture.created_at.desc())
            .limit(BASELINE_CAPTURES)
            .all()
        )
    found = []
    for (text,) in saved:
        try:
            found += json.loads(text).get("sentences", [])
        except (ValueError, AttributeError):
            continue
    return Baseline.of(found)


def save(capture_id: str, measured: dict) -> None:
    """Store a capture's measurements with it."""
    from ..database import Capture, session as database_session

    with database_session.SessionLocal() as db:
        db.query(Capture).filter(Capture.id == capture_id).update({Capture.prosody: json.dumps(measured)})
        db.commit()


async def measure_and_save(capture_id: str, expression: Expression, text: str = "") -> None:
    """Measure a sent dictation and save it with its capture. Run after the final event.

    ``text`` is the text it returned; its sentences are saved with their
    energy, whether or not "!" is written, to learn from the user's edits of
    them (docs/plans/EXPRESSIVE_DICTATION.md).
    """
    try:
        baseline = await asyncio.to_thread(load_baseline)
        words = await expression.words() if not expression.failed else []
        if not words:
            return
        voice = await asyncio.to_thread(Voice, expression.track.frames(), words)
        measured = await asyncio.to_thread(_measure_voice, voice, baseline, text)
        await asyncio.to_thread(save, capture_id, measured)
    except Exception:
        logger.exception("Couldn't measure how the dictation was said")


def _measure_voice(voice: Voice, baseline: Baseline | None, text: str) -> dict:
    measured = measure_voice(voice, baseline)
    measured["written"] = [
        dict(text=sentence["text"], energy=round(sentence["energy"], 2))
        for sentence in written_sentences(text, voice, baseline)
    ]
    return measured
