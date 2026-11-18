"""Learned corrections, applied to dictations from memory.

The Rust server learns them from the user's reports and keeps them in
correction-learning.json (server/src/learning.rs); it asks the worker to
``reload`` the file when it changes.
"""

import json
import logging
import threading

from .. import config
from . import capture_events
from .correction_rules import MAX_TEXT, apply_rules, compile_rules

logger = logging.getLogger(__name__)
_lock = threading.RLock()
_state = None
_path = None
_compiled = ()
# Words the speaker says for punctuation marks, and where a mark word stays a word.
_punctuation = ({}, frozenset())


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


def reload():
    """Read the file again after the server changed it."""
    global _state
    with _lock:
        _state = None
        initialize()
    capture_events.publish("learned")
    return status()


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
