"""Capture (voice input) endpoints."""

import logging

from fastapi import APIRouter, Depends, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse, StreamingResponse
from sqlalchemy.orm import Session

from .. import config, models
from ..backends import get_llm_model_configs, get_stt_model_configs
from ..backends.base import is_model_cached
from ..database import Capture as DBCapture, get_db
from ..services import captures as captures_service, settings as settings_service
from ..services.refinement import RefinementFlags

logger = logging.getLogger(__name__)

router = APIRouter()


@router.get("/capture/learning")
def correction_learning_status():
    from ..services.correction_learning import status
    from ..services.model_improvement import manager

    result = status()
    result["model"] = manager.status()
    return result


@router.post("/capture/learning/run")
def run_correction_learning():
    from ..services.correction_learning import run_job
    from ..services.model_improvement import manager

    result = run_job()
    result["model"] = manager.start()
    return result


@router.post("/capture/learning/activity")
def pause_learning_for_recording():
    from ..services.model_improvement.manager import foreground_activity

    foreground_activity(recording=True)
    return {"paused": True}


@router.post("/capture/learning/cancel")
def cancel_model_learning():
    from ..services.model_improvement.manager import cancel

    return cancel()


@router.post("/capture/learning/voice/sounds")
def download_voice_sounds():
    """Start the one-time download of background sounds for voice training."""
    from ..services.voice_training import sounds

    return sounds.start()


@router.post("/capture/learning/rollback")
def rollback_correction_learning():
    from ..services.correction_learning import rollback
    from ..services.model_improvement import manager

    try:
        if manager.status()["can_rollback"]:
            manager.rollback()
            return correction_learning_status()
        return rollback()
    except ValueError as error:
        raise HTTPException(status_code=409, detail=str(error)) from error


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


@router.get("/captures", response_model=models.CaptureListResponse)
async def list_captures_endpoint(
    limit: int = 50,
    offset: int = 0,
    app_bundle_id: str | None = None,
    unknown_app: bool = False,
    db: Session = Depends(get_db),
):
    """The newest captures first. ``app_bundle_id`` keeps one app's;
    ``unknown_app`` keeps those with no app recorded."""
    if limit < 1 or limit > 200:
        raise HTTPException(status_code=400, detail="limit must be between 1 and 200")
    if offset < 0:
        raise HTTPException(status_code=400, detail="offset must be >= 0")

    items, total = captures_service.list_captures(
        db,
        limit=limit,
        offset=offset,
        app_bundle_id=app_bundle_id,
        unknown_app=unknown_app,
    )
    return models.CaptureListResponse(items=items, total=total)


@router.get("/captures/apps", response_model=models.CaptureAppsResponse)
async def list_capture_apps_endpoint(db: Session = Depends(get_db)):
    """Capture counts per app for the Captures app list."""
    return captures_service.list_capture_apps(db)


@router.get("/captures/stats", response_model=models.UsageStatsResponse)
async def capture_stats_endpoint(
    period: str = "7d",
    app_bundle_id: str | None = None,
    unknown_app: bool = False,
    db: Session = Depends(get_db),
):
    """Dictation stats for one period (today, 7d, 30d, all), in the Mac's
    local days. ``app_bundle_id`` and ``unknown_app`` narrow them like the
    capture list."""
    from ..services import usage_stats

    if period not in usage_stats.PERIODS:
        raise HTTPException(status_code=400, detail=f"period must be one of {', '.join(usage_stats.PERIODS)}")
    scope = usage_stats.AppScope(bundle_id=app_bundle_id or None, unknown=unknown_app)
    return usage_stats.usage_stats(db, period, scope)


@router.get("/captures/events")
async def capture_events_endpoint():
    """Server-Sent Events: ``changed`` whenever captures are added, edited or
    deleted, from this Mac or another using it (beta ``network``)."""
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


@router.get("/captures/{capture_id}", response_model=models.CaptureResponse)
async def get_capture_endpoint(capture_id: str, db: Session = Depends(get_db)):
    capture = captures_service.get_capture(capture_id, db)
    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture


@router.get("/captures/{capture_id}/audio")
async def get_capture_audio_endpoint(capture_id: str, db: Session = Depends(get_db)):
    """Stream the original capture audio file."""
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    if not row:
        raise HTTPException(status_code=404, detail="Capture not found")

    audio_path = config.resolve_storage_path(row.audio_path)
    if audio_path is None or not audio_path.exists():
        raise HTTPException(status_code=404, detail="Audio file not found")

    return FileResponse(
        audio_path,
        media_type="audio/wav",
        filename=f"capture_{capture_id}.wav",
    )


@router.delete("/captures/{capture_id}")
async def delete_capture_endpoint(capture_id: str, db: Session = Depends(get_db)):
    deleted = captures_service.delete_capture(capture_id, db)
    if not deleted:
        raise HTTPException(status_code=404, detail="Capture not found")
    return {"message": f"Capture {capture_id} deleted"}


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


@router.post("/captures/{capture_id}/feedback", response_model=models.CaptureFeedbackResponse)
async def report_capture_output(
    capture_id: str,
    request: models.CaptureFeedbackCreate,
    db: Session = Depends(get_db),
):
    from ..services.capture_feedback import save_feedback

    try:
        feedback = save_feedback(capture_id, request, db)
    except ValueError as error:
        raise HTTPException(status_code=409, detail=str(error)) from error
    if feedback is None:
        raise HTTPException(status_code=404, detail="Capture not found")
    return feedback


@router.delete("/captures/{capture_id}/feedback/{report_id}", status_code=204)
async def withdraw_capture_report(capture_id: str, report_id: str, db: Session = Depends(get_db)):
    """Withdraw one report and what it taught (docs/plans/CORRECTION_LEARNING.md)."""
    from ..services.capture_feedback import withdraw_feedback

    if not withdraw_feedback(capture_id, report_id, db):
        raise HTTPException(status_code=404, detail="Report not found")


@router.get("/captures/{capture_id}/feedback", response_model=list[models.CaptureFeedbackResponse])
async def list_capture_feedback(capture_id: str, db: Session = Depends(get_db)):
    from ..services.capture_feedback import list_feedback

    if captures_service.get_capture(capture_id, db) is None:
        raise HTTPException(status_code=404, detail="Capture not found")
    return list_feedback(db, capture_id)


@router.get("/capture/feedback/export", response_model=list[models.CaptureFeedbackResponse])
async def export_capture_feedback(db: Session = Depends(get_db)):
    """Export local correction snapshots; audio remains available on each capture."""
    from ..services.capture_feedback import list_feedback

    return list_feedback(db)
