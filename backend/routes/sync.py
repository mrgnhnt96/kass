"""The model calls the Rust server (kass-server) makes for sharing Kass.

The Rust server serves /sync/* and runs a stand-in's sync with the shared
Kass. The worker only switches to the shared Kass's trained models once
they're copied.
"""

from fastapi import APIRouter
from pydantic import BaseModel

from .stream_models import CleanupPrompt

router = APIRouter(prefix="/worker", tags=["worker"])


class Mirrored(BaseModel):
    active: dict


@router.post("/sync/mirrored")
async def use_mirrored(request: Mirrored):
    """A stand-in copied the shared Kass: use its trained models, already
    copied here, and the learned corrections training checks against."""
    from ..services import correction_learning
    from ..services.model_improvement import manager

    active = manager.use_mirrored(request.active)
    correction_learning._state = None
    correction_learning.initialize()
    # The server saves the ones in use as its own (model_improvement.rs).
    return {"active": active}


class Startup(BaseModel):
    """What the app sends once the worker is up (app/server/src/startup.rs)."""

    stt_model: str
    llm_model: str
    auto_refine: bool
    language: str | None = None
    # CaptureSettings.shared_adapters, until /worker/shared-adapters/switched.
    shared_adapters: bool = True
    # The default style's cleanup prompt first, then the other styles apps are in.
    prompts: list[CleanupPrompt] = []


@router.post("/models/startup")
async def load_startup_models(request: Startup):
    """Load the saved models and warm the cleanup prompts dictation sends:
    when the app starts the worker, and when a stand-in starts dictating."""
    from ..services.model_startup import load_startup_models as load

    await load(request)
    return {}
