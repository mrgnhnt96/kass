"""Local-only correction learning; inference reads an immutable cache.

The model-improvement job runs it (model_improvement/manager.py) before each
adapter run and at the next idle moment after a report is saved or
withdrawn.
"""

import hashlib
import json
import logging
import os
import threading
from datetime import UTC, datetime

from .. import config
from ..database import session as database_session
from ..database.models import CaptureFeedback
from . import spoken_punctuation
from .correction_rules import MAX_TEXT, Example, apply_rules, compile_rules, evaluate, loss

logger = logging.getLogger(__name__)
_lock = threading.RLock()
_state = None
_path = None
_compiled = ()
# Words the speaker says for punctuation marks, and where a mark word stays a word.
_punctuation = ({}, frozenset())
# Reports changed since the last run; a withdrawn one also needs the adapter retrained.
_pending = False
_retrain = False


def _empty():
    return {
        "version": 1,
        "revision": 0,
        "rules": [],
        "history": [],
        "blocked": [],
        # Rule id -> the reports that contradicted it. Rollback blocks have none.
        "blocked_by": {},
        "last_run": None,
        "fingerprint": None,
        "evaluated_report_ids": [],
        "metrics": None,
        "outcome": "waiting",
        "punctuation": {"aliases": {}, "kept": []},
    }


def _punctuation_of(state):
    learned = state.get("punctuation") or {}
    return dict(learned.get("aliases", {})), frozenset(learned.get("kept", []))


def initialize():
    global _state, _path, _compiled, _punctuation
    with _lock:
        path = config.get_data_dir() / "correction-learning.json"
        if _state is not None and _path == path:
            return
        state = _empty()
        if path.exists():
            try:
                loaded = json.loads(path.read_text())
                if loaded["version"] != 1 or len(loaded["rules"]) > 32:
                    raise ValueError("Unsupported correction state")
                compile_rules(loaded["rules"])
                state.update(loaded)
            except (OSError, ValueError, KeyError, TypeError):
                logger.exception("Could not load correction learning; using no learned rules")
        _path, _state = path, state
        _compiled = compile_rules(state["rules"])
        _punctuation = _punctuation_of(state)


def _publish(state):
    global _state, _compiled, _punctuation
    compiled = compile_rules(state["rules"])
    _path.parent.mkdir(parents=True, exist_ok=True)
    temporary = _path.with_suffix(".tmp")
    try:
        with temporary.open("w") as stream:
            json.dump(state, stream, ensure_ascii=False, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(_path)
    finally:
        temporary.unlink(missing_ok=True)
    _state = state
    _compiled = compiled
    _punctuation = _punctuation_of(state)


def request_run(retrain=False):
    """Ask for a run at the next idle moment, and an adapter run if ``retrain``."""
    global _pending, _retrain
    _pending = True
    _retrain = _retrain or retrain


def pending():
    return _pending


def take_retrain():
    """Whether a withdrawn report asked for an adapter run; clears the request."""
    global _retrain
    retrain, _retrain = _retrain, False
    return retrain


def apply_learned_corrections(text, language=None):
    # No disk/DB access, locks, extra prompts, or model calls during dictation.
    # Very long transcripts skip the bounded, latency-tested rule layer.
    return apply_rules(text, _compiled, language) if len(text) <= MAX_TEXT else text


def learned_punctuation():
    """The speaker's own words for marks, and contexts where a mark word is a word."""
    # Read from memory like the rules: never disk during dictation.
    return _punctuation


def status():
    initialize()
    with _lock:
        return {
            "evaluated_report_ids": list(_state["evaluated_report_ids"]),
            "revision": _state["revision"],
            "active_rules": len(_state["rules"]),
            "last_run": _state["last_run"],
            "outcome": _state["outcome"],
            "metrics": _state["metrics"],
            "can_rollback": bool(_state["history"]),
        }


def _examples(db):
    rows = (
        db.query(CaptureFeedback)
        .order_by(CaptureFeedback.created_at.desc(), CaptureFeedback.id.desc())
        .limit(500)
        .all()
    )
    latest = {}
    for row in rows:
        key = (row.capture_id, row.target)
        if key in latest:
            continue
        latest[key] = row
    examples = []
    for row in reversed(list(latest.values())):
        try:
            snapshot = json.loads(row.snapshot)
            original = snapshot["transcript_raw" if row.target == "raw" else "transcript_refined"]
            if not original or max(len(original), len(row.expected_text)) > 1000:
                continue
            examples.append(
                Example(row.id, row.capture_id, original, row.expected_text, snapshot.get("language"), row.source)
            )
        except (ValueError, KeyError, TypeError):
            logger.warning("Skipping invalid correction snapshot %s", row.id)
    return examples


def run_job():
    """Run serially in a worker thread, owning the DB session in that thread."""
    global _pending
    initialize()
    with _lock:
        # A report saved during the run asks for another one.
        _pending = False
        with database_session.SessionLocal() as db:
            examples = _examples(db)
            causes = {report for reports in _state["blocked_by"].values() for report in reports}
            existing = {report for (report,) in db.query(CaptureFeedback.id).filter(CaptureFeedback.id.in_(causes))}
        fingerprint = hashlib.sha256(repr(examples).encode()).hexdigest()
        report_ids = [example.id for example in examples]
        # A contradiction's block lasts while a report behind it exists.
        lifted = {rule for rule, reports in _state["blocked_by"].items() if not existing & set(reports)}
        punctuation = spoken_punctuation.learn(examples)
        if (
            fingerprint == _state["fingerprint"]
            and report_ids == _state["evaluated_report_ids"]
            and not lifted
            and punctuation == _state.get("punctuation")
        ):
            return status()
        state = json.loads(json.dumps(_state))
        state["punctuation"] = punctuation
        state["blocked"] = [rule for rule in state["blocked"] if rule not in lifted]
        state["blocked_by"] = {rule: reports for rule, reports in state["blocked_by"].items() if rule not in lifted}
        active = state["rules"]
        # A new report contradicting a learned rule disables it before proposing
        # replacements. The block lasts until that report is withdrawn or
        # deleted.
        retained = []
        for rule in active:
            compiled = compile_rules([rule])
            contradicting = [
                e.id
                for e in examples
                if apply_rules(e.expected, compiled, e.language) != e.expected
                or loss(apply_rules(e.original, compiled, e.language), e.expected) > loss(e.original, e.expected)
            ]
            if contradicting:
                state["blocked"].append(rule["id"])
                state["blocked_by"][rule["id"]] = contradicting
            else:
                retained.append(rule)
        rules, metrics = evaluate(examples, retained, state["blocked"], every_report=True)
        if rules != active:
            state["history"] = (state["history"] + [{"revision": state["revision"], "rules": active}])[-10:]
            state["revision"] += 1
            state["rules"] = rules
        state.update(
            fingerprint=fingerprint,
            evaluated_report_ids=report_ids,
            metrics=metrics,
            last_run=datetime.now(UTC).isoformat(),
            outcome="updated" if rules != active else "no_change",
        )
        _publish(state)
        return status()


def rollback():
    initialize()
    with _lock:
        if not _state["history"]:
            raise ValueError("No previous correction version is available")
        state = json.loads(json.dumps(_state))
        previous = state["history"].pop()
        restored_ids = {r["id"] for r in previous["rules"]}
        state["blocked"] = list(
            set(state["blocked"]) | {r["id"] for r in state["rules"] if r["id"] not in restored_ids}
        )
        # A contradicted rule must not be restored by rolling back another change.
        state["rules"] = [r for r in previous["rules"] if r["id"] not in state["blocked"]]
        state["revision"] += 1
        state["outcome"] = "rolled_back"
        _publish(state)
        return status()
