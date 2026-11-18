"""The model calls the Rust server (kass-server) makes for sharing Kass.

The Rust server serves /sync/* and runs a stand-in's sync with the shared
Kass. The worker only switches to the shared Kass's trained models once
they're copied, and loads its models when the app falls back to a stand-in.
"""

from fastapi import APIRouter
from pydantic import BaseModel

router = APIRouter(prefix="/worker", tags=["worker"])


class Mirrored(BaseModel):
    active: dict


@router.post("/sync/mirrored")
async def use_mirrored(request: Mirrored):
    """A stand-in copied the shared Kass: use its trained models, already
    copied here, and read everything else it copied afresh."""
    from ..services import (
        correction_learning,
        correction_notes,
        dictionary,
        known_names,
        personal_examples,
        styles,
        writing_style,
    )
    from ..services.model_improvement import manager

    active = manager.use_mirrored(request.active)
    writing_style._state = None
    correction_notes._state = None
    correction_learning._state = None
    correction_learning.initialize()
    personal_examples.invalidate()
    known_names.invalidate()
    styles.invalidate()
    dictionary.invalidate()
    # The server saves the ones in use as its own (model_improvement.rs).
    return {"active": active}


@router.post("/models/startup")
async def load_startup_models():
    """Load the models a stand-in leaves unloaded: the app dictates on it."""
    from ..services.model_startup import load_startup_models as load

    await load()
    return {}
