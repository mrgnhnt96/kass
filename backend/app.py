"""The model worker the Rust server (kass-server) starts and calls: it loads
Whisper and the cleanup model, recognizes, generates and trains."""

import asyncio
import logging
import sys
from contextlib import asynccontextmanager
from pathlib import Path
from typing import ClassVar


class ColoredFormatter(logging.Formatter):
    """Custom formatter to add colors matching uvicorn's style."""

    COLORS: ClassVar[dict[str, str]] = {
        "DEBUG": "\033[36m",  # Cyan
        "INFO": "\033[32m",  # Green
        "WARNING": "\033[33m",  # Yellow
        "ERROR": "\033[31m",  # Red
        "CRITICAL": "\033[35m",  # Magenta
    }
    RESET = "\033[0m"

    def format(self, record):
        log_color = self.COLORS.get(record.levelname, self.RESET)
        record.levelname = f"{log_color}{record.levelname}{self.RESET}"
        return super().format(record)


# Configure logging to match uvicorn's format with colors
handler = logging.StreamHandler(sys.stderr)
handler.setFormatter(ColoredFormatter("%(levelname)s:     %(message)s"))
logging.basicConfig(
    level=logging.INFO,
    handlers=[handler],
)

logger = logging.getLogger(__name__)

from fastapi import FastAPI

from . import __version__, config
from .routes import register_routers
from .services import llm, transcribe
from .utils.platform_detect import BACKEND_TYPE, GPU_TYPE, require_apple_silicon
from .utils.progress import get_progress_manager


def create_app() -> FastAPI:
    """Create and configure the FastAPI application."""

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        await _run_startup(app)
        from .services.correction_learning import initialize

        initialize()
        try:
            yield
        finally:
            # Runs whether or not startup finished, so a partial startup
            # still unloads whatever models were loaded.
            await _run_shutdown()

    application = FastAPI(
        title="kass model worker",
        description="Runs Whisper, the cleanup model and training for the Kass server",
        version=__version__,
        lifespan=lifespan,
    )

    from .services.model_improvement.middleware import ForegroundPriorityMiddleware

    application.add_middleware(ForegroundPriorityMiddleware)
    register_routers(application)

    return application


async def _run_startup(application: FastAPI) -> None:
    """Initialize storage on lifespan entry; the app asks for the models after."""
    import platform
    import sys

    logger.info("Kass v%s starting up", __version__)
    logger.info(
        "Python %s on %s %s (%s)",
        sys.version.split()[0],
        platform.system(),
        platform.release(),
        platform.machine(),
    )

    require_apple_silicon()

    logger.info("Data directory: %s", config.get_data_dir())

    logger.info("Backend: %s", BACKEND_TYPE.upper())
    logger.info("GPU: %s", GPU_TYPE)

    try:
        progress_manager = get_progress_manager()
        progress_manager._set_main_loop(asyncio.get_running_loop())
    except Exception as e:
        logger.warning("Could not initialize progress manager event loop: %s", e)

    try:
        from huggingface_hub import constants as hf_constants

        cache_dir = Path(hf_constants.HF_HUB_CACHE)
        cache_dir.mkdir(parents=True, exist_ok=True)
        logger.info("Model cache: %s", cache_dir)
    except Exception as e:
        logger.warning("Could not create HuggingFace cache directory: %s", e)

    from .services import sync

    if sync.standing_in():
        logger.info("Standing in for the shared Kass at %s; models load if the app falls back", sync.shared_server())
    # Otherwise the app asks for its models once this is up (/worker/models/startup).
    logger.info("Ready")


async def _run_shutdown() -> None:
    """Unload models on lifespan exit."""
    logger.info("Kass server shutting down...")
    try:
        from .services.model_improvement.manager import stop

        # A training run's child process doesn't outlive the worker.
        await asyncio.to_thread(stop)
    except Exception:
        logger.exception("Failed to stop model improvement")
    try:
        await transcribe.unload_whisper_model()
    except Exception:
        logger.exception("Failed to unload Whisper model")
    try:
        await llm.unload_llm_model()
    except Exception:
        logger.exception("Failed to unload LLM model")


app = create_app()
