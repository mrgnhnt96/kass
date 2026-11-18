"""Capture (voice input) endpoints that run the models.

The Rust server answers the capture list, each capture and its audio, the
reports on them and the events stream from kass.db. The worker keeps saving,
refining and retranscribing captures and model learning, and ``worker_router``
holds what the server asks of it for the rest. The server learns corrections
(routes/learning.py has what it asks the worker).
"""

import logging

from fastapi import APIRouter, Depends, File, Form, HTTPException, UploadFile
from fastapi.responses import StreamingResponse
from pydantic import BaseModel
from sqlalchemy.orm import Session

from .. import models
from ..backends import get_llm_model_configs, get_stt_model_configs
from ..backends.base import is_model_cached
from ..database import Capture as DBCapture, get_db
from ..services import captures as captures_service, settings as settings_service
from ..services.refinement import RefinementFlags

logger = logging.getLogger(__name__)

router = APIRouter()
worker_router = APIRouter(prefix="/worker/captures", tags=["worker"])


UPLOAD_CHUNK_SIZE = 1024 * 1024  # 1 MB


@router.post("/captures", response_model=models.CaptureCreateResponse)
async def create_capture_endpoint(
    file: UploadFile = File(...),
    source: str = Form("file"),
    language: str | None = Form(None),
    stt_model: str | None = Form(None),
    app_bundle_id: str | None = Form(None),
    app_name: str | None = Form(None),
    db: Session = Depends(get_db),
):
    """Upload audio, run STT, persist the capture."""
    chunks = []
    while chunk := await file.read(UPLOAD_CHUNK_SIZE):
        chunks.append(chunk)
    audio_bytes = b"".join(chunks)

    if not audio_bytes:
        raise HTTPException(status_code=400, detail="Uploaded file is empty")

    saved = settings_service.get_capture_settings(db)
    resolved_stt = stt_model or saved.stt_model
    if language is None:
        resolved_language = None if saved.language == "auto" else saved.language
    else:
        resolved_language = None if language == "auto" else language

    bundle_id, name = captures_service.target_app(app_bundle_id, app_name)
    try:
        capture = await captures_service.create_capture(
            audio_bytes=audio_bytes,
            filename=file.filename or "capture.wav",
            source=source,
            language=resolved_language,
            stt_model=resolved_stt,
            db=db,
            app_bundle_id=bundle_id,
            app_name=name,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e)) from e
    except Exception as e:
        logger.exception("Failed to create capture")
        raise HTTPException(status_code=500, detail=str(e)) from e

    # A command recording always goes on to its rewrite (POST /commands/run),
    # which replaces the selection whatever the paste setting says.
    is_command = source == "command"
    return models.CaptureCreateResponse(
        **capture.model_dump(),
        auto_refine=is_command or bool(saved.auto_refine),
        allow_auto_paste=is_command or bool(saved.allow_auto_paste),
    )


@router.post("/captures/{capture_id}/refine", response_model=models.CaptureResponse)
async def refine_capture_endpoint(
    capture_id: str,
    request: models.CaptureRefineRequest,
    db: Session = Depends(get_db),
):
    saved = settings_service.get_capture_settings(db)
    if request.flags is not None:
        flags = RefinementFlags.from_dict(request.flags.model_dump())
    else:
        # Cleaned up in the style its corrections teach: the one asked for
        # by name ("use formal mode"), else the app's.
        from ..services.styles import correction_style, flags_for, snapshot

        existing = db.get(DBCapture, capture_id)
        styles = snapshot()
        style_id = correction_style(styles, existing.app_bundle_id, existing.teaches_style_id) if existing else None
        flags = flags_for(styles.resolve(style_id), saved)

    resolved_model = request.model_size or saved.llm_model

    try:
        capture = await captures_service.refine_capture(
            capture_id=capture_id,
            flags=flags,
            model_size=resolved_model,
            db=db,
        )
    except Exception as e:
        logger.exception("Refinement failed for capture %s", capture_id)
        raise HTTPException(status_code=500, detail=str(e)) from e

    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture


@router.get("/capture/readiness", response_model=models.CaptureReadinessResponse)
async def capture_readiness_endpoint(db: Session = Depends(get_db)):
    """Whether the STT and LLM models the user has selected are downloaded.

    The frontend gates the global hotkey on this — pressing the chord with
    a missing model would otherwise produce a stuck "transcribing" pill that
    waits forever for a download to finish. Checks on-disk cache, not RAM
    load, so the answer survives backend restarts.
    """
    saved = settings_service.get_capture_settings(db)

    stt_cfg = next(
        (c for c in get_stt_model_configs() if c.model_size == saved.stt_model),
        None,
    )
    llm_cfg = next(
        (c for c in get_llm_model_configs() if c.model_size == saved.llm_model),
        None,
    )

    if stt_cfg is None or llm_cfg is None:
        # Should be impossible — both fields are pattern-validated against
        # known sizes — but bail loudly rather than return half a response.
        raise HTTPException(
            status_code=500,
            detail=f"No model config for stt={saved.stt_model} or llm={saved.llm_model}",
        )

    return models.CaptureReadinessResponse(
        stt=models.ModelReadiness(
            ready=is_model_cached(stt_cfg.hf_repo_id),
            model_name=stt_cfg.model_name,
            display_name=stt_cfg.display_name,
            size=stt_cfg.model_size,
            size_mb=stt_cfg.size_mb or None,
        ),
        llm=models.ModelReadiness(
            ready=is_model_cached(llm_cfg.hf_repo_id),
            model_name=llm_cfg.model_name,
            display_name=llm_cfg.display_name,
            size=llm_cfg.model_size,
            size_mb=llm_cfg.size_mb or None,
        ),
    )


@router.post("/captures/{capture_id}/retranscribe", response_model=models.CaptureResponse)
async def retranscribe_capture_endpoint(
    capture_id: str,
    request: models.CaptureRetranscribeRequest,
    db: Session = Depends(get_db),
):
    saved = settings_service.get_capture_settings(db)
    resolved_stt = request.model or saved.stt_model
    if request.language is None:
        resolved_language = None if saved.language == "auto" else saved.language
    else:
        resolved_language = request.language

    try:
        capture = await captures_service.retranscribe_capture(
            capture_id=capture_id,
            stt_model=resolved_stt,
            language=resolved_language,
            db=db,
        )
    except FileNotFoundError as e:
        raise HTTPException(status_code=410, detail=str(e)) from e
    except Exception as e:
        logger.exception("Retranscribe failed for capture %s", capture_id)
        raise HTTPException(status_code=500, detail=str(e)) from e

    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture


# -- What the Rust server asks of the worker ----------------------------------


class ChangedReports(BaseModel):
    target: str
    source: str


class ReportsChangedRequest(BaseModel):
    changed: list[ChangedReports] = []
    forgotten: str | None = None


@worker_router.post("/reports-changed")
def reports_changed_endpoint(request: ReportsChangedRequest, db: Session = Depends(get_db)):
    """The server saved, withdrew or deleted reports: relearn from them, and
    drop a deleted capture's voice take."""
    from ..services import capture_feedback, dictionary
    from ..services.voice_training import bank

    # The server wrote the dictionary words the reports fixed or took back.
    dictionary.invalidate()
    for item in request.changed:
        capture_feedback.reports_changed(item.target, item.source, db)
    if request.forgotten is not None:
        bank.forget(request.forgotten)
    return {"ok": True}


@worker_router.get("/events")
async def capture_events_endpoint():
    """The worker's own capture events (saved, refined, learned), which the
    server passes on to the apps."""
    from ..services import capture_events

    return StreamingResponse(
        capture_events.subscribe(None),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )
