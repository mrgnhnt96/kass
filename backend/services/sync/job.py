"""
The stand-in's side of sync: every few seconds, see whether the shared Kass
is up; when it is, send it what changed here, then copy what it has.

Also wakes the stand-in when the app falls back to it, loading the models it
left unloaded, and fetches the models the shared Kass uses while it's up so
falling back never waits on a download.
"""

import asyncio
import json
import logging
import time
from datetime import UTC, datetime
from pathlib import Path

import httpx

from ... import config
from ...database import session
from ...database.models import Capture
from ..task_queue import create_background_task
from . import changes, mirror, shared_server
from .rows import KINDS, to_json

logger = logging.getLogger(__name__)

CHECK_SECONDS = 5
# How often to copy from the shared Kass when nothing was sent to it.
COPY_SECONDS = 60
HEALTH_TIMEOUT = 2.0
# Changes per request: each capture brings its recording.
BATCH = 20

_state = {
    "reachable": None,
    "synced_at": None,
    "revision": None,
    "copied_at": 0.0,
    "error": None,
}
_awake = False
_wake_lock = asyncio.Lock()
_downloading: set[str] = set()
_warmth: asyncio.Task | None = None


def status() -> dict:
    with session.SessionLocal() as db:
        pending = changes.count(db)
    return {
        "standing_in": True,
        "shared_server": shared_server(),
        "reachable": _state["reachable"],
        "pending": pending,
        "synced_at": _state["synced_at"],
        "awake": _awake,
        "downloading": sorted(_downloading),
        "error": _state["error"],
    }


async def wake() -> None:
    """Load the models the stand-in left unloaded, once: the app is dictating here."""
    global _awake, _warmth
    async with _wake_lock:
        if _awake:
            return
        _awake = True
        from ..model_startup import load_startup_models
        from ..model_warmth import periodic_job as warmth_job

        logger.info("The shared Kass is away; loading models to dictate on this Mac")
        await load_startup_models()
        _warmth = asyncio.create_task(warmth_job())


def _key(model: type, ref: str):
    (column,) = model.__table__.primary_key.columns
    return int(ref) if column.type.python_type is int else ref


def _batch() -> tuple[list[dict], list[tuple[str, tuple[str, bytes, str]]], list[tuple]]:
    """Up to :data:`BATCH` changes as sent, their recordings, and what to forget once sent."""
    with session.SessionLocal() as db:
        batch = changes.pending(db)[:BATCH]
        payload, files, keys = [], [], []
        for change in batch:
            model = KINDS.get(change.kind)
            if model is None:
                continue
            row = None if change.deleted else db.get(model, _key(model, change.ref_id))
            payload.append(
                {
                    "kind": change.kind,
                    "id": change.ref_id,
                    "deleted": row is None,
                    "changed_at": change.changed_at.isoformat(),
                    "row": row and to_json(row),
                }
            )
            keys.append((change.kind, change.ref_id, change.changed_at))
            if isinstance(row, Capture) and not row.audio_deleted:
                path = config.resolve_storage_path(row.audio_path)
                if path and path.is_file():
                    files.append(("audio", (f"{row.id}{path.suffix}", path.read_bytes(), "application/octet-stream")))
        return payload, files, keys


async def _push(client: httpx.AsyncClient, url: str) -> bool:
    """Send one batch of changes; whether there was one."""
    payload, files, keys = await asyncio.to_thread(_batch)
    if not keys:
        return False
    response = await client.post(
        f"{url}/sync/import",
        data={"changes": json.dumps(payload)},
        files=files or None,
        timeout=120,
    )
    response.raise_for_status()

    def forget():
        with session.SessionLocal() as db:
            changes.sent(db, keys)

    await asyncio.to_thread(forget)
    logger.info("Sent %d changes to the shared Kass", len(keys))
    return True


def _download(client: httpx.Client, url: str):
    def download(kind: str, name: str, destination: Path) -> None:
        temporary = destination.with_suffix(destination.suffix + ".part")
        with client.stream("GET", f"{url}/sync/adapters/{kind}/{name}", timeout=300) as response:
            response.raise_for_status()
            with temporary.open("wb") as stream:
                for chunk in response.iter_bytes():
                    stream.write(chunk)
        temporary.replace(destination)

    return download


def _copy(url: str, data: dict) -> None:
    with httpx.Client() as client, session.SessionLocal() as db:
        mirror.apply(db, data, _download(client, url))


def _missing_models(settings: dict) -> list[str]:
    """The Hugging Face repos dictation will need here that aren't downloaded."""
    from ...backends import WHISPER_HF_REPOS
    from ...backends.qwen_llm_backend import MLX_HF_REPOS
    from .. import llm, transcribe
    from ..model_improvement import manager

    missing = []
    stt = manager.speech_model(settings.get("stt_model"))
    if stt and not transcribe.get_whisper_model()._is_model_cached(stt):
        missing.append(WHISPER_HF_REPOS.get(stt, f"openai/whisper-{stt}"))
    size = settings.get("llm_model")
    if settings.get("auto_refine") and size in MLX_HF_REPOS and not llm.get_llm_model()._is_model_cached(size):
        missing.append(MLX_HF_REPOS[size])
    return missing


async def _fetch_models(settings: dict) -> None:
    """Download, without loading, the models the shared Kass dictates with."""
    from huggingface_hub import snapshot_download

    for repo in await asyncio.to_thread(_missing_models, settings):
        if repo in _downloading:
            continue
        _downloading.add(repo)

        async def fetch(repo=repo):
            try:
                logger.info("Downloading %s so this Mac can dictate while the shared Kass is away", repo)
                await asyncio.to_thread(snapshot_download, repo)
            except Exception:
                logger.warning("Could not download %s", repo, exc_info=True)
            finally:
                _downloading.discard(repo)

        create_background_task(fetch())


async def sync_once(client: httpx.AsyncClient, url: str) -> None:
    try:
        response = await client.get(f"{url}/health", timeout=HEALTH_TIMEOUT)
        response.raise_for_status()
    except httpx.HTTPError:
        _state["reachable"] = False
        return
    _state["reachable"] = True
    sent = False
    while await _push(client, url):
        sent = True
    if not sent and time.monotonic() - _state["copied_at"] < COPY_SECONDS and _state["revision"]:
        return
    if await asyncio.to_thread(_pending):
        return
    response = await client.get(f"{url}/sync/snapshot", timeout=30)
    response.raise_for_status()
    data = response.json()
    _state["copied_at"] = time.monotonic()
    if data.get("revision") != _state["revision"]:
        await asyncio.to_thread(_copy, url, data)
        _state["revision"] = data.get("revision")
        logger.info("Copied settings, dictionary and learning from the shared Kass")
    _state["synced_at"] = datetime.now(UTC).isoformat()
    _state["error"] = None
    if data.get("settings"):
        await _fetch_models(data["settings"])


def _pending() -> int:
    with session.SessionLocal() as db:
        return changes.count(db)


def _failed(message: str, exc_info: bool = False) -> None:
    """Note a failed sync, logging it only when it's a new failure: they repeat every few seconds."""
    if message != _state["error"]:
        logger.warning(message, exc_info=exc_info)
    _state["error"] = message


async def periodic_job() -> None:
    url = shared_server()
    if url is None:
        return
    async with httpx.AsyncClient() as client:
        while True:
            try:
                await sync_once(client, url)
            except httpx.HTTPStatusError as error:
                # Up, but it turned the request down: an older Kass, say.
                _failed(f"The shared Kass at {url} turned down a sync: {error}")
            except httpx.HTTPError as error:
                _state["reachable"] = False
                _failed(f"Could not sync with the shared Kass at {url}: {error}")
            except Exception as error:
                _failed(f"Syncing with the shared Kass failed: {error}", exc_info=True)
            await asyncio.sleep(CHECK_SECONDS)
