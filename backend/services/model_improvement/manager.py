"""Idle scheduling, child-process supervision and atomic model promotion."""

import asyncio
import hashlib
import json
import logging
import os
import platform
import signal
import subprocess
import sys
import threading
import time
import uuid
from contextlib import suppress
from copy import deepcopy
from datetime import UTC, datetime
from functools import lru_cache
from pathlib import Path

from ... import config
from ...database import session as database_session
from .. import correction_learning, shared_adapters
from .data import collect, digest, readiness
from .evaluation import score_rows, score_speech

logger = logging.getLogger(__name__)
_lock = threading.RLock()
_state = None
_root = None
_active = {}
_thread = None
_process = None
_generation = 0
_last_activity = time.monotonic()
_last_attempt = 0.0
_foreground_count = 0
_recording_until = 0.0
_run_directory = None
IDLE_SECONDS = 120
INTERVAL_SECONDS = 6 * 3600


@lru_cache(maxsize=1)
def pipeline_id():
    # Hash bytecode without filenames/line tables, which change when a frozen
    # executable extracts into a new temporary directory on each launch.
    import types

    from .. import correction_rules, dictation_edits, refinement, spoken_corrections

    def stable(value):
        if isinstance(value, types.CodeType):
            return [value.co_code.hex(), list(value.co_names), [stable(c) for c in value.co_consts]]
        if isinstance(value, (str, int, float, bool)) or value is None:
            return value
        if isinstance(value, (list, tuple)):
            return [stable(item) for item in value]
        return type(value).__name__

    functions = [
        stable(value.__code__)
        for module in (refinement, dictation_edits, spoken_corrections, correction_rules)
        for _, value in sorted(vars(module).items())
        if isinstance(value, types.FunctionType) and value.__module__ == module.__name__
    ]
    prompts = [
        refinement.build_refinement_prompt(refinement.RefinementFlags(a, b, c))
        for a in (False, True)
        for b in (False, True)
        for c in (False, True)
    ]
    return digest([functions, prompts, refinement.REFINEMENT_EXAMPLES])


def _empty():
    return {
        "version": 1,
        "revision": 0,
        "phase": "waiting",
        "active": {},
        "history": [],
        "groups": [],
        "attempted": [],
        "blocked": [],
        "counts": {},
        "last_run": None,
        "metrics": None,
        "error": None,
        "evaluated_report_ids": [],
        # The last trained voice adapter, promoted or not, that the next run
        # continues from (docs/plans/VOICE_TRAINING.md).
        "voice_resume": None,
    }


def _valid_adapter(item):
    try:
        path = Path(item["path"])
        # All deployments must remain inside our own immutable run directories.
        path.resolve().relative_to(_root.resolve())
        return (
            item["pipeline"] == pipeline_id()
            and (path / "adapter_config.json").is_file()
            and hashlib.sha256((path / "adapter_config.json").read_bytes()).hexdigest() == item["config_sha256"]
            and hashlib.sha256((path / "adapters.safetensors").read_bytes()).hexdigest() == item["sha256"]
        )
    except (OSError, KeyError, ValueError, TypeError):
        return False


def _inside_root(path):
    Path(path).resolve().relative_to(_root.resolve())
    return Path(path)


def _valid_voice(item):
    from ..voice_training import lora

    try:
        path = _inside_root(item["path"])
        return lora.digests(path) == (item["sha256"], item["config_sha256"])
    except (OSError, KeyError, ValueError, TypeError):
        return False


def initialize():
    global _root, _state, _active
    with _lock:
        root = config.get_data_dir() / "model-improvement"
        if _root == root and _state is not None:
            return
        _root = root
        state = _empty()
        path = root / "state.json"
        if path.exists():
            try:
                loaded = json.loads(path.read_text())
                if loaded["version"] != 1:
                    raise ValueError("Unsupported learning state")
                state.update(loaded)
            except (OSError, ValueError, KeyError, TypeError):
                logger.exception("Could not read model improvement state")
        _state = state
        _active = deepcopy(state["active"])
        if "llm" in _active and not _valid_adapter(_active["llm"]):
            _active.pop("llm")
            state["active"] = deepcopy(_active)
            state["error"] = "The saved adapter failed integrity or pipeline checks; using the base model."
        if "voice" in _active and not _valid_voice(_active["voice"]):
            _active.pop("voice")
            state["active"] = deepcopy(_active)
            state["error"] = "The saved voice model failed its integrity check; using plain turbo."
        if state["phase"] in ("preparing", "training", "evaluating", "queued"):
            state["phase"] = "interrupted"


def _save():
    _root.mkdir(parents=True, exist_ok=True)
    temp = _root / "state.tmp"
    with temp.open("w") as stream:
        json.dump(_state, stream, indent=2)
        stream.flush()
        os.fsync(stream.fileno())
    temp.replace(_root / "state.json")


def status():
    initialize()
    with _lock:
        result = {key: deepcopy(_state[key]) for key in ("phase", "revision", "counts", "last_run", "metrics", "error")}
        result["active_adapter"] = _active.get("llm", {}).get("id")
        result["speech_model"] = _active.get("speech", {}).get("model")
        result["can_rollback"] = bool(_state["history"])
        result["voice"] = _voice_status()
        result["running"] = bool(_thread and _thread.is_alive())
        if result["running"] and _run_directory and result["phase"] in ("training", "evaluating"):
            try:
                progress = json.loads((_run_directory / "progress.json").read_text())
                if progress["phase"].startswith("evaluating"):
                    result["phase"] = "evaluating"
            except (OSError, ValueError, KeyError):
                pass
        return result


def active_adapter(model_size, flags):
    """The personal adapter for cleanup, else the shared one (shared_adapters.py), else None."""
    from ..refinement import RefinementFlags

    normalized = RefinementFlags.from_dict(flags).to_dict()
    return _personal_adapter(model_size, normalized) or shared_adapters.cleanup(model_size, normalized)


def _personal_adapter(model_size, flags):
    item = _active.get("llm")
    if not item or item["model_size"] != model_size or not shared_adapters.usable(item.get("shared")):
        return None
    if digest(correction_learning._state["rules"] if correction_learning._state else []) != item["rules_digest"]:
        return None
    if flags not in item["tested_flags"]:
        return None
    return item["path"]


def _voice_status():
    from ..voice_training import bank, sounds

    item = _active.get("voice")
    return {
        "active": item["id"] if item else None,
        "active_since": item.get("activated") if item else None,
        "bank": bank.summary(),
        "sounds": sounds.status(),
        "metrics": (_state.get("metrics") or {}).get("voice"),
        "training": (_state.get("metrics") or {}).get("voice_training"),
        "min_train": VOICE_MIN_TRAIN,
        "min_test": VOICE_MIN_TEST,
        # Undo restores the previous model version; offer it here only when
        # that would undo a voice model change.
        "can_undo": bool(_state["history"]) and _state["history"][-1]["active"].get("voice") != item,
    }


def _personal_voice(model_size):
    item = _active.get("voice")
    if not item or item["model_size"] != model_size or not shared_adapters.usable(item.get("shared")):
        return None
    return item["path"]


def voice_adapter(model_size):
    """The voice adapter to merge into ``model_size``: personal, else shared, else None for plain weights."""
    return _personal_voice(model_size) or shared_adapters.voice(model_size)


def quarantine_voice(message, path=None):
    """Stop using a voice adapter that failed to load; plain turbo takes over."""
    global _active
    initialize()
    with _lock:
        if "voice" not in _active or (path and path != _active["voice"]["path"]):
            if path:
                shared_adapters.quarantine(path)
            return
        _state["blocked"].append(_active["voice"]["id"])
        _active = {key: value for key, value in _active.items() if key != "voice"}
        _state["active"] = deepcopy(_active)
        _state["voice_resume"] = None
        _state["error"] = message
        _state["phase"] = "reverted"
        _save()


def speech_model(configured):
    item = _active.get("speech")
    return item["model"] if item and item["configured"] == configured else configured


def quarantine_adapter(message, path=None):
    global _active
    initialize()
    with _lock:
        if "llm" not in _active or (path and path != _active["llm"]["path"]):
            if path:
                shared_adapters.quarantine(path)
            return
        _state["blocked"].append(_active["llm"]["id"])
        _active = {key: value for key, value in _active.items() if key != "llm"}
        _state["active"] = deepcopy(_active)
        _state["error"] = message
        _state["phase"] = "reverted"
        _save()


def _stop_process(process):
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=1)
    except subprocess.TimeoutExpired:
        with suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        with suppress(subprocess.TimeoutExpired):
            process.wait(timeout=2)
    except ProcessLookupError:
        pass


def foreground_activity(recording=False):
    """Cancel training before foreground inference; never wait for its job lock."""
    global _generation, _last_activity, _recording_until
    with _lock:
        _last_activity = time.monotonic()
        if recording:
            _recording_until = _last_activity + 45
        _generation += 1
        process = _process
    _stop_process(process)


def idle_generation():
    """The foreground generation when nothing uses the models, else None.

    Other background jobs compare it later to notice a dictation started.
    """
    with _lock:
        now = time.monotonic()
        busy = (
            _foreground_count
            or now < _recording_until
            or now - _last_activity < IDLE_SECONDS
            or (_thread and _thread.is_alive())
        )
        return None if busy else _generation


def dictating():
    """Whether a dictation is recording or finishing right now."""
    with _lock:
        return bool(_foreground_count) or time.monotonic() < _recording_until


def interrupted(generation):
    return generation != _generation


def cancel():
    global _last_attempt
    foreground_activity()
    _last_attempt = time.monotonic()
    return status()


def begin_foreground():
    global _foreground_count
    with _lock:
        _foreground_count += 1
    foreground_activity()


def end_foreground():
    global _foreground_count, _last_activity
    with _lock:
        _foreground_count = max(0, _foreground_count - 1)
        _last_activity = time.monotonic()


def _cached(repo):
    from huggingface_hub import snapshot_download
    from huggingface_hub.errors import LocalEntryNotFoundError

    try:
        path = Path(snapshot_download(repo, local_files_only=True))
        if not any(path.glob("*.safetensors")) and not any(path.glob("*.bin")):
            return None
        return str(path)
    except LocalEntryNotFoundError:
        return None


def _prepare():
    from ...backends import WHISPER_HF_REPOS
    from ...backends.qwen_llm_backend import MLX_HF_REPOS
    from ..settings import get_capture_settings

    correction_learning.run_job()
    with _lock:
        groups = deepcopy(_state["groups"])
        active = deepcopy(_active)
    with database_session.SessionLocal() as db:
        from ..styles import flags_for, load as load_styles

        samples = collect(db, groups)
        settings = get_capture_settings(db)
        size = settings.llm_model or "0.6B"
        configured_stt = settings.stt_model or "base"
        # Every writing style's flags are evaluated, so an accepted adapter
        # covers each style (docs/plans/PER_APP_STYLE.md).
        style_flags = [flags_for(style, settings).to_dict() for style in load_styles(db).styles]
        voice_bank = _scan_bank(db, settings)
        language = None if (settings.language or "auto") == "auto" else settings.language
    counts, train_ready = readiness(samples)
    current_stt = speech_model(configured_stt)
    raw_tests = [s for s in samples if s["target"] == "raw" and s["split"] == "test" and s.get("audio")]
    speech_candidates = (
        [size for size in WHISPER_HF_REPOS if size != current_stt and _cached(WHISPER_HF_REPOS[size])]
        # Voice training specializes turbo; don't switch away from it meanwhile.
        if len(raw_tests) >= 5 and current_stt != "turbo"
        else []
    )
    rules = deepcopy(correction_learning._state["rules"])
    path = _cached(MLX_HF_REPOS[size]) if train_ready or (len(raw_tests) >= 5 and counts["audio_test"] >= 5) else None
    prior = active.get("llm")
    if prior and (
        prior["model_size"] != size
        or prior["rules_digest"] != digest(rules)
        or not shared_adapters.usable(prior.get("shared"))
    ):
        prior = None
    # Training starts from the shared adapter when the personal one isn't
    # built on this release's (docs/plans/SHARED_ADAPTERS.md); otherwise from
    # the personal one. Either way the candidate must beat the model in use.
    shared = shared_adapters.cleanup(size)
    shared_id = shared_adapters.cleanup_id(size) if shared else None
    if shared and (not prior or prior.get("shared") != shared_id):
        start, lineage = shared, shared_id
    else:
        start, lineage = (prior["path"], prior.get("shared")) if prior else (None, None)
    plan = {
        "samples": samples,
        "model_size": size,
        "model_path": path,
        "stt_model": current_stt,
        "configured_stt": configured_stt,
        "speech_candidates": speech_candidates,
        "rules": rules,
        "pipeline": pipeline_id(),
        "baseline_adapter": prior["path"] if prior else shared,
        "start_adapter": start,
        "shared": lineage,
        "train_ready": train_ready and bool(path),
        "has_training_data": train_ready,
        "baseline_revision": _state["revision"],
        "voice": None,
        "style_flags": style_flags,
        # Prompts for learned punctuation read each style's habits from here.
        "data_dir": str(config.get_data_dir()),
    }
    fingerprint = digest(
        {
            "samples": samples,
            "model": path,
            "stt": current_stt,
            "rules": rules,
            "pipeline": pipeline_id(),
            "start": start,
        }
    )
    # The refinement model trains first when it has new data; otherwise the
    # run trains the voice adapter. One at a time fits the worker's 30 minutes.
    with _lock:
        refinement_due = plan["train_ready"] and fingerprint not in _state["attempted"] + _state["blocked"]
    if voice_bank and current_stt == "turbo" and not refinement_due:
        plan["voice"] = _voice_plan(voice_bank, active, raw_tests, language)
        if plan["voice"]:
            plan["train_ready"] = False
            fingerprint = digest([fingerprint, plan["voice"]["bank_digest"], plan["voice"]["resume"]])
    with _lock:
        _state["groups"] = groups
        _state["counts"] = counts | {"speech_test": len(raw_tests)}
        _state["evaluated_report_ids"] = [s["id"] for s in samples]
        _save()
    ready = plan["train_ready"] or (len(raw_tests) >= 5 and bool(speech_candidates)) or bool(plan["voice"])
    return plan, fingerprint, ready


VOICE_MIN_TRAIN = 100
VOICE_MIN_TEST = 12


def _scan_bank(db, settings):
    from ..voice_training import bank

    try:
        return bank.scan(db, settings)
    except Exception:
        logger.exception("Voice bank scan failed; voice training waits")
        return None


def _voice_plan(summary, active, raw_tests, language):
    """The voice-training part of a run, or None until there's enough to train on."""
    from ...backends import WHISPER_HF_REPOS
    from ..voice_training import bank, sounds

    if summary["train"] < VOICE_MIN_TRAIN or summary["test"] < VOICE_MIN_TEST:
        return None
    if not sounds.ready() or not _cached(WHISPER_HF_REPOS["turbo"]):
        return None
    takes = []
    for take in bank.takes():
        try:
            take["hash"] = hashlib.sha256(Path(take["audio"]).read_bytes()).hexdigest()
        except OSError:
            continue
        takes.append(take)
    resume = _state.get("voice_resume")
    try:
        resume = str(_inside_root(resume)) if resume and (Path(resume) / "adapter.safetensors").is_file() else None
    except ValueError:
        resume = None
    personal = active.get("voice")
    if personal and not shared_adapters.usable(personal.get("shared")):
        personal = None
    shared = shared_adapters.voice("turbo")
    shared_id = shared_adapters.voice_id("turbo") if shared else None
    production = personal["path"] if personal else shared
    start = resume or (personal["path"] if personal else None)
    lineage = _voice_lineage(start) if start else None
    if (start and not shared_adapters.usable(lineage)) or (shared and lineage != shared_id):
        # Continue from this release's shared adapter rather than from a
        # model built on another one, or on plain turbo.
        start, lineage = shared, shared_id
    return {
        "repo": WHISPER_HF_REPOS["turbo"],
        "takes": takes,
        "room": bank.room_paths(),
        "language": language,
        "resume": start,
        "production": production,
        "shared": lineage,
        "bank_digest": bank.digest(),
        "corrections": [
            {"id": s["id"], "audio": s["audio"], "audio_hash": s["audio_hash"], "expected": s["expected"]}
            for s in raw_tests
        ],
        "train_seconds": 900,
        "max_updates": 250,
    }


def _voice_lineage(path):
    """The shared voice adapter a trained one was built on (its config's ``shared``), or None."""
    from ..voice_training import lora

    try:
        return json.loads((Path(path) / lora.CONFIG_FILE).read_text()).get("shared")
    except (OSError, ValueError):
        return None


def _command(plan):
    if getattr(sys, "frozen", False):
        return [sys.executable, "--improve-model", "--plan", str(plan), "--parent-pid", str(os.getpid())]
    return [
        sys.executable,
        "-m",
        "backend.services.model_improvement.worker",
        "--plan",
        str(plan),
        "--parent-pid",
        str(os.getpid()),
    ]


def _promote(plan, directory, result, fingerprint):
    """Recompute gates in the parent; publish one reversible deployment."""
    global _active
    if plan["baseline_revision"] != _state["revision"]:
        raise ValueError("The production model changed during evaluation")
    if plan["pipeline"] != pipeline_id() or digest(plan["rules"]) != digest(correction_learning._state["rules"]):
        raise ValueError("The refinement pipeline changed during evaluation")
    active = deepcopy(_active)
    metrics = {}
    if result.get("adapter"):
        report = result["adapter"]
        metrics["adapter"] = score_rows(report["rows"])
        if metrics["adapter"]["passed"]:
            from ..refinement import RefinementFlags

            path = directory / "adapter"
            active["llm"] = {
                "id": directory.name,
                "path": str(path),
                "model_size": plan["model_size"],
                "base_path": plan["model_path"],
                "pipeline": plan["pipeline"],
                "rules_digest": digest(plan["rules"]),
                # The shared adapter it was built on (shared_adapters.py), or None.
                "shared": plan.get("shared"),
                "tested_flags": [RefinementFlags.from_dict(flags).to_dict() for flags in report["tested_flags"]],
                "sha256": hashlib.sha256((path / "adapters.safetensors").read_bytes()).hexdigest(),
                "config_sha256": hashlib.sha256((path / "adapter_config.json").read_bytes()).hexdigest(),
            }
            if not _valid_adapter(active["llm"]):
                raise ValueError("Candidate adapter integrity check failed")
    if result.get("speech"):
        evaluations = result["speech"]["evaluations"]
        passing = [entry for entry in evaluations if score_speech(entry["rows"])["passed"]]
        metrics["speech"] = [dict(model=e["model"], **score_speech(e["rows"])) for e in evaluations]
        pipeline = result["speech"].get("pipeline")
        pipeline_passed = pipeline and score_rows(pipeline["rows"])["passed"]
        if pipeline:
            metrics["speech_pipeline"] = score_rows(pipeline["rows"])
        # Promote one independently evaluated component per revision.
        if (
            passing
            and pipeline_passed
            and not metrics.get("adapter", {}).get("passed")
            and not metrics.get("voice", {}).get("passed")
        ):
            winner = min(passing, key=lambda e: score_speech(e["rows"])["candidate_errors"])
            active["speech"] = {"model": winner["model"], "configured": plan["configured_stt"]}
    voice_resume = _state.get("voice_resume")
    if result.get("voice"):
        from ..voice_training import lora
        from ..voice_training.gate import score_voice, worth_continuing

        metrics["voice"] = score_voice(result["voice"]["rows"])
        metrics["voice_training"] = result["voice"].get("training")
        path = directory / "voice"
        # The next run continues from a candidate that is on its way, even if
        # it isn't good enough to use yet (gate.worth_continuing).
        if worth_continuing(metrics["voice"]):
            voice_resume = str(path)
        if metrics["voice"]["passed"] and not metrics.get("adapter", {}).get("passed"):
            sha, config_sha = lora.digests(path)
            active["voice"] = {
                "id": directory.name,
                "path": str(path),
                "model_size": "turbo",
                "sha256": sha,
                "config_sha256": config_sha,
                "activated": datetime.now(UTC).isoformat(),
                "shared": (plan.get("voice") or {}).get("shared"),
            }
            if not _valid_voice(active["voice"]):
                raise ValueError("Candidate voice adapter integrity check failed")
            active.pop("speech", None)
    previous = deepcopy(_state)
    changed = active != _active
    if changed:
        _state["history"] = (_state["history"] + [{"active": deepcopy(_active), "fingerprint": fingerprint}])[-10:]
        _state["revision"] += 1
        _state["active"] = active
    _state.update(
        phase="activated" if changed else "rejected",
        metrics=metrics,
        last_run=datetime.now(UTC).isoformat(),
        voice_resume=voice_resume,
    )
    _state["attempted"].append(fingerprint)
    try:
        _save()
    except OSError:
        _state.clear()
        _state.update(previous)
        raise
    voice_changed = active.get("voice") != _active.get("voice")
    _active = active
    if voice_changed:
        _reload_speech()


def _reload_speech():
    """Load the new voice model now if turbo is in memory, so the next dictation doesn't wait."""
    try:
        from ..mlx_thread import _mlx_executor
        from ..transcribe import get_whisper_model

        whisper = get_whisper_model()
        if getattr(whisper, "model", None) is not None and getattr(whisper, "model_size", None) == "turbo":
            _mlx_executor.submit(whisper._ensure_loaded_sync, "turbo")
    except Exception:
        logger.exception("Couldn't reload the speech model; it reloads on next use")


def _run(token):
    global _process, _last_attempt, _run_directory
    directory = None
    try:
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            with _lock:
                _state["phase"] = "unsupported"
                _last_attempt = time.monotonic()
            return
        plan, fingerprint, ready = _prepare()
        with _lock:
            if token != _generation:
                _state["phase"] = "paused"
                return
            if not ready:
                _state["phase"] = (
                    "waiting_model" if plan.get("has_training_data") and not plan["model_path"] else "waiting_data"
                )
                _state["last_run"] = datetime.now(UTC).isoformat()
                _save()
                _last_attempt = time.monotonic()
                return
            if fingerprint in _state["attempted"] or fingerprint in _state["blocked"]:
                _state["phase"] = "no_new_data"
                _last_attempt = time.monotonic()
                return
            # Do not start a GPU training job under memory pressure.
            import psutil

            if psutil.virtual_memory().available < 8 * 1024**3:
                _state["phase"] = "waiting_memory"
                return
            directory = _root / "runs" / uuid.uuid4().hex
            directory.mkdir(parents=True)
            _run_directory = directory
            (directory / "plan.json").write_text(json.dumps(plan, indent=2))
            with (directory / "worker.log").open("w") as log:
                _process = subprocess.Popen(
                    _command(directory / "plan.json"),
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                    cwd=str(Path(__file__).resolve().parents[3]),
                )
            _state["phase"] = "training" if plan["train_ready"] or plan["voice"] else "evaluating"
            process = _process
        try:
            exit_code = process.wait(timeout=1800)
        except subprocess.TimeoutExpired:
            _stop_process(process)
            raise RuntimeError("Training/evaluation exceeded the 30-minute budget") from None
        with _lock:
            _process = None
            if token != _generation:
                _state["phase"] = "paused"
                return
            if exit_code:
                raise RuntimeError(f"Model worker failed (exit {exit_code}); details: {directory / 'worker.log'}")
            result = json.loads((directory / "result.json").read_text())
            _promote(plan, directory, result, fingerprint)
            _last_attempt = time.monotonic()
    except Exception as error:
        logger.exception("Model improvement failed; production model unchanged")
        with _lock:
            _state["phase"] = "failed"
            _state["error"] = str(error)
            _last_attempt = time.monotonic()
            _save()
    finally:
        with _lock:
            _process = None
            # Detailed evaluation artifacts stay local for diagnosis and audit.
            if _state["phase"] == "paused":
                _save()


def start():
    global _thread
    initialize()
    with _lock:
        if _thread and _thread.is_alive():
            return status()
        if _foreground_count or time.monotonic() < _recording_until:
            _state["phase"] = "waiting_idle"
            return status()
        _state.update(phase="preparing", error=None)
        _thread = threading.Thread(target=_run, args=(_generation,), daemon=True, name="model-improvement")
        _thread.start()
        return status()


def rollback():
    global _active
    cancel()
    with _lock:
        if not _state["history"]:
            raise ValueError("No previous model version is available")
        previous = deepcopy(_state)
        entry = _state["history"].pop()
        active = entry["active"]
        if "llm" in active and not _valid_adapter(active["llm"]):
            active.pop("llm")
        if "voice" in active and not _valid_voice(active["voice"]):
            active.pop("voice")
        _state["blocked"].append(entry["fingerprint"])
        # Training continues from the restored voice model, not the undone one.
        _state["voice_resume"] = active.get("voice", {}).get("path")
        _state["active"] = active
        _state["revision"] += 1
        _state["phase"] = "rolled_back"
        try:
            _save()
        except OSError:
            _state.clear()
            _state.update(previous)
            raise
        voice_changed = active.get("voice") != _active.get("voice")
        _active = deepcopy(active)
        if voice_changed:
            _reload_speech()
        return status()


def _learn_rules():
    """Relearn correction rules; whether that invalidated the active adapter."""
    correction_learning.initialize()
    before = digest(correction_learning._state["rules"])
    try:
        correction_learning.run_job()
    except Exception:
        logger.exception("Correction learning failed; keeping the active rules")
        return False
    return bool(_active.get("llm")) and digest(correction_learning._state["rules"]) != before


async def periodic_job():
    initialize()
    try:
        while True:
            await asyncio.sleep(60)
            if idle_generation() is None:
                continue
            # A withdrawn report may be in the active adapter's training data.
            retrain = correction_learning.take_retrain() and bool(_active.get("llm"))
            if (
                retrain
                or time.monotonic() - _last_attempt >= INTERVAL_SECONDS
                # A saved or withdrawn report is learned from soon, not in six
                # hours. The adapter was tested with the old rules, so new ones
                # retest it.
                or (correction_learning.pending() and await asyncio.to_thread(_learn_rules))
            ):
                start()
    finally:
        await asyncio.to_thread(foreground_activity)
        if _thread and _thread.is_alive():
            await asyncio.to_thread(_thread.join, 5)
