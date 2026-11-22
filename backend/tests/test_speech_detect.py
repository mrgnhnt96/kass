"""The voice detector that keeps Whisper off audio nobody spoke into."""

import wave
from pathlib import Path

import numpy as np
import pytest

from backend.services import speech_detect
from backend.services.speech_detect import SpeechDetector, has_speech

RATE = 48000
SOUNDS = Path(__file__).resolve().parents[2] / "app" / "sounds"


def room_noise(seconds, level=300, seed=0):
    return (np.random.default_rng(seed).normal(0, level, round(seconds * RATE))).astype(np.int16)


def voice(seconds, seed=1):
    """A crude voice: the harmonics of a wavering pitch, weighted by vowel
    resonances that glide from vowel to vowel, with a syllable rhythm.
    Silero hears it as clearly as real speech."""
    rng = np.random.default_rng(seed)
    t = np.arange(round(seconds * RATE)) / RATE
    pitch = 120 + 20 * np.sin(2 * np.pi * 3 * t)
    phase = 2 * np.pi * np.cumsum(pitch) / RATE
    first = 600 + 200 * np.sin(2 * np.pi * 2 * t)
    second = 1300 + 400 * np.sin(2 * np.pi * 1.3 * t)
    signal = np.zeros_like(t)
    for k in range(1, 40):
        f = k * pitch
        gain = (
            np.exp(-(((f - first) / 150) ** 2))
            + 0.6 * np.exp(-(((f - second) / 200) ** 2))
            + 0.2 * np.exp(-(((f - 2600) / 300) ** 2))
            + 0.02
        )
        signal += gain * np.sin(k * phase)
    syllables = np.clip(np.sin(2 * np.pi * 4 * t), 0, 1) ** 0.5
    signal = signal * syllables + rng.normal(0, 0.01, len(t))
    return (signal / np.abs(signal).max() * 12000).astype(np.int16)


@pytest.fixture(autouse=True)
def detector_loads():
    assert speech_detect.load() is not None


def test_silence_and_room_noise_are_not_speech():
    assert not has_speech(np.zeros(RATE, dtype=np.int16), RATE)
    assert not has_speech(room_noise(2), RATE)


def test_a_voice_is_speech():
    assert has_speech(np.concatenate([room_noise(0.5), voice(1.5), room_noise(0.5)]), RATE)


def start_cue():
    """The bundled dictation start cue, at 48 kHz."""
    with wave.open(str(SOUNDS / "start.wav")) as w:
        rate = w.getframerate()
        cue = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32)
    return np.interp(np.arange(0, len(cue), rate / RATE), np.arange(len(cue)), cue)


def loud_chime():
    """The app's first start cue: a bright 220 ms C major chord with a 5 ms
    attack, which Silero hears as a voice."""
    t = np.arange(round(0.22 * RATE)) / RATE
    chord = sum(np.sin(2 * np.pi * f * t) + 0.15 * np.sin(4 * np.pi * f * t) for f in (523.25, 659.25, 783.99)) / 3
    return chord * np.minimum(1, t / 0.005) * np.exp(-t * 9) * 0.35 * 32767


def with_cue(take, mic_open_ms, cue=None):
    """``take`` with a cue (the start cue unless given) picked up by the
    microphone ``mic_open_ms`` after it began, as loud as the file itself."""
    cue = start_cue() if cue is None else cue
    heard = cue[round(mic_open_ms * RATE / 1000) :]
    mixed = take.astype(np.float32)
    mixed[: len(heard)] += heard
    return np.clip(mixed, -32768, 32767).astype(np.int16)


START_CUE_SPAN = RATE * 300 // 1000  # sound_cues::START_CUE_SPAN_MS


@pytest.mark.parametrize("mic_open_ms", [20, 100, 160])
def test_a_voice_where_the_start_cue_can_be_does_not_count(mic_open_ms):
    take = with_cue(room_noise(2, level=30), mic_open_ms, loud_chime())
    # A loud chime reads as a voice to Silero...
    unaware = SpeechDetector(RATE)
    unaware.feed(take)
    assert unaware.heard(0, len(take))
    # ...so a voice where the cue can be doesn't count.
    detector = SpeechDetector(RATE, ignore_before=START_CUE_SPAN)
    detector.feed(take)
    assert not detector.heard(0, len(take))


@pytest.mark.parametrize("mic_open_ms", [20, 100, 160])
def test_the_start_cue_in_a_silent_take_is_not_speech(mic_open_ms):
    take = with_cue(room_noise(2, level=30), mic_open_ms)
    detector = SpeechDetector(RATE, ignore_before=START_CUE_SPAN)
    detector.feed(take)
    assert not detector.heard(0, len(take))


def test_speech_after_the_start_cue_is_heard():
    take = with_cue(np.concatenate([room_noise(0.3, level=30), voice(1.0)]), 0)
    detector = SpeechDetector(RATE, ignore_before=START_CUE_SPAN)
    detector.feed(take)
    assert detector.heard(0, len(take))
    assert not detector.heard(0, START_CUE_SPAN)


def test_float_audio_at_16k_is_checked_too():
    audio = np.concatenate([room_noise(0.5), voice(1.5)]).astype(np.float32)[::3] / 32768
    assert has_speech(audio, 16000)
    assert not has_speech(room_noise(1).astype(np.float32)[::3] / 32768, 16000)


@pytest.mark.parametrize("rate", [16000, 24000, 44100, 48000])
def test_streamed_audio_says_where_the_voice_was(rate):
    silence = np.zeros(rate, dtype=np.int16)
    spoken = voice(1.5)
    spoken = np.interp(np.arange(0, len(spoken), RATE / rate), np.arange(len(spoken)), spoken).astype(np.int16)
    audio = np.concatenate([silence, spoken, silence])
    detector = SpeechDetector(rate)
    for offset in range(0, len(audio), rate // 10):
        detector.feed(audio[offset : offset + rate // 10])
    assert not detector.heard(0, rate // 2)
    assert detector.heard(rate, rate + len(spoken))
    assert not detector.heard(len(audio) - rate // 2, len(audio))


def test_quiet_counts_the_audio_since_the_last_voice():
    rate = 16000
    detector = SpeechDetector(rate)
    detector.feed(np.zeros(rate, dtype=np.int16))
    assert detector.quiet() >= rate * 0.95  # all but a partial window
    detector.feed(voice(1.5)[::3])
    after_voice = detector.quiet()
    assert after_voice < rate * 1.5  # counted from the voice, not the start
    detector.feed(np.zeros(rate, dtype=np.int16))
    assert abs(detector.quiet() - after_voice - rate) <= speech_detect.WINDOW


def test_without_the_model_everything_counts_as_speech(monkeypatch):
    monkeypatch.setattr(speech_detect, "_session", None)
    monkeypatch.setattr(speech_detect, "_unavailable", True)
    assert has_speech(np.zeros(RATE, dtype=np.int16), RATE)
    assert SpeechDetector(RATE).heard(0, RATE)
    # And no pause is ever found, so phrases are all recognized at finish.
    detector = SpeechDetector(RATE)
    detector.feed(np.zeros(RATE, dtype=np.int16))
    assert detector.quiet() == 0


def style_cue(gain=1.0):
    """The bundled style cue, at 48 kHz."""
    with wave.open(str(SOUNDS / "style.wav")) as w:
        rate = w.getframerate()
        cue = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16).astype(np.float32)
    return np.interp(np.arange(0, len(cue), rate / RATE), np.arange(len(cue)), cue) * gain


STYLE_CUE_SPAN = RATE * 350 // 1000  # sound_cues::STYLE_CUE_SPAN_MS


def during(take, at_s, sound):
    mixed = take.astype(np.float32)
    start = round(at_s * RATE)
    mixed[start : start + len(sound)] += sound[: len(mixed) - start]
    return np.clip(mixed, -32768, 32767).astype(np.int16)


def test_the_style_cue_alone_is_not_a_voice():
    # Unlike the first start cue: short, high and soft, even picked up loud.
    for gain in (1.0, 4.0, 8.0):
        detector = SpeechDetector(RATE)
        detector.feed(during(room_noise(1.0, level=30), 0.3, style_cue(gain)))
        assert not detector.heard(0, RATE)


@pytest.mark.parametrize("gain", [1.0, 4.0])
def test_the_style_cue_in_a_pause_is_not_a_voice_once_marked(gain):
    # "Make this formal", a pause with the cue in it, then nothing.
    take = during(np.concatenate([voice(1.2), room_noise(1.0, level=30)]), 1.3, style_cue(gain))
    cue_at = round(1.3 * RATE)
    detector = SpeechDetector(RATE)
    detector.feed(take[:cue_at])
    # The app says where the cue is while the audio is still arriving.
    detector.ignore(cue_at - RATE // 20, cue_at + STYLE_CUE_SPAN)
    detector.feed(take[cue_at:])
    assert detector.heard(0, RATE)
    assert not detector.heard(round(1.25 * RATE), len(take))


def test_marking_the_cue_drops_a_voice_already_found_there():
    take = during(room_noise(1.0, level=30), 0.3, voice(0.3).astype(np.float32))
    detector = SpeechDetector(RATE)
    detector.feed(take)
    assert detector.heard(0, len(take))
    detector.ignore(round(0.3 * RATE), round(0.3 * RATE) + STYLE_CUE_SPAN)
    assert not detector.heard(0, len(take))


def test_a_voice_after_the_cue_still_counts():
    take = np.concatenate([voice(1.0), room_noise(0.4, level=30), voice(1.0)])
    detector = SpeechDetector(RATE)
    detector.ignore(RATE, RATE + STYLE_CUE_SPAN)
    detector.feed(during(take, 1.0, style_cue()))
    assert detector.heard(round(1.4 * RATE), len(take))
