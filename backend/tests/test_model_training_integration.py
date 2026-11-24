"""Opt-in real MLX gradient update and production adapter loading.

KASS_RUN_MLX_TRAINING_TESTS=1 backend/venv/bin/python -m pytest \
    backend/tests/test_model_training_integration.py -q
Requires an already-cached Qwen3-0.6B MLX model. Never downloads or deploys.
"""

import os

import pytest

pytestmark = pytest.mark.skipif(
    os.environ.get("KASS_RUN_MLX_TRAINING_TESTS") != "1", reason="Opt-in local MLX integration test"
)


@pytest.mark.asyncio
async def test_real_adapter_training_and_production_inference(tmp_path, monkeypatch):
    import json

    from huggingface_hub import snapshot_download

    from backend.backends.qwen_llm_backend import MLXQwenLLMBackend
    from backend.services.model_improvement.worker import train_adapter

    monkeypatch.setenv("HF_HUB_OFFLINE", "1")
    plan = {
        "model_path": snapshot_download("mlx-community/Qwen3-0.6B-4bit", local_files_only=True),
        "iterations": 1,
        "min_train": 1,
        "min_validation": 1,
        "samples": [
            {
                "target": "refined",
                "split": "train",
                "raw": "please open the voice box app",
                "expected": "Please open the Voicebox app.",
            },
            {
                "target": "refined",
                "split": "validation",
                "raw": "we need to test the voice box recorder",
                "expected": "We need to test the Voicebox recorder.",
            },
        ],
    }
    # Training and inference normally run in separate processes. Run training on
    # the same dedicated MLX thread here to respect Metal's thread affinity.
    from backend.services.mlx_thread import run_on_mlx_thread

    await run_on_mlx_thread(train_adapter, plan, tmp_path)
    evidence = json.loads((tmp_path / "training.json").read_text())
    assert evidence["weights_changed"]
    backend = MLXQwenLLMBackend("0.6B")
    text = await backend.generate(
        "please open the application",
        system="Clean up the dictated text.",
        max_tokens=64,
        temperature=0,
        model_size="0.6B",
        adapter_path=str(tmp_path / "adapter"),
    )
    assert text.strip()
    assert backend._adapter_path == str(tmp_path / "adapter")
    await backend.generate("hello", max_tokens=8, temperature=0)
    assert backend._adapter_path is None
    await backend.unload()
