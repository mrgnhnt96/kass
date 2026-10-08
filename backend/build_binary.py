"""
PyInstaller build script for creating the standalone Python server binary.

Usage:
    python build_binary.py
"""

import PyInstaller.__main__
import logging
import os
import platform
import sys
from pathlib import Path

logger = logging.getLogger(__name__)


def is_apple_silicon():
    """Check if running on Apple Silicon."""
    return platform.system() == "Darwin" and platform.machine() == "arm64"


def build_server():
    """Build the Python server as a folder bundle (Apple Silicon only).

    A one-file build unpacks every library to a temp dir on each launch,
    which cost several seconds of app startup. The folder build runs in place.
    """
    if not is_apple_silicon():
        raise SystemExit("kass-server builds on Apple Silicon (arm64 macOS) only.")

    backend_dir = Path(__file__).parent
    binary_name = "kass-server"

    args = [
        "server.py",  # Use server.py as entry point instead of main.py
        "--onedir",
        # Nothing to decompress at launch, and UPX would break code signatures.
        "--noupx",
        "--name",
        binary_name,
        # Include service packages used by optional flows and subprocess workers.
        "--collect-submodules",
        "backend.services",
    ]

    # Paths are passed relative to backend_dir because os.chdir(backend_dir)
    # runs before PyInstaller. Absolute paths would get baked into the
    # generated .spec, breaking reproducible builds on other machines.
    args.extend(
        [
            # Let scipy.stats (librosa -> scipy.signal -> scipy.stats) load
            # under the frozen importer. See pyi_rth_scipy_distn.py.
            "--runtime-hook",
            "pyi_rth_scipy_distn.py",
            # Per-module collection overrides (e.g. forcing scipy.stats._distn_infrastructure
            # to bundle .py source alongside .pyc so the runtime hook can source-patch it).
            "--additional-hooks-dir",
            "pyi_hooks",
        ]
    )

    # Add common hidden imports
    args.extend(
        [
            "--hidden-import",
            "backend",
            "--hidden-import",
            "backend.main",
            "--hidden-import",
            "backend.config",
            "--hidden-import",
            "backend.database",
            "--hidden-import",
            "backend.models",
            "--hidden-import",
            "backend.services.transcribe",
            "--hidden-import",
            "backend.utils.platform_detect",
            "--hidden-import",
            "backend.backends",
            "--hidden-import",
            "backend.backends.qwen_llm_backend",
            "--hidden-import",
            "backend.utils.audio",
            "--hidden-import",
            "backend.utils.progress",
            "--hidden-import",
            "backend.utils.hf_progress",
            "--hidden-import",
            "transformers",
            "--hidden-import",
            "fastapi",
            "--hidden-import",
            "uvicorn",
            # Uvicorn resolves WebSocket transports by import string. Include
            # the streaming dictation transport in frozen desktop builds.
            "--hidden-import",
            "uvicorn.protocols.websockets.auto",
            "--hidden-import",
            "uvicorn.protocols.websockets.websockets_impl",
            "--collect-submodules",
            "websockets",
            "--hidden-import",
            "sqlalchemy",
            # librosa uses lazy_loader which generates .pyi stub files at
            # install time and reads them at runtime to discover submodules.
            # --hidden-import alone doesn't bundle the stubs, causing
            # "Cannot load imports from non-existent stub" at runtime.
            "--collect-all",
            "lazy_loader",
            "--collect-all",
            "librosa",
            "--hidden-import",
            "soundfile",
            # The voice detector that keeps Whisper off silent audio.
            "--hidden-import",
            "onnxruntime",
            "--collect-binaries",
            "onnxruntime",
            "--add-data",
            "assets/silero_vad.onnx:backend/assets",
            "--copy-metadata",
            "requests",
            "--copy-metadata",
            "transformers",
            "--copy-metadata",
            "huggingface-hub",
            "--copy-metadata",
            "tokenizers",
            "--copy-metadata",
            "safetensors",
            "--copy-metadata",
            "tqdm",
            "--hidden-import",
            "requests",
            # Fix for pkg_resources and jaraco namespace packages
            "--hidden-import",
            "pkg_resources.extern",
            "--collect-submodules",
            "jaraco",
        ]
    )

    if sys.version_info >= (3, 13):
        args.extend(["--hidden-import", "audioop"])

    # mlx_audio ships TTS/STS models that depend on torch. Kass never loads
    # them, so keep torch out even when the build venv still has it.
    for module in ("torch", "torchaudio", "torchvision"):
        args.extend(["--exclude-module", module])

    # MLX runs Whisper and the refinement LLM.
    args.extend(
        [
            "--hidden-import",
            "backend.backends.mlx_backend",
            "--hidden-import",
            "mlx",
            "--hidden-import",
            "mlx.core",
            "--hidden-import",
            "mlx.nn",
            "--hidden-import",
            "mlx_audio",
            "--hidden-import",
            "mlx_audio.stt",
            "--hidden-import",
            "mlx_lm",
            "--collect-submodules",
            "mlx",
            "--collect-submodules",
            "mlx_lm",
            # Use --collect-all so PyInstaller bundles both data files AND
            # native shared libraries (.dylib, .metallib) for MLX.
            # Previously only --collect-data was used, which caused MLX to
            # raise OSError at runtime inside the bundled binary because
            # the Metal shader libraries were missing.
            "--collect-all",
            "mlx",
            # mlx_audio is collected whole by pyinstaller_hooks/hook-mlx_audio.py.
            # mlx_lm ships chat_templates/ JSON files and loads tool_parsers
            # submodules dynamically via importlib at tokenizer load time,
            # which --hidden-import alone can't resolve.
            "--collect-all",
            "mlx_lm",
        ]
    )

    # The adapters Kass trains and ships (services/shared_adapters.py).
    if (backend_dir / "assets" / "shared-adapters").is_dir():
        args.extend(["--add-data", "assets/shared-adapters:backend/assets/shared-adapters"])

    # Project hooks override pyinstaller-hooks-contrib's.
    args.extend(["--additional-hooks-dir", "pyinstaller_hooks"])

    dist_dir = str(backend_dir / "dist")
    build_dir = str(backend_dir / "build")

    args.extend(
        [
            "--distpath",
            dist_dir,
            "--workpath",
            build_dir,
            "--noconfirm",
            "--clean",
        ]
    )

    # Change to backend directory
    os.chdir(backend_dir)

    PyInstaller.__main__.run(args)

    logger.info("Server built in %s", backend_dir / "dist" / binary_name / binary_name)


if __name__ == "__main__":
    build_server()
