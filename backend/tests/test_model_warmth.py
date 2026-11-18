"""Measuring which model pages are in RAM, and reading the rest back."""

import mmap

import pytest

from backend.services import model_warmth
from backend.services.model_warmth import PAGE_SIZE, Warmth, buffer_of, page_in, residency


def _half_touched(pages=64):
    """Anonymous memory with only its first half ever written, so only that half is in RAM."""
    memory = mmap.mmap(-1, pages * PAGE_SIZE)
    for page in range(pages // 2):
        memory[page * PAGE_SIZE] = 1
    return memory


def test_residency_counts_only_the_pages_in_ram():
    memory = _half_touched()
    warmth = residency([buffer_of(memory)])
    assert (warmth.resident, warmth.total) == (32, 64)
    assert warmth.fraction == 0.5


def test_page_in_reads_back_exactly_the_missing_pages():
    memory = _half_touched()
    buffer = buffer_of(memory)

    warmth = page_in([buffer])

    assert (warmth.resident, warmth.paged_in, warmth.total) == (32, 32, 64)
    assert residency([buffer]).fraction == 1.0
    # Reading must not change the data.
    assert memory[0] == 1
    assert memory[63 * PAGE_SIZE] == 0


def test_a_buffer_that_starts_mid_page_is_read_inside_its_bounds():
    memory = _half_touched()
    whole = buffer_of(memory)
    inner = model_warmth.Buffer(whole.address + 100, 3 * PAGE_SIZE, memory)

    assert page_in([inner]).total == 4


def test_mlx_weights_are_measured_where_they_live():
    mx = pytest.importorskip("mlx.core")
    weights = mx.ones((256, 1024), dtype=mx.bfloat16)
    mx.eval(weights)

    buffer = buffer_of(weights)

    assert buffer is not None
    assert buffer.size == 256 * 1024 * 2
    assert residency([buffer]).fraction == 1.0


def test_something_without_a_buffer_is_skipped():
    assert buffer_of(object()) is None
    assert buffer_of(bytearray()) is None


@pytest.mark.asyncio
async def test_keep_resident_reads_back_only_a_model_that_went_cold(monkeypatch, caplog):
    nn = pytest.importorskip("mlx.nn")
    import mlx.core as mx

    from backend.services.mlx_thread import run_on_mlx_thread

    def load():
        # As the backends do: made and evaluated on the MLX thread.
        model = nn.Linear(64, 64)
        mx.eval(model.parameters())
        return model

    warm, cold = type("Backend", (), {})(), type("Backend", (), {})()
    warm.model, cold.model = await run_on_mlx_thread(load), await run_on_mlx_thread(load)
    monkeypatch.setattr(model_warmth, "_backends", lambda llm, stt: [("Whisper", warm), ("Qwen3", cold)])
    weights_of = {}

    def fake_residency(buffers):
        owner = buffers[0].owner[0]
        return Warmth(10, 10) if any(owner is v for v in weights_of["warm"]) else Warmth(1, 10)

    from mlx.utils import tree_flatten

    weights_of["warm"] = [value for _, value in tree_flatten(warm.model.parameters())]
    monkeypatch.setattr(model_warmth, "residency", fake_residency)
    read = []
    monkeypatch.setattr(model_warmth, "page_in", lambda buffers: read.append(buffers) or Warmth(1, 10, 9, 0.5))

    with caplog.at_level("INFO", logger=model_warmth.__name__):
        await model_warmth.keep_resident("test")

    assert len(read) == 1
    assert all(not any(b.owner[0] is v for v in weights_of["warm"]) for b in read[0])
    assert "Brought Qwen3 back into RAM (test): 90% had been paged out" in caplog.text


def test_prompt_cache_buffers_are_the_whole_key_and_value_arrays():
    mx = pytest.importorskip("mlx.core")
    nn = pytest.importorskip("mlx.nn")
    layer = type("Layer", (), {})()
    layer.keys = mx.zeros((1, 8, 256, 128), dtype=mx.float16)
    layer.values = mx.zeros((1, 8, 256, 128), dtype=mx.float16)
    mx.eval(layer.keys, layer.values)
    backend = type("Backend", (), {})()
    backend.model = nn.Linear(4, 4)
    backend._prompt_caches = [type("Entry", (), {"cache": [layer]})()]

    sizes = sorted(buffer.size for buffer in model_warmth._model_buffers(backend))

    assert sizes[-2:] == [1 * 8 * 256 * 128 * 2] * 2
