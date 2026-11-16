"""Health and infrastructure endpoints."""

import asyncio
import os
import resource
import signal
import time

from fastapi import APIRouter, Depends
from sqlalchemy.orm import Session

from .. import models
from ..database import get_db
from ..services import transcribe
from ..services.task_queue import create_background_task
from ..utils.platform_detect import BACKEND_TYPE, GPU_TYPE

router = APIRouter()

# When this server process started, for the uptime the status bar shows.
STARTED_AT = time.time()


@router.get("/")
async def root():
    """Root endpoint."""
    from .. import __version__

    return {"message": "kass API", "version": __version__}


@router.post("/shutdown")
async def shutdown():
    """Gracefully shutdown the server."""

    async def shutdown_async():
        await asyncio.sleep(0.1)
        os.kill(os.getpid(), signal.SIGTERM)

    # Held so the event loop can't drop the task before it runs.
    create_background_task(shutdown_async())
    return {"message": "Shutting down..."}


@router.get("/health", response_model=models.HealthResponse)
async def health(db: Session = Depends(get_db)):
    """Health check endpoint.

    ``model_loaded`` / ``model_size`` describe the Whisper model in memory;
    ``model_downloaded`` says whether the Whisper model chosen in the capture
    settings is cached locally.
    """
    from .. import __version__

    whisper_model = transcribe.get_whisper_model()
    model_loaded = False
    model_size = None
    try:
        if whisper_model.is_loaded():
            model_loaded = True
            model_size = getattr(whisper_model, "model_size", None)
    except Exception:
        model_loaded = False
        model_size = None

    model_downloaded = None
    try:
        from ..backends import WHISPER_HF_REPOS
        from ..backends.base import is_model_cached
        from ..services import settings as settings_service

        stt_size = settings_service.get_capture_settings(db).stt_model
        repo = WHISPER_HF_REPOS.get(stt_size)
        if repo:
            model_downloaded = is_model_cached(repo, weight_extensions=(".safetensors", ".bin", ".npz"))
    except Exception:
        pass

    return models.HealthResponse(
        status="healthy",
        model_loaded=model_loaded,
        model_downloaded=model_downloaded,
        model_size=model_size,
        # Startup refuses anything but Apple Silicon, so MLX on Metal is
        # always the backend here.
        gpu_available=True,
        gpu_type=GPU_TYPE,
        backend_type=BACKEND_TYPE,
        version=__version__,
        started_at=STARTED_AT,
        pid=os.getpid(),
        # macOS reports the peak resident size in bytes.
        peak_memory_mb=round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024**2),
    )
