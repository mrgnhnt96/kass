"""The learned corrections model improvement checks a trained adapter against.

The Rust server learns them from the user's reports, keeps them in
correction-learning.json (server/src/learning.rs) and applies them to
dictations; it asks the worker to ``reload`` the file when it changes. A
personal cleanup adapter is used only with the rules it was tested with.
"""

import json
import logging
import threading

from .. import config
from . import capture_events

logger = logging.getLogger(__name__)
_lock = threading.RLock()
_state = None
_path = None


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


def initialize():
    global _state, _path
    with _lock:
        path = config.get_data_dir() / "correction-learning.json"
        if _state is not None and _path == path:
            return
        state = _empty()
        if path.exists():
            try:
                loaded = json.loads(path.read_text())
                if loaded["version"] != 1 or not isinstance(loaded["rules"], list) or len(loaded["rules"]) > 32:
                    raise ValueError("Unsupported correction state")
                state.update(loaded)
            except (OSError, ValueError, KeyError, TypeError):
                logger.exception("Could not load correction learning; using no learned rules")
        _path, _state = path, state


def reload():
    """Read the file again after the server changed it."""
    global _state
    with _lock:
        _state = None
        initialize()
    capture_events.publish("learned")
    return status()


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
