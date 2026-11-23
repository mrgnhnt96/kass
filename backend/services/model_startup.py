"""Load the dictation models the app asks for when it starts the worker.

The Rust server sends the saved settings and the cleanup prompts its
dictations will send (``app/server/src/startup.rs``), so the worker warms the
same prompt caches dictation uses and doesn't read kass.db for them.
"""

import asyncio
import logging
import time

import numpy as np

from .. import config
from . import llm, shared_adapters, speech_detect, transcribe
from .mlx_thread import keep_weights_resident, run_on_mlx_thread

logger = logging.getLogger(__name__)
# The other styles' prompts, cached after startup (docs/plans/PER_APP_STYLE.md).
_style_warmup: asyncio.Task | None = None
WARM_RATE = 48000
# Enough real speech to run Whisper's full decode, short enough to keep
# startup quick.
WARM_SECONDS = 5
# A cleanup of "okay" is a few tokens.
WARM_TOKENS = 16


async def load_startup_models(startup) -> None:
    """Load installed capture models, leaving missing downloads to setup.

    ``startup`` is the app's request (``routes.sync.Startup``): the saved
    models, and the cleanup prompts, the default style's first.
    """
    # Before Whisper loads: turbo merges the shared voice adapter when it's on.
    shared_adapters._enabled = startup.shared_adapters
    stt_size = startup.stt_model
    llm_size = startup.llm_model
    language = None if startup.language in (None, "auto") else startup.language

    try:
        await run_on_mlx_thread(keep_weights_resident)
    except Exception:
        logger.exception("Could not keep model weights resident")

    selected = [("Whisper", transcribe.get_whisper_model, stt_size)]
    if startup.auto_refine:
        selected.append(("refinement", llm.get_llm_model, llm_size))

    for name, get_backend, size in selected:
        try:
            backend = get_backend()
            if not backend._is_model_cached(size):
                logger.info("Skipping startup load for %s %s: not downloaded", name, size)
                continue
            started = time.monotonic()
            logger.info("Loading %s %s for startup", name, size)
            await backend.load_model(size)
            logger.info("Startup %s %s loaded in %.3fs", name, size, time.monotonic() - started)
        except Exception:
            # Keep setup and diagnostics available if an installed model fails.
            logger.exception("Could not load startup %s model %s", name, size)

    # The voice detector every dictation checks its audio with.
    await asyncio.to_thread(speech_detect.load)
    await warm_whisper(stt_size, language)
    if startup.auto_refine and startup.prompts:
        first, *others = startup.prompts
        await warm_refinement(first)
        global _style_warmup
        _style_warmup = asyncio.create_task(warm_styles(others))


async def warm_whisper(stt_size: str, language: str | None) -> None:
    """Run Whisper once during startup so the first dictation is warm.

    Loading Whisper doesn't run it; the first real transcription otherwise
    pays one-time setup after the user lets go of the keys: ~0.3 s for the
    first run, and ~0.25 s more to find the ellipsis tokens that dictation
    phrases suppress (see ``ellipsis_token_ids``). Faint
    noise barely decodes anything, so the user's most recent recording is
    used when there is one; its text is discarded.
    """
    backend = transcribe.get_whisper_model()
    if not backend.is_loaded():
        return
    try:
        started = time.monotonic()
        samples, rate = _warm_audio()
        # Called the way streaming recognizes a dictation's first phrase
        # (previous_text=""), which builds the phrase options, such as the
        # suppressed ellipsis tokens, once per process.
        # Whisper runs even if that recording is silent: warming it is the point.
        await backend.transcribe_array(
            samples, rate, language=language, model_size=stt_size, previous_text="", check_speech=False
        )
        logger.info("Whisper warmed in %.3fs", time.monotonic() - started)
    except Exception:
        logger.exception("Could not warm Whisper")


async def warm_refinement(prompt) -> None:
    """Run one short cleanup so the first dictation reuses the cached prompt.

    ``prompt`` is the default style's, as a dictation sends it
    (``routes.stream_models.CleanupPrompt``).
    """
    from ..backends.qwen_llm_backend import prompt_cache_key
    from ..routes.stream_models import Generate, _adapter, _generate

    backend = llm.get_llm_model()
    if not backend.is_loaded():
        return
    try:
        started = time.monotonic()
        request = Generate(**prompt.model_dump(), id="startup", prompt="okay", max_tokens=WARM_TOKENS)
        key = prompt_cache_key.set(prompt.cache_key)
        try:
            await _generate(backend, request, _adapter(backend, prompt.model_size, prompt.flags))
        finally:
            prompt_cache_key.reset(key)
        logger.info("Refinement prompt warmed in %.3fs", time.monotonic() - started)
    except Exception:
        logger.exception("Could not warm the refinement prompt")


async def warm_styles(prompts: list) -> None:
    """Cache the other styles' prompts in the background, one at a time, while nothing is dictated.

    Each takes seconds on 4B the first time. A style whose app is dictated in
    before its turn is cached at that key-down instead.
    """
    from ..routes.stream_models import cleanup_prefill
    from .model_improvement.manager import dictating

    backend = llm.get_llm_model()
    for prompt in prompts:
        if not backend.is_loaded() or dictating():
            return
        try:
            started = time.monotonic()
            await cleanup_prefill(prompt)
            style = (prompt.flags or {}).get("style")
            logger.info("Style %s prompt cached in %.3fs", style, time.monotonic() - started)
        except Exception:
            logger.exception("Could not cache a style's prompt")
            return


def _warm_audio() -> tuple[np.ndarray, int]:
    """The newest recording's first seconds, or faint 48 kHz noise.

    48 kHz like a Mac microphone, so the resampling path is warm too.
    """
    try:
        recordings = sorted(config.get_captures_dir().glob("*.wav"), key=lambda p: p.stat().st_mtime)
        if recordings:
            import soundfile as sf

            with sf.SoundFile(str(recordings[-1])) as audio:
                rate = audio.samplerate
                samples = audio.read(frames=rate * WARM_SECONDS, dtype="int16", always_2d=True)[:, 0]
            if len(samples):
                return samples, rate
    except Exception:
        logger.info("No usable recording to warm Whisper with; using noise", exc_info=True)
    return np.random.default_rng(0).integers(-30, 30, WARM_RATE, dtype=np.int16), WARM_RATE
