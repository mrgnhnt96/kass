"""The model calls a dictation streamed through the Rust server (kass-server) makes.

The Rust session owns the audio, the text rules and the cleanup prompt; the
worker only runs Whisper and Qwen3 for it. Python's own /captures/stream
doesn't use these and calls the models in-process.

Recognition takes raw PCM, so a phrase isn't written to a file first. Word
alignments stay here, named by an id, until the session asks for their
words: working them out costs a few milliseconds the release shouldn't wait
for. Cleanup streams the text generated so far as JSON lines and stops before
the next token when told to, or when the Rust side drops the response.
"""

import asyncio
import contextlib
import json
import logging
import struct
import threading
import time
from collections import OrderedDict

import numpy as np
from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel, Field

from ..backends.qwen_llm_backend import generation_hint, generation_listener, generation_stop, prompt_cache_key
from ..services import commands, llm as llm_service
from ..services.mlx_thread import speculative_mlx_work
from ..services.model_warmth import keep_resident
from ..services.transcribe import get_whisper_model

router = APIRouter(prefix="/worker", tags=["worker"])
logger = logging.getLogger(__name__)

# The JSON head of a recognize body is small; a bigger one is a broken client.
MAX_HEAD_BYTES = 256 * 1024
# Alignments the session never asked about are dropped, oldest first.
MAX_ALIGNMENTS = 64

_alignments: OrderedDict[str, object] = OrderedDict()
_next_alignment = 0
# Cleanups running now, by the id the Rust side gave them, so it can stop one.
_generations: dict[str, threading.Event] = {}


class RecognizeHead(BaseModel):
    rate: int = Field(gt=0)
    language: str | None = None
    stt_model: str | None = None
    previous_text: str | None = None
    vocabulary: list[str] = []
    # Keep the decode's word alignment, for how the phrase was said.
    alignment: bool = False


class RecognizeFile(BaseModel):
    path: str
    language: str | None = None
    stt_model: str | None = None
    previous_text: str | None = None
    vocabulary: list[str] = []


class Cleanup(BaseModel):
    # The style's cleanup flags (RefinementFlags.to_dict()), which pick its
    # personal or shared adapter. None runs the base model.
    flags: dict | None = None
    model_size: str


class CleanupPrompt(Cleanup):
    system: str
    examples: list[tuple[str, str]] = []
    # The prompt cache the call continues: one per writing style.
    cache_key: str | None = None


class Generate(CleanupPrompt):
    # Named by the caller, for /worker/cleanup/{id}/stop.
    id: str
    prompt: str
    # Text the output mostly copies, for lookup decoding: "" for the
    # transcript alone, the previous cleanup of the same tail, or None for none.
    hint: str | None = None
    max_tokens: int = 2048
    temperature: float = 0.2


class CommandPrefill(BaseModel):
    selection: str
    model_size: str


class CommandRewrite(CommandPrefill):
    instruction: str


class KeepResident(BaseModel):
    reason: str
    llm: bool = True
    stt: bool = True


class SpeechModel(BaseModel):
    configured: str


def _keep_alignment(alignment) -> str | None:
    global _next_alignment
    if alignment is None:
        return None
    _next_alignment += 1
    key = str(_next_alignment)
    _alignments[key] = alignment
    while len(_alignments) > MAX_ALIGNMENTS:
        _alignments.popitem(last=False)
    return key


def _parse_recognize(body: bytes) -> tuple[RecognizeHead, np.ndarray]:
    """A recognize body: the JSON head's length (u32 LE), the head, then mono s16le PCM."""
    if len(body) < 4:
        raise ValueError("Missing the head's length")
    (size,) = struct.unpack_from("<I", body)
    if size > MAX_HEAD_BYTES or 4 + size > len(body):
        raise ValueError("Bad head length")
    head = RecognizeHead.model_validate(json.loads(body[4 : 4 + size]))
    pcm = body[4 + size :]
    if len(pcm) % 2:
        raise ValueError("PCM must be whole 16-bit samples")
    return head, np.frombuffer(pcm, dtype="<i2")


@router.post("/recognize")
async def recognize(request: Request):
    """Whisper's text for one phrase of a dictation; the caller already checked it has a voice."""
    try:
        head, samples = _parse_recognize(await request.body())
    except ValueError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
    if not len(samples):
        return {"text": "", "alignment": None, "seconds": 0.0}
    alignments = [] if head.alignment else None
    started = time.monotonic()
    text = await get_whisper_model().transcribe_array(
        samples,
        head.rate,
        head.language,
        head.stt_model,
        previous_text=head.previous_text,
        check_speech=False,
        vocabulary=tuple(head.vocabulary),
        alignments=alignments,
    )
    seconds = time.monotonic() - started
    return {
        "text": text.strip(),
        "alignment": _keep_alignment(alignments[0]) if alignments else None,
        "seconds": seconds,
    }


@router.post("/recognize/file")
async def recognize_file(request: RecognizeFile):
    """Whisper's text for a whole recording, when the phrases couldn't be joined."""
    started = time.monotonic()
    text = await get_whisper_model().transcribe(
        request.path,
        request.language,
        request.stt_model,
        previous_text=request.previous_text,
        check_speech=False,
        vocabulary=tuple(request.vocabulary),
    )
    return {"text": text.strip(), "seconds": time.monotonic() - started}


@router.post("/alignments/{key}/words")
async def alignment_words(key: str):
    """Each word of a recognized phrase and when it was said, in seconds from the phrase's start.

    Worked out off the MLX thread, once; the alignment is dropped after.
    """
    alignment = _alignments.pop(key, None)
    if alignment is None:
        raise HTTPException(status_code=404, detail="No such alignment")
    words = await asyncio.to_thread(alignment.words)
    return {"words": [{"text": word.text, "start": word.start, "end": word.end} for word in words]}


def _adapter(backend, model_size: str, flags: dict | None) -> str | None:
    if flags is None or not getattr(backend, "supports_adapters", False):
        return None
    from ..services.model_improvement.manager import active_adapter

    return active_adapter(model_size, flags)


async def _generate(backend, request: Generate, adapter_path: str | None) -> str:
    """One cleanup generation, falling back to the base model when the adapter fails, as refine_transcript does."""
    arguments = dict(
        prompt=request.prompt,
        system=request.system,
        max_tokens=request.max_tokens,
        temperature=request.temperature,
        model_size=request.model_size,
        examples=request.examples,
    )
    if adapter_path is None:
        return await backend.generate(**arguments)
    try:
        return await backend.generate(**arguments, adapter_path=adapter_path)
    except Exception:
        from ..services.model_improvement.manager import quarantine_adapter

        quarantine_adapter("The personal adapter failed to load or generate; reverted to the base model.", adapter_path)
        return await backend.generate(**arguments)


def _line(event: dict) -> bytes:
    return (json.dumps(event, ensure_ascii=False) + "\n").encode()


@router.post("/cleanup/generate")
async def cleanup_generate(request: Generate):
    """Run the cleanup prompt the session built, streaming what's generated.

    JSON lines: ``{"partial": text so far}`` as tokens come (the latest only,
    when several arrive between sends), then ``{"text", "model", "stopped",
    "seconds"}``, or ``{"error"}`` if generation failed.
    """
    if request.id in _generations:
        raise HTTPException(status_code=409, detail="A cleanup with this id is running")
    backend = llm_service.get_llm_model()
    adapter_path = _adapter(backend, request.model_size, request.flags)
    stop = threading.Event()
    loop = asyncio.get_running_loop()
    latest: list[str | None] = [None]
    changed = asyncio.Event()

    def update(partial: str) -> None:
        latest[0] = partial
        changed.set()

    def listener(partial: str) -> None:  # MLX worker thread
        with contextlib.suppress(RuntimeError):
            loop.call_soon_threadsafe(update, partial)

    tokens = (
        generation_listener.set(listener),
        generation_stop.set(stop),
        generation_hint.set(request.hint),
        prompt_cache_key.set(request.cache_key),
    )
    try:
        # The task copies the context now, with the generation's settings.
        task = asyncio.create_task(_generate(backend, request, adapter_path))
    finally:
        generation_listener.reset(tokens[0])
        generation_stop.reset(tokens[1])
        generation_hint.reset(tokens[2])
        prompt_cache_key.reset(tokens[3])
    _generations[request.id] = stop
    task.add_done_callback(lambda _: _generations.pop(request.id, None))
    started = time.monotonic()

    async def lines():
        try:
            while not task.done():
                waiter = asyncio.ensure_future(changed.wait())
                try:
                    await asyncio.wait({task, waiter}, return_when=asyncio.FIRST_COMPLETED)
                finally:
                    waiter.cancel()
                if changed.is_set() and not task.done():
                    changed.clear()
                    yield _line({"partial": latest[0]})
            if task.exception() is not None:
                logger.warning("Cleanup generation failed", exc_info=task.exception())
                yield _line({"error": str(task.exception())})
                return
            yield _line(
                {
                    "text": task.result(),
                    "model": request.model_size,
                    "stopped": stop.is_set(),
                    "seconds": time.monotonic() - started,
                }
            )
        finally:
            if not task.done():
                # The session dropped the response: nobody wants the rest.
                stop.set()

    return StreamingResponse(lines(), media_type="application/x-ndjson")


@router.post("/cleanup/{key}/stop")
async def cleanup_stop(key: str):
    """End a running cleanup before its next token; it answers with the text so far, marked stopped."""
    stop = _generations.get(key)
    if stop is not None:
        stop.set()
    return {"stopped": stop is not None}


@router.post("/cleanup/prefill")
async def cleanup_prefill(request: CleanupPrompt):
    """Put a style's cleanup prompt in the model's cache while the user speaks (prefill_cleanup)."""
    backend = llm_service.get_llm_model()
    adapter_path = _adapter(backend, request.model_size, request.flags)
    started = time.monotonic()
    key = prompt_cache_key.set(request.cache_key)
    try:
        # A recognition queued meanwhile goes first: the cleanup prefills it anyway.
        with speculative_mlx_work():
            await backend.generate(
                prompt="",
                system=request.system,
                max_tokens=1,
                temperature=0,
                model_size=request.model_size,
                examples=request.examples,
                **({"adapter_path": adapter_path} if adapter_path else {}),
            )
    finally:
        prompt_cache_key.reset(key)
    return {"seconds": time.monotonic() - started}


@router.post("/cleanup/load")
async def cleanup_load(request: Cleanup):
    """Load the cleanup model and the style's adapter before the first phrase needs them (load_cleanup_model)."""
    backend = llm_service.get_llm_model()
    prepare = getattr(backend, "prepare", None)
    if prepare is not None:
        adapter_path = _adapter(backend, request.model_size, request.flags)
        with speculative_mlx_work():
            await prepare(request.model_size, adapter_path)
    return {"ok": True}


def _failed(error: Exception) -> JSONResponse:
    # The message is what the session reports as the refinement error.
    return JSONResponse(status_code=422 if isinstance(error, ValueError) else 500, content={"detail": str(error)})


@router.post("/command/prefill")
async def command_prefill(request: CommandPrefill):
    """Put the command prompt and the selection in the cache while the instruction is spoken."""
    try:
        commands.ensure_model_ready(request.model_size)
        await commands.prefill(request.selection, request.model_size)
    except Exception as error:
        return _failed(error)
    return {"ok": True}


@router.post("/command/rewrite")
async def command_rewrite(request: CommandRewrite):
    """Rewrite the selection by the instruction; the session resolved transforms already."""
    try:
        commands.ensure_model_ready(request.model_size)
        text, model = await commands.rewrite(request.selection, request.instruction, request.model_size)
    except Exception as error:
        return _failed(error)
    return {"text": text, "model": model}


@router.post("/speech-model")
async def speech_model(request: SpeechModel):
    """The Whisper model a dictation with ``configured`` set runs: a trained one replaces it while active."""
    from ..services.model_improvement.manager import speech_model as active_speech_model

    return {"model": active_speech_model(request.configured)}


@router.post("/keep-resident")
async def keep_models_resident(request: KeepResident):
    """Read back whatever of the loaded models macOS paged out."""
    await keep_resident(request.reason, llm=request.llm, stt=request.stt)
    return {"ok": True}


@router.post("/foreground/begin")
async def foreground_begin():
    """A dictation started: training waits until every one has ended."""
    from ..services.model_improvement.manager import begin_foreground

    await asyncio.to_thread(begin_foreground)
    return {"ok": True}


@router.post("/foreground/end")
async def foreground_end():
    from ..services.model_improvement.manager import end_foreground

    end_foreground()
    return {"ok": True}
