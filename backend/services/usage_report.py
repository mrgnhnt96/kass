"""
Anonymous daily usage reports, sent to Amplitude when the user shares usage
(``CaptureSettings.share_usage``, on by default and offered at the end of
onboarding).

One ``daily_usage`` event per local day with dictation: counts, words,
speaking time, time saved, pace, how fast takes landed and how many failed.
The setup it ran on (chip, memory, macOS, models, which features are on)
goes along as user properties. Never any text, audio, dictionary words,
style rules or app names, and no IP address (none is set on the event).

Only finished days are sent. ``usage_sent_through`` is the last day sent,
so days the Mac was off or Kass wasn't running are caught up on the next
run, in order. Each event's insert id is the install id plus its day, so a
day sent twice (a crash between the upload and saving the marker) counts
once. A first report catches up on every earlier day; turning sharing back
on starts from that day, so the days it was off are never sent.

Runs a minute after startup, then hourly. Only in the built app: a dev
server never sends unless ``KASS_USAGE_REPORTS=1``, and
``KASS_USAGE_REPORTS=0`` stops any build from sending.
"""

import asyncio
import json
import logging
import os
import platform
import subprocess
import sys
import uuid
from datetime import UTC, date, datetime, time, timedelta
from functools import cache
from typing import Any

import httpx
from sqlalchemy.orm import Session

from .. import __version__
from ..database import (
    CaptureSettings as DBCaptureSettings,
    DictionaryEntry,
    TakeReport,
    WritingStyle,
    session as database_session,
)
from . import settings as settings_service, usage_stats

logger = logging.getLogger(__name__)

# Public by design: it ships in every client, like the site's.
API_KEY = "15288b16e4a64d54978fa9d86adddad1"
ENDPOINT = "https://api2.amplitude.com/2/httpapi"
EVENT = "daily_usage"
# Amplitude takes far more per request; small batches keep a long catch-up light.
BATCH = 100
FIRST_RUN_DELAY_SECONDS = 60
RUN_SECONDS = 60 * 60
# Take reports never wait longer than this to be sent.
KEEP_TAKE_REPORTS_DAYS = 60
DICTIONARY_BUCKETS = ((0, "0"), (10, "1-10"), (50, "11-50"), (200, "51-200"))


def sending_allowed() -> bool:
    flag = os.environ.get("KASS_USAGE_REPORTS")
    if flag is not None:
        return flag == "1"
    return bool(getattr(sys, "frozen", False))


def _utc(day: date) -> datetime:
    return usage_stats._utc(day)


def _local_day(created_at: datetime) -> date:
    return created_at.replace(tzinfo=UTC).astimezone().date()


@cache
def _sysctl(name: str) -> str | None:
    try:
        return subprocess.run(["sysctl", "-n", name], capture_output=True, text=True, timeout=5).stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def machine() -> dict[str, Any]:
    """The Mac's chip, memory and macOS version."""
    memory = _sysctl("hw.memsize")
    return {
        "chip": _sysctl("machdep.cpu.brand_string"),
        "memory_gb": round(int(memory) / 2**30) if memory and memory.isdigit() else None,
        "os_version": platform.mac_ver()[0] or None,
    }


def _dictionary_bucket(count: int) -> str:
    for most, label in DICTIONARY_BUCKETS:
        if count <= most:
            return label
    return "200+"


def _voice_training_on() -> bool:
    try:
        from .model_improvement import manager

        return manager.status().get("active_adapter") is not None
    except Exception:
        return False


def setup_properties(db: Session, row: DBCaptureSettings) -> dict[str, Any]:
    """Which models and features are in use, without anything the user wrote."""
    return {
        "speech_model": row.stt_model,
        "cleanup_model": row.llm_model,
        "auto_refine": row.auto_refine,
        "live_text": row.live_text,
        "voice_edits": row.voice_edits,
        "command_mode": bool(row.chord_command_keys),
        "sound_cues": row.sound_cues,
        "keep_recordings": not row.discard_audio,
        "history_days": row.history_retention_days,
        "styles": db.query(WritingStyle).count(),
        "dictionary_size": _dictionary_bucket(db.query(DictionaryEntry).count()),
        "voice_training": _voice_training_on(),
    }


def _first_day(db: Session) -> date | None:
    days = [usage_stats.first_day(db)]
    first_take = db.query(TakeReport.created_at).order_by(TakeReport.created_at).first()
    if first_take:
        days.append(_local_day(first_take[0]))
    days = [d for d in days if d]
    return min(days) if days else None


def _takes_by_day(db: Session, start: date, end: date) -> dict[date, list[TakeReport]]:
    rows = (
        db.query(TakeReport)
        .filter(TakeReport.created_at >= _utc(start), TakeReport.created_at < _utc(end + timedelta(days=1)))
        .all()
    )
    days: dict[date, list[TakeReport]] = {}
    for row in rows:
        day = _local_day(row.created_at)
        days.setdefault(day, []).append(row)
    return days


def day_properties(totals, takes: list[TakeReport]) -> dict[str, Any]:
    """One day's numbers. ``totals`` is ``None`` on a day with takes but no saved dictation."""
    latencies = sorted(t.latency_ms for t in takes if t.outcome == "delivered" and t.latency_ms is not None)
    return {
        "dictations": totals.captures if totals else 0,
        "words": totals.words if totals else 0,
        "speaking_seconds": round(totals.speaking_ms / 1000) if totals else 0,
        "time_saved_seconds": round(totals.time_saved_ms / 1000) if totals else 0,
        "pace_wpm": totals.pace_wpm if totals else None,
        "fixed_dictations": totals.fixed_captures if totals else 0,
        "delivered_takes": sum(1 for t in takes if t.outcome == "delivered"),
        "failed_takes": sum(1 for t in takes if t.outcome == "failed"),
        "command_takes": sum(1 for t in takes if t.mode == "command"),
        "latency_p50_ms": usage_stats._quantile(latencies, 0.5),
        "latency_p95_ms": usage_stats._quantile(latencies, 0.95),
    }


def pending_days(row: DBCaptureSettings, first: date | None, today: date) -> tuple[date, date] | None:
    """The finished days not yet sent, oldest first, or ``None``."""
    if row.usage_sent_through:
        start = date.fromisoformat(row.usage_sent_through) + timedelta(days=1)
    elif first:
        start = first
    else:
        return None
    end = today - timedelta(days=1)
    return (start, end) if start <= end else None


def build_events(db: Session, row: DBCaptureSettings, start: date, end: date) -> list[dict[str, Any]]:
    """A ``daily_usage`` event for each day from ``start`` to ``end`` with any dictation."""
    totals = usage_stats.daily_totals(db, start, end)
    takes = _takes_by_day(db, start, end)
    hardware = machine()
    user_properties = {**setup_properties(db, row), "chip": hardware["chip"], "memory_gb": hardware["memory_gb"]}
    events = []
    for day in sorted(set(totals) | set(takes)):
        events.append(
            {
                "event_type": EVENT,
                "device_id": row.usage_device_id,
                "time": round(datetime.combine(day, time(12)).astimezone().timestamp() * 1000),
                "insert_id": f"{row.usage_device_id}-{day.isoformat()}",
                "event_properties": {"day": day.isoformat(), **day_properties(totals.get(day), takes.get(day, []))},
                "user_properties": user_properties,
                "app_version": __version__,
                "platform": "macOS",
                "os_name": "macOS",
                "os_version": hardware["os_version"],
                "device_manufacturer": "Apple",
                "device_model": hardware["chip"],
            }
        )
    return events


def _post(events: list[dict[str, Any]]) -> bool:
    # Exactly what leaves the Mac (less the public API key), so anyone can check it in server.log.
    logger.info("Sending usage to %s: %s", ENDPOINT, json.dumps(events))
    try:
        response = httpx.post(ENDPOINT, json={"api_key": API_KEY, "events": events}, timeout=15)
    except httpx.HTTPError as error:
        logger.info("Usage report not sent: %s", error)
        return False
    if response.status_code != 200:
        logger.warning("Usage report refused: HTTP %s %s", response.status_code, response.text[:200])
        return False
    return True


def _forget_takes_before(db: Session, day: date) -> None:
    db.query(TakeReport).filter(TakeReport.created_at < _utc(day)).delete()
    db.commit()


def report(db: Session, today: date | None = None, post=_post) -> int:
    """Send every finished day not yet sent. Returns how many events went."""
    today = today or date.today()
    row = settings_service.get_capture_settings(db)
    _forget_takes_before(db, today - timedelta(days=KEEP_TAKE_REPORTS_DAYS))
    if not row.share_usage:
        # Nothing from a day with sharing off is ever sent.
        _forget_takes_before(db, today)
        return 0
    if not row.onboarding_completed:
        return 0
    if not row.usage_device_id:
        row.usage_device_id = str(uuid.uuid4())
        db.commit()
    days = pending_days(row, _first_day(db), today)
    if days is None:
        return 0
    start, end = days
    sent = 0
    while start <= end:
        batch_end = min(end, start + timedelta(days=BATCH - 1))
        events = build_events(db, row, start, batch_end)
        if events and not post(events):
            return sent
        sent += len(events)
        row.usage_sent_through = batch_end.isoformat()
        db.commit()
        _forget_takes_before(db, batch_end + timedelta(days=1))
        start = batch_end + timedelta(days=1)
    return sent


def run_once() -> None:
    if not sending_allowed():
        return
    # init_db binds SessionLocal after import, so look it up when the job runs.
    with database_session.SessionLocal() as db:
        sent = report(db)
    if sent:
        logger.info("Sent %d day(s) of usage", sent)


async def periodic_job():
    """A minute after startup, so it never competes with loading models, then hourly."""
    await asyncio.sleep(FIRST_RUN_DELAY_SECONDS)
    while True:
        try:
            await asyncio.to_thread(run_once)
        except Exception:
            logger.exception("Usage report failed; trying again later")
        await asyncio.sleep(RUN_SECONDS)
