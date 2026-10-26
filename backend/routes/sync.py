"""Sync between a shared Kass and a Mac's stand-in for it (services/sync)."""

import json

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import FileResponse
from sqlalchemy.orm import Session

from ..database import get_db
from ..services import sync
from ..services.sync import importer, mirror

router = APIRouter(prefix="/sync", tags=["sync"])

# A batch's changes, as JSON: its rows' text, never its recordings.
MAX_CHANGES_BYTES = 32 * 1024 * 1024


@router.get("/snapshot")
async def snapshot_endpoint(db: Session = Depends(get_db)):
    """What a stand-in copies from this Kass."""
    return mirror.snapshot(db)


@router.get("/adapters/{kind}/{name}")
async def adapter_file_endpoint(kind: str, name: str):
    """A file of a trained model in use, for a stand-in to copy."""
    path = mirror.adapter_file(kind, name)
    if path is None:
        raise HTTPException(status_code=404, detail="No such model file in use")
    return FileResponse(path)


@router.post("/import", status_code=204)
async def import_endpoint(request: Request, db: Session = Depends(get_db)):
    """Apply what a stand-in changed while this Kass was away."""
    if sync.standing_in():
        raise HTTPException(status_code=409, detail="This Kass stands in for another; send changes to that one")
    form = await request.form(max_part_size=MAX_CHANGES_BYTES)
    try:
        changes = json.loads(form["changes"])
    except (KeyError, TypeError, ValueError) as error:
        raise HTTPException(status_code=422, detail="Expected the changes as JSON") from error
    audio = {}
    for upload in form.getlist("audio"):
        if hasattr(upload, "read") and upload.filename:
            stem = upload.filename.rsplit(".", 1)[0]
            audio[stem] = (upload.filename, await upload.read())
    importer.apply(db, changes, audio)


@router.get("/status")
async def status_endpoint():
    """Whether this server stands in for a shared Kass, and how syncing with it goes."""
    if not sync.standing_in():
        return {"standing_in": False}
    from ..services.sync import job

    return job.status()


@router.post("/wake", status_code=204)
async def wake_endpoint():
    """The app is about to dictate here: load the models a stand-in leaves unloaded."""
    if not sync.standing_in():
        return
    from ..services.sync import job

    await job.wake()
