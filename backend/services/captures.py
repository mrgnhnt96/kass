"""
Captures service — persists raw audio alongside its STT transcript and,
optionally, an LLM-refined version.

A capture is a single voice input event (dictation, long-form recording, or
uploaded file). Storage mirrors the generations flow: audio lives under
``data/captures/<id>.wav`` and rows live in the ``captures`` table.
"""

import contextlib
import json
import logging
import time
import uuid
from pathlib import Path

import soundfile as sf
from sqlalchemy import func
from sqlalchemy.orm import Session

from .. import config
from ..database import Capture as DBCapture
from ..models import (
    CaptureAppCount,
    CaptureAppsResponse,
    CaptureResponse,
    RefinementFlagsModel,
    RefinementReviewModel,
)
from ..utils.audio import load_audio
from . import history_retention
from .content_check import check_refinement, summarize_reviews
from .refinement import RefinementFlags, refine_transcript, style_first_word
from .transcribe import get_whisper_model
from .voice_commands import mark_commands

logger = logging.getLogger(__name__)


VALID_SOURCES = {"dictation", "recording", "file", "command"}
# Suffixes whisper's miniaudio loader can read directly. Anything outside
# this set has to go through librosa for decode + a soundfile transcode
# before whisper sees it.
WHISPER_NATIVE_FORMATS = (".wav", ".mp3", ".flac", ".ogg")


MAX_APP_FIELD_CHARS = 255


def _clean_app_field(value: object) -> str | None:
    if not isinstance(value, str):
        return None
    return value.strip()[:MAX_APP_FIELD_CHARS] or None


def target_app(bundle_id: object, name: object) -> tuple[str | None, str | None]:
    """The dictation's target app as stored: blank or non-string parts become None."""
    return _clean_app_field(bundle_id), _clean_app_field(name)


def target_app_category(category: object) -> str | None:
    """The target app's App Store category (``LSApplicationCategoryType``), as stored."""
    return _clean_app_field(category)


def app_categories(db: Session) -> dict[str, str | None]:
    """Each app's App Store category, from its newest capture that has one."""
    rows = (
        db.query(DBCapture.app_bundle_id, DBCapture.app_category, func.max(DBCapture.created_at))
        .filter(DBCapture.app_bundle_id.isnot(None))
        .group_by(DBCapture.app_bundle_id, DBCapture.app_category)
        .all()
    )
    categories: dict[str, str | None] = {}
    latest: dict[str, object] = {}
    for bundle_id, category, when in rows:
        categories.setdefault(bundle_id, None)
        if category and (bundle_id not in latest or (when and when > latest[bundle_id])):
            categories[bundle_id], latest[bundle_id] = category, when
    return categories


def spoken_style(transcript: str, bundle_id: str | None) -> tuple[str, str | None]:
    """``transcript`` without an opening request for a style ("use formal mode"), and the style
    it asked for when that isn't the app's own: the one its corrections teach."""
    from .styles import snapshot, spoken_style as find_spoken_style

    styles = snapshot()
    style, transcript = find_spoken_style(transcript, styles)
    return transcript, style.id if style and style.id != styles.for_app(bundle_id).id else None


def _to_response(row: DBCapture) -> CaptureResponse:
    flags_model: RefinementFlagsModel | None = None
    if row.refinement_flags:
        try:
            flags_model = RefinementFlagsModel(**json.loads(row.refinement_flags))
        except (ValueError, TypeError):
            flags_model = None

    review = None
    if row.refinement_review:
        try:
            review = RefinementReviewModel(**json.loads(row.refinement_review))
        except (ValueError, TypeError):
            review = None

    return CaptureResponse(
        id=row.id,
        audio_path=row.audio_path,
        source=row.source,
        language=row.language,
        duration_ms=row.duration_ms,
        transcript_raw=row.transcript_raw or "",
        transcript_refined=row.transcript_refined,
        stt_model=row.stt_model,
        llm_model=row.llm_model,
        refinement_flags=flags_model,
        refinement_review=review,
        app_bundle_id=row.app_bundle_id,
        app_name=row.app_name,
        command_selection=row.command_selection,
        command_instruction=row.command_instruction,
        command_transform=row.command_transform,
        style_id=row.style_id,
        audio_deleted=bool(row.audio_deleted),
        mac_id=row.mac_id,
        mac_name=row.mac_name,
        created_at=row.created_at,
    )


async def create_capture(
    *,
    audio_bytes: bytes,
    filename: str,
    source: str,
    language: str | None,
    stt_model: str | None,
    db: Session,
    app_bundle_id: str | None = None,
    app_name: str | None = None,
) -> CaptureResponse:
    """Persist raw audio, run STT, store the row."""
    if source not in VALID_SOURCES:
        raise ValueError(f"Invalid source '{source}'. Must be one of {sorted(VALID_SOURCES)}")

    capture_id = str(uuid.uuid4())
    suffix = Path(filename).suffix.lower() or ".wav"
    if suffix not in (".wav", ".mp3", ".m4a", ".flac", ".ogg", ".webm"):
        suffix = ".wav"

    raw_path = config.get_captures_dir() / f"{capture_id}{suffix}"
    written_files: list[Path] = []

    try:
        raw_path.write_bytes(audio_bytes)
        written_files.append(raw_path)

        preparation_started = time.monotonic()
        # Decode once with librosa — its audioread fallback handles webm/opus
        # via ffmpeg, which miniaudio (used inside mlx-audio's whisper) can't.
        # The decoded array gives us an accurate duration and becomes the
        # canonical WAV we hand to whisper.
        try:
            if suffix == ".wav":
                # Dictation is already WAV. Read its header rather than invoking
                # librosa's cold imports/JIT and resampling just for duration.
                info = sf.info(str(raw_path))
                duration_ms = round(info.duration * 1000)
                audio, sr = None, None
            else:
                audio, sr = load_audio(str(raw_path))
                duration_ms = int((len(audio) / sr) * 1000) if sr else None
        except Exception as decode_err:
            logger.warning("Could not decode capture %s (%s): %r", capture_id, suffix, decode_err)
            audio, sr = None, None
            duration_ms = None

        if audio is None or sr is None:
            # Decode failed. Only pass the file straight to whisper if the
            # source is a format its miniaudio loader can still read — webm,
            # m4a, etc. would just 500 later. Surface a clean error instead.
            if suffix not in WHISPER_NATIVE_FORMATS:
                raise ValueError(f"Could not decode {suffix} audio — the recording may be empty or corrupt")
            audio_path = raw_path
        elif suffix == ".wav":
            audio_path = raw_path
        else:
            # Transcode to WAV so downstream loaders (miniaudio, soundfile) work
            # regardless of what format the client shipped.
            audio_path = config.get_captures_dir() / f"{capture_id}.wav"
            sf.write(str(audio_path), audio, sr, format="WAV")
            written_files.append(audio_path)
            with contextlib.suppress(OSError):
                raw_path.unlink()
                written_files.remove(raw_path)

        logger.info("Capture %s audio preparation: %.3fs", capture_id, time.monotonic() - preparation_started)
        whisper = get_whisper_model()
        resolved_stt = stt_model or whisper.model_size
        from .model_improvement.manager import speech_model

        resolved_stt = speech_model(resolved_stt)
        transcription_started = time.monotonic()
        from . import dictionary

        terms = dictionary.for_app(app_bundle_id).terms
        transcript, teaches = spoken_style(
            await whisper.transcribe(str(audio_path), language, resolved_stt, vocabulary=terms), app_bundle_id
        )
        transcript = mark_commands(transcript)
        logger.info(
            "Capture %s transcription (including model load/queue): %.3fs for %sms audio",
            capture_id,
            time.monotonic() - transcription_started,
            duration_ms,
        )

        from .audio_retention import discard, discards_now
        from .settings import get_capture_settings

        stored_audio = config.to_storage_path(audio_path)
        if discards_now(get_capture_settings(db)):
            discard(audio_path)
            written_files.remove(audio_path)
            stored_audio = ""
        row = DBCapture(
            id=capture_id,
            audio_path=stored_audio,
            audio_deleted=not stored_audio,
            source=source,
            language=language,
            duration_ms=duration_ms,
            transcript_raw=transcript,
            stt_model=resolved_stt,
            app_bundle_id=app_bundle_id,
            app_name=app_name,
            teaches_style_id=teaches,
        )
        db.add(row)
        db.commit()
        db.refresh(row)
    except Exception:
        # Anything between the first write and the commit means the audio on
        # disk has no row pointing at it — clean up so data/captures doesn't
        # accumulate orphan blobs across failed transcribes.
        for path in written_files:
            with contextlib.suppress(OSError):
                path.unlink()
        raise

    return _to_response(row)


def list_captures(
    db: Session,
    limit: int = 50,
    offset: int = 0,
    app_bundle_id: str | None = None,
    unknown_app: bool = False,
) -> tuple[list[CaptureResponse], int]:
    """The newest captures first, optionally only one app's, or only those
    with no app recorded (uploads, and dictation from before apps were saved)."""
    query = db.query(DBCapture)
    if unknown_app:
        query = query.filter(DBCapture.app_bundle_id.is_(None))
    elif app_bundle_id:
        query = query.filter(DBCapture.app_bundle_id == app_bundle_id)
    total = query.count()
    rows = query.order_by(DBCapture.created_at.desc()).limit(limit).offset(offset).all()
    return [_to_response(r) for r in rows], total


def list_capture_apps(db: Session) -> CaptureAppsResponse:
    """How many captures each app has, most first, counted over every row
    rather than the page the list loads. An app is its bundle id; its name is
    the one on its newest capture, since an app can be renamed. Each app
    carries the style its dictation uses, whether the user chose it, and the
    style suggested for it until they do."""
    from .styles import snapshot, suggest_styles

    styles = snapshot()
    suggested = suggest_styles(app_categories(db), styles)
    counts: dict[str, list] = {}
    unknown = 0
    rows = (
        db.query(
            DBCapture.app_bundle_id,
            DBCapture.app_name,
            func.count(DBCapture.id),
            func.max(DBCapture.created_at),
        )
        .group_by(DBCapture.app_bundle_id, DBCapture.app_name)
        .all()
    )
    for bundle_id, name, count, latest in rows:
        if bundle_id is None:
            unknown += count
            continue
        entry = counts.setdefault(bundle_id, [None, 0, None])
        if name and (entry[2] is None or (latest and latest > entry[2])):
            entry[0] = name
        entry[1] += count
        if latest and (entry[2] is None or latest > entry[2]):
            entry[2] = latest
    apps = [
        CaptureAppCount(
            app_bundle_id=bundle_id,
            app_name=name,
            count=count,
            last_captured_at=latest,
            style_id=styles.for_app(bundle_id).id,
            confirmed=bundle_id in styles.apps,
            suggested_style_id=suggested.get(bundle_id),
        )
        for bundle_id, (name, count, latest) in counts.items()
    ]
    apps.sort(key=lambda app: (-app.count, (app.app_name or app.app_bundle_id).lower()))
    return CaptureAppsResponse(
        total=sum(app.count for app in apps) + unknown,
        unknown_count=unknown,
        apps=apps,
    )


def get_capture(capture_id: str, db: Session) -> CaptureResponse | None:
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    return _to_response(row) if row else None


def delete_capture(capture_id: str, db: Session) -> bool:
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    if not row:
        return False

    resolved = config.resolve_storage_path(row.audio_path)
    if resolved and resolved.exists():
        try:
            resolved.unlink()
        except OSError:
            logger.exception("Failed to remove capture audio %s", resolved)

    from .capture_feedback import forget_capture
    from .voice_training import bank

    forget_capture(capture_id, db)
    bank.forget(capture_id)
    db.delete(row)
    db.commit()
    return True


async def refine_capture(
    capture_id: str,
    flags: RefinementFlags,
    model_size: str | None,
    db: Session,
) -> CaptureResponse | None:
    with history_retention.in_use(capture_id):
        return await _refine_capture(capture_id, flags, model_size, db)


async def _refine_capture(
    capture_id: str,
    flags: RefinementFlags,
    model_size: str | None,
    db: Session,
) -> CaptureResponse | None:
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    if not row:
        return None

    refined, llm_size = await refine_transcript(
        row.transcript_raw or "",
        flags,
        model_size=model_size,
    )
    refined, verdict = check_refinement(row.transcript_raw or "", refined, flags)

    from . import dictionary
    from .correction_learning import apply_learned_corrections

    # The user's own dictionary entries win over learned corrections.
    found = dictionary.for_app(row.app_bundle_id, flags.style)
    refined = found.apply(apply_learned_corrections(refined, row.language))
    refined = style_first_word(refined, flags, found.names)

    row.transcript_refined = refined
    row.style_id = flags.style
    review = summarize_reviews([verdict])
    row.refinement_review = json.dumps(review) if review else None
    row.llm_model = llm_size
    row.refinement_flags = json.dumps(flags.to_dict())
    db.commit()
    db.refresh(row)
    return _to_response(row)


async def retranscribe_capture(
    capture_id: str,
    stt_model: str | None,
    language: str | None,
    db: Session,
) -> CaptureResponse | None:
    with history_retention.in_use(capture_id):
        return await _retranscribe_capture(capture_id, stt_model, language, db)


async def _retranscribe_capture(
    capture_id: str,
    stt_model: str | None,
    language: str | None,
    db: Session,
) -> CaptureResponse | None:
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    if not row:
        return None

    resolved = config.resolve_storage_path(row.audio_path)
    if not resolved or not resolved.exists():
        raise FileNotFoundError(f"Audio for capture {capture_id} is missing")

    whisper = get_whisper_model()
    resolved_stt = stt_model or whisper.model_size
    from . import dictionary

    terms = dictionary.for_app(row.app_bundle_id).terms
    transcript, teaches = spoken_style(
        await whisper.transcribe(str(resolved), language, resolved_stt, vocabulary=terms), row.app_bundle_id
    )
    transcript = mark_commands(transcript)

    row.transcript_raw = transcript
    row.teaches_style_id = teaches or row.teaches_style_id
    row.stt_model = resolved_stt
    if language:
        row.language = language
    # Refined text is stale after a fresh STT pass — force a re-refine.
    row.transcript_refined = None
    row.llm_model = None
    row.refinement_flags = None
    db.commit()
    db.refresh(row)
    return _to_response(row)
