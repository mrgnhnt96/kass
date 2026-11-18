"""What the Rust server needs from the worker for learning from reports.

The server learns corrections from the reports in kass.db and keeps them in
correction-learning.json (server/src/learning.rs). The worker applies them to
dictations and plans, trains and uses the models model improvement picks.
"""

import logging

from fastapi import APIRouter
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from ..services import correction_learning
from ..services.model_improvement import manager
from .stream_models import Generate, cleanup_generate

logger = logging.getLogger(__name__)
router = APIRouter(prefix="/worker/learning", tags=["worker"])
events_router = APIRouter(prefix="/worker/captures", tags=["worker"])


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


class Stop(BaseModel):
    recording: bool = False


@router.post("/model/stop")
def model_stop(request: Stop | None = None):
    """Stop a run in progress. ``recording``: the user started recording, so
    background model work waits until the dictation is done."""
    if request and request.recording:
        manager.foreground_activity(recording=True)
    else:
        manager.stop()
    return {}


@router.post("/model/reports")
def model_reports():
    """Trained models the worker stopped using since the last call."""
    return {"reports": manager.reports()}


@router.post("/model/voice")
def model_voice():
    """The voice recordings banked for training. The server reports the
    background sounds' download (server/src/voice_sounds.rs)."""
    from ..services.voice_training import bank

    return {"bank": bank.summary()}


class Forget(BaseModel):
    capture_id: str


@router.post("/model/forget")
def model_forget(request: Forget):
    """The server deleted a capture: its voice take leaves the training bank."""
    from ..services.voice_training import bank

    bank.forget(request.capture_id)
    return {"ok": True}


@events_router.get("/events")
async def capture_events_endpoint():
    """The worker's own events (a trained model it stopped using is
    "learned"), which the server passes on to the apps."""
    from ..services import capture_events

    return StreamingResponse(
        capture_events.subscribe(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )
