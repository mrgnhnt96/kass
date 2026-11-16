"""What the Rust server needs from the worker when settings change.

The settings routes and the capture_settings row live in the Rust server; the
worker reads the row for dictation, sweeps history past the retention window,
and switches the speech model's shared adapter.
"""

import asyncio

from fastapi import APIRouter
from pydantic import BaseModel

from ..services import history_retention, shared_adapters

router = APIRouter(prefix="/worker", tags=["worker"])


class SharedAdaptersSwitched(BaseModel):
    enabled: bool


@router.post("/history/sweep")
async def sweep_history():
    """Delete captures past a newly chosen window now, not at the next hourly sweep."""
    deleted = await asyncio.to_thread(history_retention.sweep)
    return {"deleted": deleted}


@router.post("/shared-adapters/switched")
async def shared_adapters_switched(request: SharedAdaptersSwitched):
    shared_adapters.switched(request.enabled)
    return {"ok": True}
