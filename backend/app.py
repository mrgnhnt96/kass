"""FastAPI application factory, middleware, and lifecycle events."""

import asyncio
import logging
import sys
from contextlib import asynccontextmanager, suppress
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
from fastapi.middleware.cors import CORSMiddleware

from . import __version__, config, database
from .routes import register_routers
from .services import llm, transcribe
from .utils.platform_detect import BACKEND_TYPE, GPU_TYPE, require_apple_silicon
from .utils.progress import get_progress_manager


def create_app() -> FastAPI:
    """Create and configure the FastAPI application."""

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        from .services import sync

        await _run_startup(app)
        from .services.correction_learning import initialize

        initialize()
        tasks = [asyncio.create_task(job()) for job in (_stand_in_jobs() if sync.standing_in() else _jobs())]
        try:
            yield
        finally:
            for task in tasks:
                task.cancel()
            for task in tasks:
                with suppress(asyncio.CancelledError):
                    await task
            # Runs whether or not startup finished, so a partial startup
            # still unloads whatever models were loaded.
            await _run_shutdown()

    application = FastAPI(
        title="kass API",
        description="Local dictation API: Whisper speech-to-text with LLM cleanup",
        version=__version__,
        lifespan=lifespan,
    )

    _configure_cors(application)
    from .services.model_improvement.middleware import ForegroundPriorityMiddleware

    application.add_middleware(ForegroundPriorityMiddleware)
    # Which Mac each request came from, for the captures it saves.
    from .utils.macs import MacMiddleware

    application.add_middleware(MacMiddleware)
    register_routers(application)

    return application


def _jobs():
    from .services.correction_notes import periodic_job as notes_job
    from .services.history_retention import periodic_job as retention_job
    from .services.model_improvement.manager import periodic_job as learning_job
    from .services.model_warmth import periodic_job as warmth_job
    from .services.usage_report import periodic_job as usage_job

    return (learning_job, notes_job, retention_job, usage_job, warmth_job)


def _stand_in_jobs():
    """A stand-in for a shared Kass only syncs with it: the shared Kass learns,
    keeps history and reports usage, and models stay unloaded until needed."""
    from .services.sync.job import periodic_job as sync_job

    return (sync_job,)


def _configure_cors(application: FastAPI) -> None:
    """Set up CORS middleware with local-first defaults."""
    from .utils.origins import allowed_origins

    application.add_middleware(
        CORSMiddleware,
        allow_origins=allowed_origins(),
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )


async def _run_startup(application: FastAPI) -> None:
    """Initialize storage and load configured models on lifespan entry."""
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

    database.init_db()

    from .database.session import _db_path

    logger.info("Database: %s", _db_path)
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
    else:
        from .services.model_startup import load_startup_models

        await load_startup_models()
    logger.info("Ready")


async def _run_shutdown() -> None:
    """Unload models on lifespan exit."""
    logger.info("Kass server shutting down...")
    try:
        await transcribe.unload_whisper_model()
    except Exception:
        logger.exception("Failed to unload Whisper model")
    try:
        await llm.unload_llm_model()
    except Exception:
        logger.exception("Failed to unload LLM model")


app = create_app()
