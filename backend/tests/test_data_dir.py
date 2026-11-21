"""The worker's data directory before --data-dir sets one (backend/config.py)."""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _data_dir(cwd: Path, kass_data_dir: str | None) -> Path:
    env = {k: v for k, v in os.environ.items() if k != "KASS_DATA_DIR"}
    env["PYTHONPATH"] = str(ROOT)
    if kass_data_dir is not None:
        env["KASS_DATA_DIR"] = kass_data_dir
    out = subprocess.run(
        [sys.executable, "-c", "from backend import config; print(config.get_data_dir())"],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    return Path(out.stdout.strip())


def test_kass_data_dir_sets_the_data_directory(tmp_path):
    app_dir = tmp_path / "Application Support" / "com.mrgnhnt.kass"

    assert _data_dir(tmp_path, str(app_dir)) == app_dir.resolve()


def test_without_kass_data_dir_it_is_data_in_the_working_directory(tmp_path):
    assert _data_dir(tmp_path, None) == (tmp_path / "data").resolve()


def test_an_empty_kass_data_dir_is_ignored(tmp_path):
    assert _data_dir(tmp_path, "") == (tmp_path / "data").resolve()
