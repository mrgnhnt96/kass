"""Read back whatever of the loaded models macOS took out of RAM.

After a while unused, macOS swaps or compresses the Whisper and Qwen weights
(and Qwen's cached style prompts). The next dictation's GPU work then waits
on reading them back, and because every MLX job shares one thread, the
recognition waits behind it: once 32 s behind a style prefill that normally
takes 0.1 s.

``mincore`` reports which pages of a buffer are in RAM, so a model's warmth
is measured, not guessed. Pages that are out are read back here, one byte
each, on an ordinary thread: reading a page faults it back in, and the MLX
thread keeps recognizing meanwhile.

When to do it is the app's server's call (``tauri/src-tauri/server/src/warmth.rs``): it
follows the user and asks through ``/worker/keep-resident``, as a dictation
does when it starts.
"""

import asyncio
import ctypes
import logging
import resource
import time
from dataclasses import dataclass

from .mlx_thread import run_on_mlx_thread, speculative_mlx_work

logger = logging.getLogger(__name__)

PAGE_SIZE = resource.getpagesize()
# Below this share of pages in RAM, read the rest back.
WARM_ENOUGH = 0.98

_lock = asyncio.Lock()


# -- Page residency ---------------------------------------------------------

_libc = ctypes.CDLL(None, use_errno=True)
_libc.mincore.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.c_char_p]
_libc.mincore.restype = ctypes.c_int


@dataclass
class Buffer:
    """A span of memory, and what keeps it allocated while it is read."""

    address: int
    size: int
    owner: object


def buffer_of(data) -> Buffer | None:
    """The memory behind an evaluated MLX array, or anything with a writable buffer."""
    try:
        view = memoryview(data)
        holder = (ctypes.c_char * view.nbytes).from_buffer(view)
    except (TypeError, ValueError, BufferError):
        return None
    if view.nbytes == 0:
        return None
    return Buffer(ctypes.addressof(holder), view.nbytes, (data, view, holder))


def _page_map(buffer: Buffer) -> tuple[int, bytes]:
    """The page-aligned start of ``buffer`` and one ``mincore`` byte per page."""
    start = buffer.address - buffer.address % PAGE_SIZE
    length = buffer.address + buffer.size - start
    pages = (length + PAGE_SIZE - 1) // PAGE_SIZE
    vector = ctypes.create_string_buffer(pages)
    if _libc.mincore(ctypes.c_void_p(start), length, vector) != 0:
        raise OSError(ctypes.get_errno(), "mincore failed")
    return start, vector.raw


@dataclass
class Warmth:
    resident: int
    total: int
    paged_in: int = 0
    seconds: float = 0.0

    @property
    def fraction(self) -> float:
        return self.resident / self.total if self.total else 1.0


def residency(buffers: list[Buffer]) -> Warmth:
    resident = total = 0
    for buffer in buffers:
        _, pages = _page_map(buffer)
        total += len(pages)
        resident += sum(page & 1 for page in pages)
    return Warmth(resident, total)


def page_in(buffers: list[Buffer]) -> Warmth:
    """Read one byte of every page not in RAM, which brings it back.

    Each read is its own ``ctypes`` call, which releases the GIL, so the
    server stays responsive while the reads wait on swap.
    """
    started = time.monotonic()
    sink = ctypes.create_string_buffer(1)
    warmth = Warmth(0, 0)
    for buffer in buffers:
        start, pages = _page_map(buffer)
        warmth.total += len(pages)
        for index, page in enumerate(pages):
            if page & 1:
                warmth.resident += 1
                continue
            # The first page can start before the buffer; read inside it.
            ctypes.memmove(sink, max(start + index * PAGE_SIZE, buffer.address), 1)
            warmth.paged_in += 1
    warmth.seconds = time.monotonic() - started
    return warmth


# -- The models' buffers ----------------------------------------------------


def _arrays(tree) -> list:
    from mlx.utils import tree_flatten

    return [value for _, value in tree_flatten(tree)]


def _model_buffers(backend) -> list[Buffer]:
    """Buffers of the backend's weights and cached prompts.

    Runs on the MLX thread: between jobs every array is evaluated, so taking
    its memory never starts GPU work, and a cache can't change underneath.
    """
    model = getattr(backend, "model", None)
    if model is None:
        return []
    arrays = _arrays(model.parameters())
    for entry in getattr(backend, "_prompt_caches", None) or []:
        for layer in entry.cache or []:
            # The whole buffers: ``state`` would slice them into new arrays.
            for part in (getattr(layer, "keys", None), getattr(layer, "values", None)):
                if part is not None:
                    arrays.extend(_arrays(part))
    return [buffer for buffer in map(buffer_of, arrays) if buffer is not None]


def _backends(llm: bool = True, stt: bool = True) -> list[tuple[str, object]]:
    from . import llm as llm_service
    from .transcribe import get_whisper_model

    # Recognition comes first in every dictation, so it comes back first.
    found = []
    if stt:
        found.append(("Whisper", get_whisper_model()))
    if llm:
        found.append(("Qwen3", llm_service.get_llm_model()))
    return [(name, backend) for name, backend in found if getattr(backend, "model", None) is not None]


async def keep_resident(reason: str, llm: bool = True, stt: bool = True) -> None:
    """Read back whatever of the loaded models macOS took out of RAM."""
    async with _lock:
        for name, backend in _backends(llm, stt):
            with speculative_mlx_work():
                buffers = await run_on_mlx_thread(_model_buffers, backend)
            if not buffers:
                continue
            warmth = await asyncio.to_thread(residency, buffers)
            if warmth.fraction >= WARM_ENOUGH:
                continue
            warmth = await asyncio.to_thread(page_in, buffers)
            logger.info(
                "Brought %s back into RAM (%s): %.0f%% had been paged out, %d MB read in %.2fs",
                name,
                reason,
                100 * (1 - warmth.resident / warmth.total),
                warmth.paged_in * PAGE_SIZE // (1024 * 1024),
                warmth.seconds,
            )
