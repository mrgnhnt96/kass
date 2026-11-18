"""What the Rust server needs from the worker for learning from reports.

The server learns corrections from the reports in kass.db and keeps them in
correction-learning.json (server/src/learning.rs). The worker applies them to
dictations and plans, trains and uses the models model improvement picks.
"""

import logging

from fastapi import APIRouter
from pydantic import BaseModel

from ..services import correction_learning
from ..services.model_improvement import manager
from .stream_models import Generate, cleanup_generate

logger = logging.getLogger(__name__)
router = APIRouter(prefix="/worker/learning", tags=["worker"])


@router.post("/reload")
def reload():
    """The server saved new corrections: dictations use them from now on."""
    return correction_learning.reload()


@router.post("/idle")
def idle():
    """Whether nothing uses the models, so background learning may take the
    CPU, and whether a dictation is recording or finishing right now.

    ``generation`` changes with every dictation: a background job that saw
    it change since it started was interrupted.
    """
    return {
        "idle": manager.idle_generation() is not None,
        "generation": manager.foreground_generation(),
        "dictating": manager.dictating(),
    }


@router.post("/generate")
async def generate(request: Generate):
    """A background generation, such as the correction notes job's
    (server/src/correction_notes.rs): as /worker/cleanup/generate, but not
    counted as a dictation."""
    return await cleanup_generate(request)


# Model improvement: the server keeps its state and schedule
# (server/src/model_improvement.rs) and has the worker plan, train and use.


class Prepare(BaseModel):
    groups: list[dict] = []
    voice_resume: str | None = None
    skip: list[str] = []


@router.post("/model/prepare")
def model_prepare(request: Prepare):
    try:
        return manager.prepare(request.groups, request.voice_resume, request.skip)
    except Exception as error:
        logger.exception("Model improvement failed; production model unchanged")
        return {"error": str(error)}


class Train(BaseModel):
    run: str
    generation: int
    voice_resume: str | None = None


@router.post("/model/train")
def model_train(request: Train):
    try:
        return manager.train(request.run, request.generation, request.voice_resume)
    except Exception as error:
        logger.exception("Model improvement failed; production model unchanged")
        return {"error": str(error)}


class Active(BaseModel):
    active: dict = {}


@router.post("/model/check")
def model_check(request: Active):
    """The trained models in ``active`` that pass their checks."""
    return {"active": manager.check(request.active)}


@router.post("/model/use")
def model_use(request: Active):
    """Dictation uses the trained models the server saved."""
    return {"active": manager.use(request.active)}


@router.post("/model/stop")
def model_stop():
    """Stop a run in progress."""
    manager.stop()
    return {}


@router.post("/model/reports")
def model_reports():
    """Trained models the worker stopped using since the last call."""
    return {"reports": manager.reports()}


@router.post("/model/voice")
def model_voice():
    """The voice recordings banked for training, and the background sounds it mixes in."""
    from ..services.voice_training import bank, sounds

    return {"bank": bank.summary(), "sounds": sounds.status()}
