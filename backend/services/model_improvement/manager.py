"""The worker's half of model improvement: the trained models dictation
uses, and planning, training and judging a run.

The Rust server owns the state in ``model-improvement/state.json``, the
schedule, status and rollback (server/src/model_improvement.rs). It asks the
worker to ``prepare`` a run, ``train`` it and ``use`` the trained models it
picks; the worker reads the models in use from state.json on start and never
writes it. A trained model the worker stops using (it failed its checks or
didn't load) is ``reported`` for the server to record.
"""

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
from pathlib import Path

from ... import config
from .. import capture_events, correction_learning, shared_adapters
from .data import ask, collect, digest, readiness, report_rows
from .evaluation import score_rows, score_speech

logger = logging.getLogger(__name__)
_lock = threading.RLock()
_root = None
_active = {}
_process = None
_generation = 0
_last_activity = time.monotonic()
_foreground_count = 0
_recording_until = 0.0
# Runs being prepared or trained: the models aren't idle meanwhile.
_runs = 0
# Trained models dropped since the server last asked (``reports``).
_reports = []
IDLE_SECONDS = 120
# What MLX prints when macOS stops a GPU job to keep the Mac responsive.
GPU_PREEMPTED = "kIOGPUCommandBufferCallbackErrorImpactingInteractivity"


def pipeline_id():
    """The cleanup pipeline dictation uses, as Kass's server (which runs it)
    named it when it started this worker: ``KASS_CLEANUP_PIPELINE``
    (kass_text::pipeline). None for a worker started by hand."""
    return os.environ.get("KASS_CLEANUP_PIPELINE") or None


def pipeline_matches(stamp):
    """Whether a cleanup adapter stamped ``stamp`` was tested with the
    pipeline dictation uses. Without a pipeline named, any stamp does."""
    current = pipeline_id()
    return current is None or stamp == current


def _valid_adapter(item):
    try:
        path = Path(item["path"])
        # All deployments must remain inside our own immutable run directories.
        path.resolve().relative_to(_root.resolve())
        return (
            pipeline_matches(item["pipeline"])
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


_INVALID = {
    "llm": "The saved adapter failed integrity or pipeline checks; using the base model.",
    "voice": "The saved voice model failed its integrity check; using plain turbo.",
}


def initialize():
    """Use the trained models the server's state.json names, once per data folder.

    One that fails its checks is left out and reported (:func:`reports`).
    """
    global _root, _active
    with _lock:
        root = config.get_data_dir() / "model-improvement"
        if _root == root:
            return
        _root = root
        active = {}
        try:
            loaded = json.loads((root / "state.json").read_text())
            if loaded["version"] != 1:
                raise ValueError("Unsupported learning state")
            active = dict(loaded.get("active") or {})
        except FileNotFoundError:
            pass
        except (OSError, ValueError, KeyError, TypeError):
            logger.exception("Could not read model improvement state")
        _active = _checked(active, report=True)


def _checked(active, report=False):
    """``active`` without the trained models that fail their checks."""
    checks = {"llm": _valid_adapter, "voice": _valid_voice}
    kept = {}
    for kind, item in active.items():
        if kind not in checks or checks[kind](item):
            kept[kind] = deepcopy(item)
        elif report:
            _reports.append({"kind": kind, "id": item.get("id"), "error": _INVALID[kind], "block": False})
    return kept


def check(active):
    """The trained models in ``active`` that pass their checks, for the server to save before :func:`use`."""
    initialize()
    return _checked(active)


def use(active):
    """Use the trained models the server saved; one that fails its checks is
    left out. Returns those in use."""
    global _active
    initialize()
    kept = _checked(active)
    with _lock:
        voice_changed = kept.get("voice") != _active.get("voice")
        _active = kept
    if voice_changed:
        _reload_speech()
    return deepcopy(kept)


def reports():
    """The trained models dropped since the last call, for the server to
    record: ``block`` ones failed while in use and aren't tried again."""
    initialize()
    with _lock:
        taken = list(_reports)
        _reports.clear()
    return taken


def _report(kind, message):
    """Stop using the ``kind`` trained model and report it; the caller holds the lock."""
    global _active
    _reports.append({"kind": kind, "id": _active[kind].get("id"), "error": message, "block": True})
    _active = {key: value for key, value in _active.items() if key != kind}
    capture_events.publish("learned")


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
    initialize()
    with _lock:
        if "voice" not in _active or (path and path != _active["voice"]["path"]):
            if path:
                shared_adapters.quarantine(path)
            return
        _report("voice", message)


def speech_model(configured):
    item = _active.get("speech")
    return item["model"] if item and item["configured"] == configured else configured


def quarantine_adapter(message, path=None):
    initialize()
    with _lock:
        if "llm" not in _active or (path and path != _active["llm"]["path"]):
            if path:
                shared_adapters.quarantine(path)
            return
        _report("llm", message)


def in_use():
    """The trained models in use, by kind, for a stand-in to copy (services/sync/mirror.py)."""
    initialize()
    with _lock:
        return deepcopy(_active)


def mirrored_root():
    """Where a stand-in keeps the shared Kass's trained models."""
    initialize()
    return _root / "mirrored"


def use_mirrored(active):
    """Use a shared Kass's trained models, already copied under :func:`mirrored_root`.
    One that fails its checks is left out, so dictation uses the base model.
    Returns those in use, for the server to save."""
    return use(active)


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
        busy = _foreground_count or now < _recording_until or now - _last_activity < IDLE_SECONDS or _runs
        return None if busy else _generation


def dictating():
    """Whether a dictation is recording or finishing right now."""
    with _lock:
        return bool(_foreground_count) or time.monotonic() < _recording_until


def foreground_generation():
    """Changes with every dictation or other foreground request."""
    with _lock:
        return _generation


def interrupted(generation):
    return generation != _generation


def stop():
    """Stop a run in progress: a dictation, the user or shutting down."""
    foreground_activity()


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


def _prepare(groups, voice_resume, skip, cleanup):
    """The plan for a run from the reports in kass.db, its fingerprint, whether
    it has anything to train or evaluate, and what the server keeps: the
    recording groups (``groups``, updated in place) and the counts.
    ``cleanup`` is what the server sent: its cleanup pipeline, the capture
    settings and each writing style's flags (:func:`prepare`)."""
    from ...backends import WHISPER_HF_REPOS
    from ...backends.qwen_llm_backend import MLX_HF_REPOS

    # The server learns the rules and has the worker reload them.
    correction_learning.initialize()
    with _lock:
        active = deepcopy(_active)
    settings = cleanup["settings"]
    samples = collect(report_rows(cleanup["server"]), groups)
    size = settings.get("llm_model") or "0.6B"
    configured_stt = settings.get("stt_model") or "base"
    # Every writing style's flags are evaluated, so an accepted adapter
    # covers each style (docs/plans/PER_APP_STYLE.md).
    style_flags = cleanup["style_flags"]
    voice_bank = _scan_bank(cleanup["server"], settings)
    language = None if (settings.get("language") or "auto") == "auto" else settings["language"]
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
        "pipeline": cleanup["pipeline"],
        # The child prepares its cleanup text here (worker.served_cases).
        "cleanup_server": cleanup["server"],
        "rehearsal": cleanup["rehearsal"],
        "baseline_adapter": prior["path"] if prior else shared,
        "start_adapter": start,
        "shared": lineage,
        "train_ready": train_ready and bool(path),
        "has_training_data": train_ready,
        "voice": None,
        "style_flags": style_flags,
        "data_dir": str(config.get_data_dir()),
    }
    fingerprint = digest(
        {
            "samples": samples,
            "model": path,
            "stt": current_stt,
            "rules": rules,
            "pipeline": cleanup["pipeline"],
            "start": start,
        }
    )
    # The refinement model trains first when it has new data; otherwise the
    # run trains the voice adapter. One at a time fits the worker's 30 minutes.
    refinement_due = plan["train_ready"] and fingerprint not in skip
    if voice_bank and current_stt == "turbo" and not refinement_due:
        plan["voice"] = _voice_plan(voice_bank, active, samples, language, voice_resume)
        if plan["voice"]:
            plan["train_ready"] = False
            fingerprint = digest([fingerprint, plan["voice"]["bank_digest"], plan["voice"]["resume"]])
    ready = plan["train_ready"] or (len(raw_tests) >= 5 and bool(speech_candidates)) or bool(plan["voice"])
    kept = {
        "counts": counts | {"speech_test": len(raw_tests)},
        "evaluated_report_ids": [s["id"] for s in samples],
    }
    return plan, fingerprint, ready, kept


VOICE_MIN_TRAIN = 100
VOICE_MIN_TEST = 12


def _scan_bank(server, settings):
    from ..voice_training import bank

    def dictations(after, limit):
        body = {"after": after, "limit": limit}
        return ask(server, "/capture/learning/training/dictations", body)["dictations"]

    def corrections(capture_ids):
        body = {"capture_ids": capture_ids}
        return ask(server, "/capture/learning/training/corrections", body)["corrections"]

    try:
        return bank.scan(settings, dictations, corrections)
    except Exception:
        logger.exception("Voice bank scan failed; voice training waits")
        return None


def _voice_plan(summary, active, samples, language, resume):
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
    personal = active.get("voice")
    try:
        resume = str(_inside_root(resume)) if resume and (Path(resume) / "adapter.safetensors").is_file() else None
    except ValueError:
        resume = None
    # A resume point chosen under a looser rule falls back to the model in use.
    if resume and not _still_worth_continuing(resume):
        resume = None
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
        "corrections": _voice_corrections(samples, {take["id"] for take in takes if take["split"] == "train"}),
        "train_seconds": 900,
        "max_updates": 250,
    }


def _voice_corrections(samples, trained):
    """The user's corrected test recordings, labelled with what they said.

    A fix of the transcript is the label as is. A fix of the cleaned-up text
    labels the transcript with the misheard terms it fixed
    (bank.corrected_transcript); one that fixed none says nothing about
    hearing. Recordings the voice bank trains on (``trained``) are left out.
    """
    from ..voice_training.bank import corrected_transcript

    corrections = []
    for sample in samples:
        if sample["split"] != "test" or not sample.get("audio") or sample["capture_id"] in trained:
            continue
        if sample["target"] == "raw":
            expected = sample["expected"]
        else:
            expected = corrected_transcript(sample["raw"], sample.get("output") or "", sample["expected"])
            if expected is None:
                continue
        corrections.append(
            {"id": sample["id"], "audio": sample["audio"], "audio_hash": sample["audio_hash"], "expected": expected}
        )
    return corrections


def _still_worth_continuing(path):
    """Whether the candidate at ``path`` still passes ``gate.worth_continuing``.

    Re-judged from its saved evaluation, so a resume point chosen under an
    older, looser rule is dropped.
    """
    from ..voice_training.gate import score_voice, worth_continuing

    try:
        rows = json.loads((Path(path).parent / "result.json").read_text())["voice"]["rows"]
        return worth_continuing(score_voice(rows))
    except (OSError, ValueError, KeyError, TypeError):
        return False


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


def _judge(plan, directory, result, voice_resume):
    """Recompute the gates here, not in the child: the trained models to use
    next (the ones in use unless a candidate passed), the measurements, and
    the voice adapter the next run continues from."""
    if not pipeline_matches(plan["pipeline"]) or digest(plan["rules"]) != digest(correction_learning._state["rules"]):
        raise ValueError("The refinement pipeline changed during evaluation")
    with _lock:
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
            # The model now in use is where training continues, even when it
            # won on noise by more clean words than worth_continuing allows.
            voice_resume = str(path)
    return {"active": active, "metrics": metrics, "voice_resume": voice_resume}


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


class _Busy:
    """The models aren't idle while a run is prepared or trained."""

    def __enter__(self):
        global _runs
        with _lock:
            _runs += 1

    def __exit__(self, *_):
        global _runs
        with _lock:
            _runs -= 1


# The plan :func:`prepare` made, until :func:`train` runs it.
_prepared = None


def prepare(
    groups,
    voice_resume,
    skip,
    pipeline=None,
    cleanup_server=None,
    rehearsal=None,
    settings=None,
    style_flags=None,
):
    """Plan a run for the server, from the reports in kass.db.

    ``skip`` is the fingerprints already tried or rolled back. ``pipeline``
    is the cleanup pipeline the server runs, ``cleanup_server`` where the
    worker reads kass.db's reports and dictations and the training process
    asks it for cleanup text, and ``rehearsal`` the prompt's example turns.
    ``settings`` are the capture settings and ``style_flags`` each writing
    style's cleanup flags. The answer says whether it's ``ready`` to train,
    or which phase it ``waits`` in, and names the ``run`` for :func:`train`.
    A dictation that starts meanwhile ``interrupted`` it.
    """
    global _prepared
    initialize()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        return {"unsupported": True}
    if not cleanup_server:
        return {"error": "The server didn't say where to prepare cleanup text"}
    cleanup = {
        "pipeline": pipeline or pipeline_id(),
        "server": cleanup_server,
        "rehearsal": rehearsal or [],
        "settings": settings or {},
        "style_flags": style_flags or [],
    }
    with _lock:
        generation = _generation
    with _Busy():
        groups = deepcopy(groups)
        plan, fingerprint, ready, kept = _prepare(groups, voice_resume, skip, cleanup)
    run = uuid.uuid4().hex
    with _lock:
        _prepared = {"run": run, "plan": plan, "fingerprint": fingerprint} if ready else None
        interrupted = generation != _generation
    waiting = None
    if not ready:
        waiting = "waiting_model" if plan.get("has_training_data") and not plan["model_path"] else "waiting_data"
    return kept | {
        "groups": groups,
        "ready": ready,
        "waiting": waiting,
        "fingerprint": fingerprint,
        "run": run if ready else None,
        # Training, or only evaluating other speech models.
        "trains": bool(plan["train_ready"] or plan["voice"]),
        "generation": generation,
        "interrupted": interrupted,
    }


def _preempted(log):
    """Whether the run in ``log`` died because macOS took the GPU back."""
    with suppress(OSError), log.open("rb") as stream:
        stream.seek(max(0, log.stat().st_size - 64 * 1024))
        return GPU_PREEMPTED.encode() in stream.read()
    return False


def train(run, generation, voice_resume):
    """Train and evaluate the run :func:`prepare` planned, in a child process.

    Answers ``interrupted`` when a dictation stopped it, ``waiting_memory``
    when the Mac is short of memory to start, ``gpu_preempted`` when macOS
    stopped it to keep the Mac responsive, else what to use next
    (:func:`_judge`). Raises when the run fails otherwise.
    """
    global _process, _prepared
    initialize()
    with _lock:
        prepared, _prepared = _prepared, None
    if not prepared or prepared["run"] != run:
        raise ValueError("That run wasn't prepared")
    plan = prepared["plan"]
    with _Busy():
        # Do not start a GPU training job under memory pressure.
        import psutil

        if psutil.virtual_memory().available < 8 * 1024**3:
            return {"waiting_memory": True}
        directory = _root / "runs" / run
        directory.mkdir(parents=True)
        (directory / "plan.json").write_text(json.dumps(plan, indent=2))
        with _lock:
            if generation != _generation:
                return {"interrupted": True}
            with (directory / "worker.log").open("w") as log:
                _process = subprocess.Popen(
                    _command(directory / "plan.json"),
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                    cwd=str(Path(__file__).resolve().parents[3]),
                )
            process = _process
        try:
            try:
                exit_code = process.wait(timeout=1800)
            except subprocess.TimeoutExpired:
                _stop_process(process)
                raise RuntimeError("Training/evaluation exceeded the 30-minute budget") from None
        finally:
            with _lock:
                _process = None
        if generation != _generation:
            return {"interrupted": True}
        if exit_code:
            if _preempted(directory / "worker.log"):
                # The server waits longer before the next run (model_improvement.rs).
                return {"error": "macOS stopped training to keep the Mac responsive", "gpu_preempted": True}
            raise RuntimeError(f"Model worker failed (exit {exit_code}); details: {directory / 'worker.log'}")
        # Detailed evaluation artifacts stay local for diagnosis and audit.
        result = json.loads((directory / "result.json").read_text())
        return _judge(plan, directory, result, voice_resume)
