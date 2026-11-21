"""The worker reads the notes the Rust server summarized into cleanup.

Summarizing them is the Rust server's (server/src/correction_notes.rs), and
so are listing and removing them (tests/learned.rs).
"""

import json

import pytest

from backend import config
from backend.services import correction_notes
from backend.services.refinement import RefinementFlags, build_refinement_prompt
from backend.services.styles import MIGRATED_STYLE

RULE = "Write Kass as one word."


@pytest.fixture(autouse=True)
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(correction_notes, "_state", None)
    return tmp_path


def _write(storage, state):
    (storage / "correction-notes.json").write_text(json.dumps(state))
    correction_notes.reload()


def test_notes_go_in_the_prompt(storage):
    _write(storage, {"version": 2, "styles": {"personal": {"notes": [RULE, "Drop 'basically'."]}}})
    section = correction_notes.prompt_section("personal")
    prompt = build_refinement_prompt(RefinementFlags(style="personal"), personal=True, notes=section)
    assert f"- {RULE}" in prompt
    assert correction_notes.prompt_section("other") is None


def test_candidate_notes_replace_the_saved_ones(storage):
    _write(storage, {"version": 2, "styles": {"personal": {"notes": [RULE]}}})
    assert correction_notes.prompt_section("personal", ["Keep it short."]).endswith("\n- Keep it short.")
    assert correction_notes.prompt_section("personal", []) is None


def test_version_1_notes_belong_to_the_migrated_style(storage):
    _write(storage, {"version": 1, "notes": [RULE]})
    assert correction_notes.notes(MIGRATED_STYLE) == [RULE]


def test_the_notes_are_reread_when_the_server_changes_them(storage):
    _write(storage, {"version": 2, "styles": {"personal": {"notes": [RULE]}}})
    assert correction_notes.notes("personal") == [RULE]
    _write(storage, {"version": 2, "styles": {"personal": {"notes": []}}})
    assert correction_notes.notes("personal") == []


def test_notes_about_exclamation_marks_are_not_used(storage):
    exclaim = "Add an exclamation mark to statements that end with a period."
    _write(storage, {"version": 2, "styles": {"personal": {"notes": [RULE, exclaim, "End excited lines with !"]}}})
    assert correction_notes.notes("personal") == [RULE]
