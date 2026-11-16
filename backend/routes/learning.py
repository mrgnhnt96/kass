"""What the Rust server needs from the worker for learning from reports.

The server learns corrections from the reports in kass.db and keeps them in
correction-learning.json (server/src/learning.rs). The worker applies them to
dictations and runs model improvement, which uses the models.
"""

from fastapi import APIRouter

from ..services import correction_learning
from ..services.model_improvement import manager

router = APIRouter(prefix="/worker/learning", tags=["worker"])


@router.post("/reload")
def reload():
    """The server saved new corrections: dictations use them from now on."""
    return correction_learning.reload()


@router.post("/idle")
def idle():
    """Whether nothing uses the models, so background learning may take the CPU."""
    return {"idle": manager.idle_generation() is not None}


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
