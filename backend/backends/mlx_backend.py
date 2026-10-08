"""
MLX backend implementation for Whisper STT using mlx-audio.
"""

import logging
import re
from collections.abc import Sequence

import numpy as np

logger = logging.getLogger(__name__)

# PATCH: Import and apply offline patch BEFORE any huggingface_hub usage
# This prevents mlx_audio from making network requests when models are cached
from ..utils.hf_offline_patch import patch_huggingface_hub_offline

patch_huggingface_hub_offline()

from ..services import dictionary, speech_detect
from ..services.mlx_thread import clear_mlx_cache, run_on_mlx_thread
from ..services.refinement import strip_stt_artifacts
from . import WHISPER_HF_REPOS, mlx_whisper_loader, whisper_audio, word_timing
from .base import (
    ellipsis_token_ids,
    is_model_cached,
    local_model_path,
    model_load_progress,
)

# Whisper keeps only the last tokens of its prompt (mlx-audio decoding, n_ctx // 2 - 1).
PROMPT_TOKENS = 223


def phrase_prompt(tokenizer, terms: str, previous_text: str | None) -> str | None:
    """Whisper's prompt: the dictionary terms, then as much earlier text as fits.

    Whisper drops a long prompt's first tokens, which would drop the terms;
    the earlier text is cut from its start instead. It stays last, so a
    phrase still continues its sentence.
    """
    previous = (previous_text or "").strip()
    if not terms:
        return previous or None
    if not previous:
        return terms
    # One token of slack: the two parts may tokenize a little differently joined.
    room = PROMPT_TOKENS - len(tokenizer.encode(" " + terms)) - 1
    tokens = tokenizer.encode(" " + previous)
    if len(tokens) > room:
        previous = tokenizer.decode(tokens[len(tokens) - room :]).strip()
    return f"{terms} {previous}"


# Sound this far above the phrase's quietest stretches is something said. Not
# a voice detector: Silero missed a word a built-in microphone heard from
# across the desk (17 dB under the headset's voice that followed).
OPENING_LOUDER_DB = 15
# This much sound before the first word means Whisper skipped words.
OPENING_SKIPPED_S = 0.2
# Attention may start a word a little late.
OPENING_SLACK_S = 0.15
_LEVEL_WINDOW = 320  # 20 ms at 16 kHz


def skipped_opening(audio: np.ndarray, first_word_start: float) -> bool:
    """Whether 16 kHz ``audio`` has sound before the first word Whisper wrote.

    With earlier text in its prompt, Whisper may leave out opening words that
    repeat its end ("Another one" said again after "Another one"); the text
    then starts late in the audio.
    """
    count = len(audio) // _LEVEL_WINDOW
    before = min(count, int((first_word_start - OPENING_SLACK_S) * whisper_audio.SAMPLE_RATE) // _LEVEL_WINDOW)
    if before <= 0:
        return False
    windows = np.asarray(audio[: count * _LEVEL_WINDOW], dtype=np.float32).reshape(count, _LEVEL_WINDOW)
    levels = 20 * np.log10(np.sqrt((windows**2).mean(axis=1)) + 1e-10)
    floor = np.percentile(levels, 10)
    loud = int((levels[:before] >= floor + OPENING_LOUDER_DB).sum())
    return loud * _LEVEL_WINDOW / whisper_audio.SAMPLE_RATE >= OPENING_SKIPPED_S


def adds_opening(prompted: str, unprompted: str) -> bool:
    """Whether ``unprompted`` is ``prompted`` with words in front: the same
    phrase heard without the earlier text, opening included."""
    before = re.findall(r"\w+", prompted.casefold())
    after = re.findall(r"\w+", unprompted.casefold())
    if len(after) <= len(before):
        return False
    # The word where the prompted text started may differ ("that" for "it").
    tail = after[len(after) - len(before) :]
    same = sum(a == b for a, b in zip(tail, before, strict=True))
    return same >= max(1, len(before) - 1)


def vocabulary_decoder(tokenizer):
    """Decode many token sequences in one call, or None if unavailable.

    mlx-audio's tokenizer wraps a Hugging Face fast tokenizer; its Rust
    backend decodes a batch without a Python round trip per token. Special
    tokens are kept, as the wrapper's own ``decode`` keeps them.
    """
    backend = getattr(getattr(tokenizer, "hf_tokenizer", None), "backend_tokenizer", None)
    if backend is None or not hasattr(backend, "decode_batch"):
        return None
    return lambda sequences: backend.decode_batch(sequences, skip_special_tokens=False)


def _voice_adapter(model_size: str) -> str | None:
    """The trained voice adapter for ``model_size``, if voice training activated one."""
    try:
        from ..services.model_improvement.manager import voice_adapter

        return voice_adapter(model_size)
    except Exception:
        logger.exception("Couldn't read the voice model; using plain weights")
        return None


class MLXSTTBackend:
    """MLX-based STT backend using mlx-audio Whisper."""

    def __init__(self, model_size: str = "base"):
        self.model = None
        self.model_size = model_size
        # The voice adapter merged into the loaded weights (voice_training/).
        self.adapter = None
        # The prompt each dictionary's terms fit into, for the loaded model.
        self._term_prompts: dict[tuple[str, ...], str] = {}
        # The loaded model's alignment heads, for word timings (word_timing.py).
        self.alignment_heads = None

    def is_loaded(self) -> bool:
        """Check if model is loaded."""
        return self.model is not None

    def _is_model_cached(self, model_size: str) -> bool:
        hf_repo = WHISPER_HF_REPOS.get(model_size, f"openai/whisper-{model_size}")
        return is_model_cached(hf_repo, weight_extensions=(".safetensors", ".bin", ".npz"))

    def _ensure_loaded_sync(self, model_size: str | None):
        """Load the model if the requested size isn't already resident.

        Runs on the MLX worker thread so it stays serialized with transcription.
        """
        if model_size is None:
            model_size = self.model_size
        adapter = _voice_adapter(model_size)

        if self.model is not None and self.model_size == model_size and self.adapter == adapter:
            return

        self._load_model_sync(model_size, adapter)

    async def load_model_async(self, model_size: str | None = None):
        """
        Lazy load the MLX Whisper model.

        Args:
            model_size: Model size (tiny, base, small, medium, large)
        """
        await run_on_mlx_thread(self._ensure_loaded_sync, model_size)

    # Alias for compatibility
    load_model = load_model_async

    async def unload(self):
        """Free the model, serialized onto the MLX worker thread."""
        await run_on_mlx_thread(self.unload_model)

    def _load_model_sync(self, model_size: str, adapter: str | None = None):
        """Synchronous model loading, with the voice adapter merged in when there is one."""
        progress_model_name = f"whisper-{model_size}"
        is_cached = self._is_model_cached(model_size)

        with model_load_progress(progress_model_name, is_cached):
            model_name = WHISPER_HF_REPOS.get(model_size, f"openai/whisper-{model_size}")
            logger.info("Loading MLX Whisper model %s...", model_size)
            model_dir = local_model_path(model_name, (".safetensors", ".bin", ".npz"))

            # mlx_audio.stt.load, minus imports Whisper never uses; they were
            # most of the packaged server's startup load time.
            self.model = mlx_whisper_loader.load_whisper(model_dir)
            self.alignment_heads = word_timing.alignment_heads(model_dir)
            if adapter:
                try:
                    from ..services.voice_training import lora

                    lora.apply(self.model, adapter)
                except Exception:
                    logger.exception("The voice model failed to load; using plain %s", model_size)
                    from ..services.model_improvement.manager import quarantine_voice

                    quarantine_voice("The trained voice model failed to load and was turned off.", adapter)
                    self.model = mlx_whisper_loader.load_whisper(
                        local_model_path(model_name, (".safetensors", ".bin", ".npz"))
                    )
                    adapter = None

        self.model_size = model_size
        self.adapter = adapter
        self._term_prompts = {}
        logger.info("MLX Whisper model %s loaded successfully", model_size)

    def unload_model(self):
        """Unload the model to free memory."""
        if self.model is not None:
            del self.model
            self.model = None
            clear_mlx_cache()
            logger.info("MLX Whisper model unloaded")

    async def transcribe(
        self,
        audio_path: str,
        language: str | None = None,
        model_size: str | None = None,
        previous_text: str | None = None,
        check_speech: bool = True,
        vocabulary: Sequence[str] = (),
    ) -> str:
        """
        Transcribe an audio file to text.

        Args:
            audio_path: Path to audio file
            language: Optional language hint
            model_size: Optional model size override
            previous_text: Earlier dictation text when transcribing one phrase
            check_speech: Return "" without running Whisper when no voice is
                detected; False when the caller already checked
            vocabulary: Dictionary terms, most important first

        Returns:
            Transcribed text
        """
        # Decoded here rather than by mlx-audio, whose resampler would import
        # scipy.signal on the first file that is not 16 kHz.
        return await self._transcribe(
            lambda: whisper_audio.read_audio_file(audio_path),
            language,
            model_size,
            previous_text,
            check_speech,
            vocabulary,
        )

    async def transcribe_array(
        self,
        samples: np.ndarray,
        sample_rate: int,
        language: str | None = None,
        model_size: str | None = None,
        previous_text: str | None = None,
        check_speech: bool = True,
        vocabulary: Sequence[str] = (),
        alignments: list | None = None,
    ) -> str:
        """
        Transcribe in-memory audio to text, without a temporary file.

        Args:
            samples: Mono int16 PCM, or float audio scaled to [-1, 1]; shape
                (n,) or (n, channels)
            sample_rate: Sample rate of ``samples`` in Hz (resampled to 16 kHz)
            language: Optional language hint
            model_size: Optional model size override
            previous_text: Earlier dictation text when transcribing one phrase
            check_speech: Return "" without running Whisper when no voice is
                detected; False when the caller already checked
            vocabulary: Dictionary terms, most important first
            alignments: When given, the decode's ``word_timing.Alignment`` is
                appended (None when there is none), for word times computed
                later off the MLX thread. The text is unchanged.

        Returns:
            Transcribed text, identical to ``transcribe`` of the same audio
            written to a WAV file
        """
        samples = np.asarray(samples)
        if samples.size == 0:
            raise ValueError("No audio samples to transcribe")
        return await self._transcribe(
            lambda: whisper_audio.prepare_samples(samples, sample_rate),
            language,
            model_size,
            previous_text,
            check_speech,
            vocabulary,
            alignments,
        )

    def _alignment(self, harvested, result, language, samples: int):
        """The decode's word alignment, or None (several windows, an unspaced language)."""
        try:
            if (language or "en") in word_timing.UNSPACED:
                return None
            segments = getattr(result, "segments", None) or []
            if any(segment.get("seek", 0) for segment in segments):
                return None
            tokenizer = self.model.get_tokenizer(language=language or "en")
            text_tokens = [token for segment in segments for token in segment["tokens"] if token < tokenizer.eot]
            return word_timing.alignment(harvested, text_tokens, samples // 160, tokenizer)
        except Exception:
            # Timings only measure; the text never waits on or fails for them.
            logger.exception("Couldn't keep the word alignment")
            return None

    def _terms_prompt(self, tokenizer, vocabulary: Sequence[str]) -> str:
        """The terms that fit Whisper's share of the prompt, as it reads them."""
        key = tuple(vocabulary)
        if (cached := self._term_prompts.get(key)) is None:
            fit, _ = dictionary.fit_terms(key, lambda term: len(tokenizer.encode(" " + term)))
            cached = dictionary.prompt(fit)
            if len(self._term_prompts) >= 32:
                self._term_prompts.clear()
            self._term_prompts[key] = cached
        return cached

    async def _transcribe(
        self, prepare_audio, language, model_size, previous_text, check_speech=True, vocabulary=(), alignments=None
    ) -> str:
        def _transcribe_sync():
            audio = prepare_audio()
            # Whisper invents text ("Thank you.") for audio without a voice.
            if check_speech and not speech_detect.has_speech(np.asarray(audio), whisper_audio.SAMPLE_RATE):
                return ""

            decode_options = {}
            if language:
                decode_options["language"] = language
            tokenizer = None
            if previous_text is not None or vocabulary:
                tokenizer = self.model.get_tokenizer(language=language or "en")
            if previous_text is not None:
                decode_options["suppress_tokens"] = [
                    -1,
                    *ellipsis_token_ids(
                        self.model_size, tokenizer.decode, tokenizer.eot, vocabulary_decoder(tokenizer)
                    ),
                ]
            terms = self._terms_prompt(tokenizer, vocabulary) if vocabulary else ""
            if prompt := phrase_prompt(tokenizer, terms, previous_text):
                decode_options["initial_prompt"] = prompt

            # Inference runs with the process's default HF_HUB_OFFLINE
            # state — see the comment in MLXTTSBackend.generate for the
            # regression this revert fixes (issue #462).
            def decode(options):
                if not self.alignment_heads or (alignments is None and not previous_text):
                    return self.model.generate(audio, **options), None
                with word_timing.harvest(self.alignment_heads) as harvested:
                    result = self.model.generate(audio, **options)
                return result, self._alignment(harvested, result, language, len(audio))

            result, alignment = decode(decode_options)
            text = _result_text(result)
            words = alignment.words() if previous_text and alignment else []
            if words and skipped_opening(np.asarray(audio), words[0].start):
                options = {key: value for key, value in decode_options.items() if key != "initial_prompt"}
                if terms:
                    options["initial_prompt"] = terms
                retried, retried_alignment = decode(options)
                if adds_opening(text, _result_text(retried)):
                    logger.info("Recognized a phrase again without the earlier text, which hid its opening words")
                    alignment, text = retried_alignment, _result_text(retried)
            if alignments is not None and self.alignment_heads:
                alignments.append(alignment)
            return strip_stt_artifacts(text.strip())

        # Load-if-needed and transcription run as one job on the MLX worker so
        # a concurrent unload or load can't land between them.
        def _load_and_transcribe():
            self._ensure_loaded_sync(model_size)
            return _transcribe_sync()

        return await run_on_mlx_thread(_load_and_transcribe)


def _result_text(result) -> str:
    if isinstance(result, str):
        return result
    if isinstance(result, dict):
        return result.get("text", "")
    if hasattr(result, "text"):
        return result.text
    return str(result)
