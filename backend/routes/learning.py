"""What the Rust server needs from the worker for learning from reports.

The server learns corrections from the reports in kass.db and keeps them in
correction-learning.json (server/src/learning.rs). The worker applies them to
dictations and runs model improvement, which uses the models.
"""

from fastapi import APIRouter

from ..services import correction_learning
from ..services.model_improvement import manager
from .stream_models import Generate, cleanup_generate

router = APIRouter(prefix="/worker/learning", tags=["worker"])


@router.post("/reload")
def reload():
    """The server saved new corrections: dictations use them from now on."""
    return correction_learning.reload()


@router.post("/idle")
def idle():
    """Whether nothing uses the models, so background learning may take the CPU.

    ``generation`` changes with every dictation: a background job that saw
    it change since it started was interrupted.
    """
    return {"idle": manager.idle_generation() is not None, "generation": manager.foreground_generation()}


@router.post("/generate")
async def generate(request: Generate):
    """A background generation, such as the correction notes job's
    (server/src/correction_notes.rs): as /worker/cleanup/generate, but not
    counted as a dictation."""
    return await cleanup_generate(request)


@router.post("/model")
def model_status():
    return manager.status()


@router.post("/model/start")
def model_start():
    return manager.start()


@router.post("/model/rollback")
def model_rollback():
    try:
        return manager.rollback()
    except ValueError as error:
        return {"refused": str(error)}
