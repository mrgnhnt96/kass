"""How something was said: pitch, loudness, pace and drawn-out words."""

import asyncio

import numpy as np
import pytest

from backend.backends.word_timing import Word
from backend.services import prosody
from backend.services.prosody import (
    Baseline,
    Expression,
    Frames,
    PitchTrack,
    Voice,
    exclaim,
    measure,
    semitones,
    stretch,
    stretch_word,
    syllables,
    written_sentences,
)


def tone(hz, seconds, rate, amplitude=8000):
    t = np.arange(int(seconds * rate)) / rate
    # A voice-like tone: the fundamental and a strong second harmonic.
    wave = np.sin(2 * np.pi * hz * t) + 0.6 * np.sin(4 * np.pi * hz * t)
    return (amplitude * wave / 1.6).astype("<i2")


@pytest.mark.parametrize("rate", [16000, 48000])
@pytest.mark.parametrize("hz", [110.0, 220.0])
def test_pitch_track_follows_a_tone_fed_in_chunks(rate, hz):
    track = PitchTrack(rate)
    audio = tone(hz, 1.0, rate)
    for start in range(0, len(audio), rate // 10):
        track.feed(audio[start : start + rate // 10])
    frames = track.frames()
    # One frame per 10 ms once its window has arrived.
    assert 95 <= len(frames.hz) <= 100
    voiced = frames.hz[~np.isnan(frames.hz)]
    assert len(voiced) > 0.9 * len(frames.hz)
    assert np.median(voiced) == pytest.approx(hz, rel=0.01)


def test_silence_is_unvoiced():
    track = PitchTrack(16000)
    track.feed(np.zeros(16000, dtype="<i2"))
    frames = track.frames()
    assert np.isnan(frames.hz).all()
    assert (frames.db < prosody.SILENCE_DB).all()


def test_semitones_fold_octave_slips_and_drop_short_runs():
    hz = np.full(20, 200.0)
    hz[10] = 400.0  # one frame an octave up
    hz[[3, 15]] = np.nan
    hz[16:18] = np.nan
    st = semitones(hz)
    # 200 Hz is 12 semitones above 100 Hz.
    assert np.nanmax(np.abs(st - 12)) < 1e-9
    # Frames 18–19 are a 2-frame run, too short to keep.
    assert np.isnan(st[18:]).all()
    assert np.isnan(st[3])


def test_a_whole_run_high_above_the_norm_is_kept():
    hz = np.concatenate([np.full(10, 200.0), [np.nan], np.full(10, 450.0)])
    st = semitones(hz)
    assert st[-1] == pytest.approx(12 * np.log2(4.5))


@pytest.mark.parametrize(
    ("word", "count"), [("way", 1), ("five", 1), ("table", 2), ("really", 2), ("away.", 2), ("hmm", 1)]
)
def test_syllables(word, count):
    assert syllables(word) == count


@pytest.mark.parametrize(
    ("word", "letters", "written"),
    [("way", 3, "wayyy"), ("five", 3, "fiiive"), ("So,", 4, "Soooo,"), ("long", 3, "looong"), ("hmm", 3, "hmm")],
)
def test_stretch_word_draws_out_the_last_vowel(word, letters, written):
    assert stretch_word(word, letters) == written


def statement(level, span=4.0, loudness=-20.0, text="A statement."):
    return dict(text=text, level=level, span=span, loudness=loudness)


def test_baseline_needs_enough_statements():
    assert Baseline.of([statement(5.0)] * 19) is None
    # Questions don't count toward a speaker's statements.
    assert Baseline.of([statement(5.0)] * 19 + [statement(5.0, text="Really?")]) is None
    baseline = Baseline.of([statement(5.0 + i % 3) for i in range(20)])
    assert baseline.level[0] == 6.0
    assert baseline.energy(baseline.level[0], baseline.span[0], baseline.loudness[0]) == 0.0
    assert baseline.energy(
        baseline.level[0] + 3 * baseline.level[1], baseline.span[0], baseline.loudness[0]
    ) == pytest.approx(1.0)


TEXT = "That was really far away from the old house."
TIMES = [0.0, 0.2, 0.4, 0.6, 1.9, 2.2, 2.4, 2.6, 2.9, 3.6]


def far_said(louder=6.0):
    """3.6 s of voice at 150 Hz with a break every fifth frame, except a
    steady half second in "far", which is also louder and lasts 1.3 s."""
    hz = np.full(360, 150.0)
    hz[np.arange(360) % 5 == 4] = np.nan
    hz[70:120] = 150.0
    db = np.full(360, -20.0)
    db[60:190] += louder
    words = [Word(text, start, end) for text, start, end in zip(TEXT.split(), TIMES, TIMES[1:], strict=False)]
    return Frames(hz, db), words


def test_a_long_loud_steady_word_is_drawn_out():
    frames, words = far_said()
    measured = measure(frames, words, None)
    assert measured["version"] == 1
    assert measured["pace"] == pytest.approx(0.16, abs=0.01)
    (sentence,) = measured["sentences"]
    assert sentence["text"] == TEXT
    assert sentence["level"] == pytest.approx(12 * np.log2(1.5), abs=0.01)
    assert sentence["energy"] is None
    (stretched,) = measured["stretched"]
    assert stretched["word"] == "far"
    assert stretched["index"] == 3
    assert stretched["written"] == "faaaaar"


def test_a_long_word_said_no_louder_is_not():
    frames, words = far_said(louder=0.0)
    assert measure(frames, words, None)["stretched"] == []


def test_an_initialism_is_never_drawn_out():
    frames, words = far_said()
    words[3] = Word("UI", words[3].start, words[3].end)
    assert measure(frames, words, None)["stretched"] == []


def test_energy_is_against_the_baseline():
    frames, words = far_said()
    baseline = Baseline((0.0, 1.0), (0.0, 1.0), (-20.0, 1.0))
    (sentence,) = measure(frames, words, baseline)["sentences"]
    assert sentence["energy"] == pytest.approx(
        baseline.energy(sentence["level"], sentence["span"], sentence["loudness"]), abs=0.01
    )


class Said:
    def __init__(self, *words):
        self._words = [Word(*word) for word in words]

    def words(self):
        return self._words


@pytest.mark.asyncio
async def test_words_overlapped_by_a_forced_cut_are_kept_once():
    expression = Expression(16000)
    expression.phrase(Said(("a", 0.0, 1.0), ("b", 1.0, 2.0)), 0, released=False)
    # The next phrase repeats the last half second, then goes on.
    expression.phrase(Said(("b", 0.0, 0.5), ("c", 0.6, 1.0)), 24000, released=True)
    expression.phrase(None, 48000, released=True)
    words = await expression.words()
    assert [(word.text, word.start, word.end) for word in words] == [
        ("a", 0.0, 1.0),
        ("b", 1.0, 2.0),
        ("c", 2.1, 2.5),
    ]


@pytest.mark.asyncio
async def test_a_failed_track_measures_nothing(monkeypatch):
    expression = Expression(16000)

    def broken(pcm):
        raise ValueError("bad audio")

    monkeypatch.setattr(expression.track, "feed", broken)
    expression.feed(np.zeros(160, dtype="<i2"))
    assert expression.failed
    expression.phrase(Said(("a", 0.0, 1.0)), 0, released=True)
    assert await expression.measure(None) is None


@pytest.mark.asyncio
async def test_measuring_never_raises(monkeypatch):
    def broken():
        raise RuntimeError("no database")

    monkeypatch.setattr(prosody, "load_baseline", broken)
    await prosody.measure_and_save("capture", Expression(16000))
    await asyncio.sleep(0)


# Plain sentences at 150 Hz; excited ones higher and louder.
CALM_HZ, EXCITED_HZ = 150.0, 220.0
NORM = Baseline((12 * np.log2(1.5), 1.0), (0.0, 1.0), (-20.0, 1.0))


def spoken(*phrases):
    """The voice and words of ``phrases``, each (words, excited), one word per 0.3 s."""
    hz, db, words = [], [], []
    for text, excited in phrases:
        for word in text.split():
            start = len(hz) * prosody.HOP
            hz += [EXCITED_HZ if excited else CALM_HZ] * 30
            db += [-14.0 if excited else -20.0] * 30
            words.append(Word(word, start, start + 0.3))
    return Voice(Frames(np.array(hz), np.array(db)), words)


def test_a_sentence_said_with_more_energy_ends_with_an_exclamation_mark():
    # Whisper ran the three together; the cleanup wrote three sentences.
    voice = spoken(("first we met", False), ("we finally shipped it", True), ("then we went home", False))
    text = "First we met. We finally shipped it. Then we went home."
    assert exclaim(text, voice, NORM) == "First we met. We finally shipped it! Then we went home."
    sentences = written_sentences(text, voice, NORM)
    assert [sentence["text"] for sentence in sentences] == [
        "First we met.",
        "We finally shipped it.",
        "Then we went home.",
    ]
    assert [sentence["energy"] >= prosody.EXCLAIM_ENERGY for sentence in sentences] == [False, True, False]
    assert text[: sentences[1]["end"]].endswith("shipped it.")


def test_said_calmly_nothing_changes():
    voice = spoken(("that's great news", False))
    assert exclaim("That's great news.", voice, NORM) == "That's great news."


@pytest.mark.parametrize(
    "text",
    [
        # A question stays a question.
        "Did we finally ship it?",
        # A sentence left open keeps its ending: none.
        "We finally shipped it",
        # Cleanup rewrote it past finding.
        "The release is out.",
    ],
)
def test_only_a_period_said_with_energy_changes(text):
    voice = spoken(("did we finally ship it", True))
    assert exclaim(text, voice, NORM) == text


def test_no_exclamation_until_the_speaker_has_a_norm():
    voice = spoken(("we finally shipped it", True))
    assert exclaim("We finally shipped it.", voice, None) == "We finally shipped it."
    assert written_sentences("We finally shipped it.", voice, None) == []


def test_a_period_the_speaker_said_is_kept():
    voice = spoken(("we finally shipped it period", True))
    assert exclaim("We finally shipped it.", voice, NORM) == "We finally shipped it."


def test_an_abbreviation_does_not_end_the_sentence():
    voice = spoken(("i saw dr smith today and it went great", True))
    text = "I saw Dr. Smith today and it went great."
    assert exclaim(text, voice, NORM) == "I saw Dr. Smith today and it went great!"
    assert [sentence["text"] for sentence in written_sentences(text, voice, NORM)] == [text]


def test_the_mark_goes_inside_a_closing_quote():
    voice = spoken(("she said this is amazing", True))
    assert exclaim('She said "this is amazing."', voice, NORM) == 'She said "this is amazing!"'


@pytest.mark.parametrize(
    ("text", "written"),
    [
        # A file name's dot isn't a sentence end; the period after "now" is.
        ("Read privacy.md now.", "Read privacy.md now!"),
        # A number's or an ellipsis's dots never change.
        ("Step 3.", "Step 3."),
        ("Well...", "Well..."),
    ],
)
def test_only_a_period_after_a_word_changes(text, written):
    voice = spoken((text.replace(".", " "), True))
    assert exclaim(text, voice, NORM) == written


@pytest.mark.asyncio
async def test_the_returned_text_is_measured_and_saved_even_with_the_switch_off(monkeypatch):
    voice = spoken(("first we met", False), ("we finally shipped it", True))
    expression = Expression(16000)
    expression.phrase(Said(*[(word.text, word.start, word.end) for word in voice.words]), 0, released=True)
    hz = np.where(np.arange(210) >= 90, EXCITED_HZ, CALM_HZ)
    db = np.where(np.arange(210) >= 90, -14.0, -20.0)
    monkeypatch.setattr(expression.track, "frames", lambda: Frames(hz, db))
    monkeypatch.setattr(prosody, "load_baseline", lambda: NORM)
    saved = {}
    monkeypatch.setattr(prosody, "save", lambda capture_id, measured: saved.update({capture_id: measured}))
    # Delivered without "!": what the user later changes is learned against this.
    await prosody.measure_and_save("capture", expression, "First we met. We finally shipped it.")
    written = saved["capture"]["written"]
    assert [sentence["text"] for sentence in written] == ["First we met.", "We finally shipped it."]
    assert written[1]["energy"] >= prosody.EXCLAIM_ENERGY > written[0]["energy"]


def test_a_drawn_out_word_is_written_drawn_out_where_the_text_kept_it():
    frames, words = far_said()
    voice = Voice(frames, words)
    stretched = prosody.measure_voice(voice, None)["stretched"]
    assert stretch("That was really far away from the old house.", voice, stretched) == (
        "That was really faaaaar away from the old house."
    )
    # The cleanup wrote another word: nothing to draw out.
    assert stretch("That was really distant from the old house.", voice, stretched) == (
        "That was really distant from the old house."
    )
    # Already written drawn out: left as it is.
    assert stretch("That was really faaar away.", voice, stretched) == "That was really faaar away."


def test_every_content_word_shape_is_saved_to_learn_from():
    frames, words = far_said()
    measured = measure(frames, words, None)
    # Neither first nor last.
    assert [shape[0] for shape in measured["shapes"]] == ["was", "really", "far", "away", "from", "the", "old"]
    far = measured["shapes"][2]
    assert far[1:] == [measured["stretched"][0][key] for key in ("stretch", "louder", "plateau")]


def test_a_speakers_own_rule_can_draw_out_more():
    frames, words = far_said(louder=0.0)
    assert measure(frames, words, None)["stretched"] == []
    rule = prosody.StretchRule(louder_db=-1.0)
    voice = Voice(frames, words)
    assert [word["word"] for word in prosody.measure_voice(voice, None, rule)["stretched"]] == ["far"]


@pytest.mark.parametrize(
    "shape",
    [
        # Thinking drawls from real dictations, measured the same way.
        dict(word="name", stretch=3.54, louder=1.68, plateau=0.46),
        dict(word="forward", stretch=3.77, louder=2.57, plateau=0.35),
        dict(word="learn", stretch=3.81, louder=3.13, plateau=0.48),
        dict(word="seeing", stretch=5.45, louder=2.84, plateau=0.36),
        dict(word="mode", stretch=5.7, louder=7.2, plateau=0.45),
        dict(word="code", stretch=6.38, louder=4.03, plateau=0.44),
    ],
)
def test_a_thinking_drawl_is_not_drawn_out_by_default(shape):
    assert not prosody.StretchRule().holds(shape)


def test_a_closed_class_word_is_drawn_out_only_by_a_speakers_own_rule():
    shape = dict(word="so", stretch=8.0, louder=3.0, plateau=0.4)
    assert not prosody.StretchRule().holds(shape)
    assert prosody.StretchRule(words=frozenset(["so"])).holds(shape)


def test_energy_alone_agrees_with_the_full_stats():
    voice = spoken(("first we met", False), ("we finally shipped it", True))
    for start, end in [(0.0, 0.9), (0.9, 2.1), (0.0, 2.1)]:
        stats = voice.stats(start, end)
        assert voice.energy(start, end, NORM) == pytest.approx(
            NORM.energy(stats["level"], stats["span"], stats["loudness"])
        )
    assert voice.energy(0.0, 0.05, NORM) is None
