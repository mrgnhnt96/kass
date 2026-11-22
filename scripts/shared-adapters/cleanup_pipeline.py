"""The cleanup pipeline's id, which shared cleanup adapters are stamped with.

Kass's server runs dictation's cleanup in Rust and names its pipeline
(kass_text::pipeline); the app drops a shared cleanup adapter stamped with
another. This asks the Rust crate for it, so building it needs cargo.
"""

import subprocess
from functools import cache
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


@cache
def pipeline_id():
    manifest = ROOT / "app" / "Cargo.toml"
    command = ["cargo", "run", "--quiet", "--release", "--manifest-path", str(manifest)]
    command += ["-p", "kass-text", "--example", "pipeline_id"]
    return subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()
