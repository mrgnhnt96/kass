"""What the cleanup adapter learns to write, and spotting macOS stopping a run."""

import pytest

from backend.services.model_improvement import manager
from backend.services.model_improvement.worker import target


@pytest.mark.parametrize(
    ("expected", "output", "learned"),
    [
        # A fix typed without the final period still ends like a sentence.
        ("Commit and merge it to main", "Commit and merge it to me.", "Commit and merge it to main."),
        # The output's own mark, a question here, is the one it gets.
        (
            "Would you like me to refine the wording",
            "Would you like me to refine the wording?",
            "Would you like me to refine the wording?",
        ),
        # The style's habit took the period off the output: still a period.
        ("Make sure the docs are deployed", "Make sure the docks are deployed", "Make sure the docs are deployed."),
        # An end mark the user typed is theirs.
        ("Is the API ready?", "Is the API ready.", "Is the API ready?"),
        ("Ship it!", "Ship it.", "Ship it!"),
        # Short commands, list items and quoted ends stay as written.
        ("/clear", "/clear.", "/clear"),
        ("Commit", "Come here.", "Commit"),
        ("Groceries:\n- Milk\n- Eggs", "Groceries: milk, eggs.", "Groceries:\n- Milk\n- Eggs"),
        ("To do:\n1. Go home\n2. Get the kids", "Go home, then get the kids.", "To do:\n1. Go home\n2. Get the kids"),
        ('Remove "Mac overloaded"', 'Remove, "Mac overloaded."', 'Remove "Mac overloaded"'),
    ],
)
def test_the_adapter_learns_an_end_mark_the_fix_left_off(expected, output, learned):
    assert target({"expected": expected, "output": output}) == learned


def test_a_report_without_the_output_it_corrected_gets_a_period():
    # Rehearsal examples and reports saved before outputs were kept.
    assert target({"expected": "Remind me to call Dad tomorrow"}) == "Remind me to call Dad tomorrow."
    assert target({"expected": "Remind me to call Dad.", "output": None}) == "Remind me to call Dad."


def test_macos_stopping_a_run_for_interactivity_is_told_apart(tmp_path):
    log = tmp_path / "worker.log"
    log.write_text(
        "x" * 100_000 + "\nRuntimeError: [METAL] Command buffer execution failed: Impacting Interactivity "
        "(0000000e:kIOGPUCommandBufferCallbackErrorImpactingInteractivity).\n"
    )
    assert manager._preempted(log)
    log.write_text("ValueError: Not enough model-eligible training/validation reports\n")
    assert not manager._preempted(log)
    assert not manager._preempted(tmp_path / "missing.log")
