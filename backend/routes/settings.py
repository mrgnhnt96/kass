"""What the Rust server needs from the worker when settings change.

The settings routes and the capture_settings row live in the Rust server; the
worker reads the row for dictation and switches the speech model's shared
adapter. History retention runs in the Rust server (captures::retention).
"""

from fastapi import APIRouter
from pydantic import BaseModel

from ..services import shared_adapters

router = APIRouter(prefix="/worker", tags=["worker"])


class SharedAdaptersSwitched(BaseModel):
    enabled: bool


@router.post("/shared-adapters/switched")
async def shared_adapters_switched(request: SharedAdaptersSwitched):
    shared_adapters.switched(request.enabled)
    return {"ok": True}
