"""Dictation's cleanup, from the Rust crate that runs it, for the adapter scripts.

Kass's server runs dictation's cleanup in Rust and names its pipeline
(kass_text::pipeline); the app drops a shared cleanup adapter stamped with
another. Adapters are trained and scored on the same preprocessing and
prompt. This asks the Rust crate for both, so it needs cargo.
"""

import json
import subprocess
from functools import cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _example(name, stdin=None):
    manifest = ROOT / "app" / "Cargo.toml"
    command = ["cargo", "run", "--quiet", "--release", "--manifest-path", str(manifest)]
    command += ["-p", "kass-text", "--example", name]
    return subprocess.run(command, check=True, capture_output=True, text=True, input=stdin).stdout


@cache
def pipeline_id():
    return _example("pipeline_id").strip()


def cleanup_cases(samples):
    """What cleanup gives the model for each sample's ``raw`` text and
    ``flags`` (None for the defaults), as on a new install: ``text``,
    ``bypass`` (the finished text when a spoken edit resolved it, so the model
    is skipped), ``system`` and ``examples``. Also the built-in example pairs,
    which training rehearses."""
    request = {"samples": [{"raw": sample["raw"], "flags": sample.get("flags")} for sample in samples]}
    answer = json.loads(_example("cleanup_cases", json.dumps(request)))
    return answer["cases"], [tuple(pair) for pair in answer["rehearsal"]]
