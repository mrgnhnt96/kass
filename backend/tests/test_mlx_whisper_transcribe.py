"""MLX Whisper takes audio in memory, and loads without word-timing imports.

The model is faked, so these exercise the backend's own orchestration: what
reaches ``model.generate`` for a file versus in-memory samples.
"""

import platform
import sys
import wave
from types import SimpleNamespace

import numpy as np
import pytest

from backend.services.mlx_thread import run_on_mlx_thread

pytestmark = pytest.mark.skipif(
    not (sys.platform == "darwin" and platform.machine() == "arm64"),
    reason="MLX is only installed on Apple Silicon macOS",
)


class FakeWhisper:
    def __init__(self):
        self.calls = []

    def generate(self, audio, **options):
        # Runs on the MLX worker; arrays made there must be read there too.
        self.calls.append((type(audio).__name__, np.array(audio), options))
        return SimpleNamespace(text="  hello there  ")

    def get_tokenizer(self, language="en"):
        return SimpleNamespace(decode=lambda tokens: "", eot=50257)


@pytest.fixture
def stt(monkeypatch):
    from backend.backends import mlx_backend

    monkeypatch.setattr(mlx_backend, "ellipsis_token_ids", lambda size, decode, eot, decode_batch=None: [1131])
    # A test tone isn't a voice; the detector has its own tests.
    monkeypatch.setattr(mlx_backend.speech_detect, "has_speech", lambda samples, rate: True)
    backend = mlx_backend.MLXSTTBackend("turbo")
    backend.model = FakeWhisper()
    return backend


def pcm(rate, seconds):
    t = np.arange(int(rate * seconds)) / rate
    return (np.sin(2 * np.pi * 330 * t) * 12000).astype(np.int16)


def write_wav(path, samples, rate):
    with wave.open(str(path), "wb") as audio:
        audio.setnchannels(1)
        audio.setsampwidth(2)
        audio.setframerate(rate)
        audio.writeframes(samples.tobytes())


@pytest.mark.asyncio
async def test_transcribe_array_passes_the_waveform_not_a_path(stt):
    from backend.backends import whisper_audio

    samples = pcm(48000, 0.5)

    text = await stt.transcribe_array(samples, 48000, "en", "turbo")

    assert text == "hello there"
    kind, audio, options = stt.model.calls[0]
    assert kind == "array"
    expected = await run_on_mlx_thread(lambda: np.array(whisper_audio.prepare_samples(samples, 48000)))
    np.testing.assert_array_equal(audio, expected)
    assert options == {"language": "en"}


@pytest.mark.asyncio
async def test_transcribe_array_uses_the_same_phrase_options_as_files(stt, tmp_path):
    samples = pcm(16000, 0.5)
    write_wav(tmp_path / "w.wav", samples, 16000)

    await stt.transcribe(str(tmp_path / "w.wav"), "en", "turbo", previous_text="We met and")
    await stt.transcribe_array(samples, 16000, "en", "turbo", previous_text="We met and")

    (_, file_audio, file_options), (_, array_audio, array_options) = stt.model.calls
    assert (
        array_options
        == file_options
        == {
            "language": "en",
            "suppress_tokens": [-1, 1131],
            "initial_prompt": "We met and",
        }
    )
    np.testing.assert_array_equal(array_audio, file_audio)


@pytest.mark.asyncio
async def test_transcribe_file_is_decoded_in_process(stt, tmp_path):
    """File input is decoded here, so a 48 kHz file never needs scipy.signal."""
    from backend.backends import whisper_audio

    path = str(tmp_path / "w.wav")
    write_wav(path, pcm(48000, 0.5), 48000)

    await stt.transcribe(path, None, "turbo")

    kind, audio, options = stt.model.calls[0]
    assert kind == "array"
    expected = await run_on_mlx_thread(lambda: np.array(whisper_audio.read_audio_file(path)))
    np.testing.assert_array_equal(audio, expected)
    assert options == {}


@pytest.mark.asyncio
async def test_audio_without_a_voice_never_reaches_whisper(stt, monkeypatch, tmp_path):
    from backend.backends import mlx_backend

    heard = []

    def has_speech(samples, rate):
        heard.append(rate)
        return False

    monkeypatch.setattr(mlx_backend.speech_detect, "has_speech", has_speech)
    write_wav(tmp_path / "w.wav", pcm(48000, 0.5), 48000)

    assert await stt.transcribe_array(pcm(48000, 0.5), 48000, "en", "turbo") == ""
    assert await stt.transcribe(str(tmp_path / "w.wav"), "en", "turbo") == ""
    # Checked on Whisper's own 16 kHz input.
    assert heard == [16000, 16000]
    assert stt.model.calls == []
    # A caller that already checked, such as a streaming dictation, skips it.
    assert await stt.transcribe_array(pcm(48000, 0.5), 48000, "en", "turbo", check_speech=False) == "hello there"
    assert len(heard) == 2


@pytest.mark.asyncio
async def test_empty_samples_are_rejected_before_inference(stt):
    with pytest.raises(ValueError, match="No audio samples"):
        await stt.transcribe_array(np.zeros(0, dtype=np.int16), 16000, "en", "turbo")
    assert stt.model.calls == []


def test_load_goes_through_the_lean_whisper_loader(monkeypatch):
    from backend.backends import mlx_backend, mlx_whisper_loader

    loaded = []
    monkeypatch.setattr(mlx_whisper_loader, "load_whisper", lambda repo: loaded.append(repo) or FakeWhisper())
    # Not downloaded: the repo name, for the library to fetch.
    monkeypatch.setattr(mlx_backend, "local_model_path", lambda repo, extensions: repo)
    backend = mlx_backend.MLXSTTBackend("base")

    backend._load_model_sync("turbo")

    assert loaded == ["openai/whisper-large-v3-turbo"]
    assert isinstance(backend.model, FakeWhisper)
    assert backend.model_size == "turbo"


def test_a_downloaded_whisper_loads_from_its_cached_folder(monkeypatch):
    from backend.backends import mlx_backend, mlx_whisper_loader

    loaded = []
    monkeypatch.setattr(mlx_whisper_loader, "load_whisper", lambda path: loaded.append(path) or FakeWhisper())
    monkeypatch.setattr(mlx_backend, "local_model_path", lambda repo, extensions: f"/cache/{repo}")

    mlx_backend.MLXSTTBackend("base")._load_model_sync("turbo")

    assert loaded == ["/cache/openai/whisper-large-v3-turbo"]


class WordTokenizer:
    """One token per word, with the leading space Whisper's tokenizer keeps."""

    def encode(self, text):
        return text.split()

    def decode(self, tokens):
        return " ".join(tokens)


def test_dictionary_terms_come_first_and_earlier_text_last():
    from backend.backends.mlx_backend import phrase_prompt

    assert phrase_prompt(WordTokenizer(), "Kubernetes, Zed.", "and then we") == "Kubernetes, Zed. and then we"
    assert phrase_prompt(WordTokenizer(), "Kubernetes, Zed.", "") == "Kubernetes, Zed."
    assert phrase_prompt(WordTokenizer(), "", "and then we") == "and then we"
    assert phrase_prompt(WordTokenizer(), "", "") is None


def test_long_earlier_text_is_cut_from_its_start_so_the_terms_survive():
    from backend.backends.mlx_backend import PROMPT_TOKENS, phrase_prompt

    earlier = " ".join(f"w{i}" for i in range(400))

    prompt = phrase_prompt(WordTokenizer(), "Kubernetes, Zed.", earlier)

    assert prompt.startswith("Kubernetes, Zed. ")
    assert prompt.endswith("w399")
    assert len(prompt.split()) <= PROMPT_TOKENS


@pytest.mark.asyncio
async def test_whisper_is_prompted_with_the_terms_that_fit(stt, monkeypatch):
    from backend.backends import mlx_backend

    monkeypatch.setattr(mlx_backend.dictionary, "PROMPT_TOKENS", 6)
    tokenizer = SimpleNamespace(decode=lambda tokens: "", eot=50257, encode=lambda text: text.split())
    stt.model.get_tokenizer = lambda language="en": tokenizer

    await stt.transcribe_array(
        pcm(16000, 0.5), 16000, "en", "turbo", previous_text="we met", vocabulary=["Zed", "Kubernetes", "Tailscale"]
    )
    await stt.transcribe_array(pcm(16000, 0.5), 16000, "en", "turbo", vocabulary=["Zed"])

    (_, _, phrase), (_, _, whole) = stt.model.calls
    # 1 for the period, 2 per one-word term: two fit in 6.
    assert phrase["initial_prompt"] == "Zed, Kubernetes. we met"
    assert whole == {"language": "en", "initial_prompt": "Zed."}


def take(word_at=None, seconds=1.6, rate=16000):
    """Room noise at -66 dB, with a quiet 0.4 s word (-42 dB) from ``word_at``."""
    rng = np.random.default_rng(0)
    audio = rng.normal(0, 10 ** (-66 / 20), int(rate * seconds)).astype(np.float32)
    if word_at is not None:
        start = int(word_at * rate)
        t = np.arange(int(0.4 * rate)) / rate
        audio[start : start + len(t)] += (np.sin(2 * np.pi * 220 * t) * 10 ** (-42 / 20) * 1.41).astype(np.float32)
    return audio


def test_sound_before_the_first_word_is_a_skipped_opening():
    from backend.backends.mlx_backend import skipped_opening

    # "Another one" at the start, the text starting at 0.8 s.
    assert skipped_opening(take(word_at=0.0), 0.8)
    # The text starts with the sound.
    assert not skipped_opening(take(word_at=0.0), 0.05)
    # Only room noise before the first word.
    assert not skipped_opening(take(), 0.8)
    # A sound too short to be words.
    assert not skipped_opening(take(word_at=0.6), 0.8)


def test_an_unprompted_decode_counts_only_when_it_adds_opening_words():
    from backend.backends.mlx_backend import adds_opening

    assert adds_opening("that still is not working very well.", "Another one. It still is not working very well.")
    assert not adds_opening("And here's another one", "And here's another one.")
    assert not adds_opening("We met on Tuesday", "Okay so we went home early")


class PromptedWhisper(FakeWhisper):
    """Leaves out "Another one" when the earlier text ends with it."""

    def generate(self, audio, **options):
        self.calls.append((type(audio).__name__, np.array(audio), options))
        prompted = options.get("initial_prompt", "").endswith("Another one")
        return SimpleNamespace(
            text=" that still works" if prompted else " Another one. It still works",
            segments=[],
            prompted=prompted,
        )


@pytest.fixture
def aligned(stt, monkeypatch):
    """A backend whose alignment starts the prompted text at 0.8 s."""
    import contextlib

    from backend.backends import mlx_backend, word_timing

    stt.model = PromptedWhisper()
    stt.alignment_heads = [(0, 0)]
    monkeypatch.setattr(word_timing, "harvest", lambda heads: contextlib.nullcontext(None))

    def alignment(harvested, result, language, samples):
        start = 0.8 if result.prompted else 0.0
        return SimpleNamespace(words=lambda: [word_timing.Word("x", start, start + 0.2)], prompted=result.prompted)

    monkeypatch.setattr(stt, "_alignment", alignment)
    monkeypatch.setattr(mlx_backend.whisper_audio, "prepare_samples", lambda samples, rate: take(word_at=0.0))
    return stt


@pytest.mark.asyncio
async def test_words_hidden_by_the_earlier_text_are_recognized_again(aligned):
    alignments = []

    text = await aligned.transcribe_array(
        pcm(16000, 1.6), 16000, "en", "turbo", previous_text="Here's another try. Another one", alignments=alignments
    )

    assert text == "Another one. It still works"
    first, second = (options for _, _, options in aligned.model.calls)
    assert first["initial_prompt"] == "Here's another try. Another one"
    assert "initial_prompt" not in second
    # The word times are the retried decode's.
    assert [alignment.prompted for alignment in alignments] == [False]


@pytest.mark.asyncio
async def test_a_phrase_whose_text_starts_with_its_sound_is_recognized_once(aligned):
    text = await aligned.transcribe_array(pcm(16000, 1.6), 16000, "en", "turbo", previous_text="We met and")

    assert text == "Another one. It still works"
    assert len(aligned.model.calls) == 1
