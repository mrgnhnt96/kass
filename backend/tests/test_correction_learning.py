"""Learning must earn activation on independent examples and remain reversible."""

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from backend import config
from backend.database.models import Base, Capture
from backend.services import correction_learning as learning, correction_rules as rules
from backend.tests.feedback_rows import file_report


def samples(language=None, sources=("manual", "manual", "manual")):
    return [
        rules.Example(
            str(i),
            str(i),
            f"Please use voice box today at {place}.",
            f"Please use Voicebox today at {place}.",
            language,
            source,
        )
        for i, (place, source) in enumerate(zip(("home", "work", "school"), sources, strict=True))
    ]


@pytest.mark.parametrize("every_report", [False, True])
def test_activation_requires_two_teaching_takes_and_a_third_that_improves(every_report):
    examples = samples()
    assert rules.evaluate(examples[:2], [], every_report=every_report)[0] == []
    selected, metrics = rules.evaluate(examples, [], every_report=every_report)
    assert len(selected) == 1
    assert metrics["examples"] == 3
    assert metrics["accepted"] == 1
    assert rules.evaluate([examples[0]] * 6, [], every_report=every_report)[0] == []
    same_take = [rules.Example(e.id, "same", e.original, e.expected, e.language) for e in examples]
    assert rules.evaluate(same_take, [], every_report=every_report)[0] == []


def test_newest_reports_are_evidence_at_once():
    # Chronological hold-out kept the newest third of reports from ever teaching.
    # That stays the rule without every_report.
    older = [rules.Example(f"u{i}", f"u{i}", f"Old text {i}.", f"Totally new {i}.", None) for i in range(30)]
    assert rules.evaluate([*older, *samples()], [])[0] == []
    selected, metrics = rules.evaluate([*older, *samples()], [], every_report=True)
    assert len(selected) == 1
    assert metrics["accepted"] == 1


@pytest.mark.parametrize(
    ("sources", "learned"),
    [
        (("redictation", "redictation", "redictation"), False),
        (("manual", "redictation", "redictation"), True),
        (("voice_fix", "redictation", "voice_fix"), True),
        (("voice_fix", "voice_fix", "voice_fix"), True),
    ],
)
def test_redictations_only_back_up_an_explicit_report(sources, learned):
    assert bool(rules.evaluate(samples(sources=sources), [], every_report=True)[0]) is learned


def test_active_rule_without_support_is_dropped():
    selected, _ = rules.evaluate(samples(), [], every_report=True)
    kept, metrics = rules.evaluate(samples()[:2], selected, every_report=True)
    assert kept == []
    assert metrics["withdrawn"] == 1
    assert rules.evaluate(samples(), selected, every_report=True)[0] == selected
    # Without every_report, learned rules stay until contradicted or rolled back.
    assert rules.evaluate(samples()[:2], selected)[0] == selected


def test_only_matching_context_and_language_change():
    selected, _ = rules.evaluate(samples("en"), [])
    compiled = rules.compile_rules(selected)
    assert (
        rules.apply_rules("Please use voice box today at lunch.", compiled, "en")
        == "Please use Voicebox today at lunch."
    )
    for text in (
        "The voice box is part of the larynx.",
        "Please use voice box tomorrow.",
        "Please reuse voice box today.",
        "Please use voice box todayish.",
    ):
        assert rules.apply_rules(text, compiled, "en") == text
    assert rules.apply_rules(samples()[0].original, compiled, "fr") == samples()[0].original


def test_heldout_contradiction_and_known_good_block_activation(monkeypatch):
    examples = samples()
    unchanged = rules.Example(
        "4", "4", "Please use voice box today outside.", "Please use voice box today outside.", None
    )
    assert rules.evaluate([*examples, unchanged], [])[0] == []
    monkeypatch.setattr(rules, "KNOWN_GOOD", (examples[0].original,))
    assert rules.evaluate(examples, [])[0] == []


def test_latency_gate_blocks_activation(monkeypatch):
    ticks = iter(i * 0.01 for i in range(100))
    monkeypatch.setattr(rules.time, "perf_counter", lambda: next(ticks))
    selected, metrics = rules.evaluate(samples(), [])
    assert selected == []
    assert not metrics["latency_passed"]


@pytest.mark.parametrize(
    "expected", ["", "Completely different wording.", "Please use voice box today at home. Extra instructions."]
)
def test_deletions_and_broad_rewrites_are_not_learned(expected):
    assert rules.candidate(rules.Example("1", "1", samples()[0].original, expected, None)) is None


def merges(*originals):
    return [rules.Example(str(i), str(i), original, expected, "en") for i, (original, expected) in enumerate(originals)]


def test_an_edit_at_the_end_of_a_take_is_anchored_to_the_end():
    rule = rules.candidate(merges(("Commit, PR, merge, domain.", "Commit, PR, merge to main."))[0])
    assert (rule["source"], rule["replacement"], rule["edges"]) == (["merge", "domain"], ["to", "main"], [False, True])
    compiled = rules.compile_rules([rule])
    assert rules.apply_rules("Commit and merge, domain", compiled, "en") == "Commit and merge to main"
    assert rules.apply_rules("Okay merge domain.", compiled, "en") == "Okay merge to main."
    for text in ("Merge domain names first.", "We merge domain logic, then ship.", "Commit, merge, domains"):
        assert rules.apply_rules(text, compiled, "en") == text


def test_an_edit_at_the_start_or_the_whole_take_is_anchored_to_the_start():
    rule = rules.candidate(merges(("The market is done", "Mark it is done"))[0])
    assert (rule["source"], rule["edges"]) == (["The", "market", "is"], [True, False])
    compiled = rules.compile_rules([rule])
    assert rules.apply_rules("The market is done", compiled, "en") == "Mark it is done"
    assert rules.apply_rules("So the market is done", compiled, "en") == "So the market is done"
    whole = rules.compile_rules([rules.candidate(merges(("Clod.", "Claude."))[0])])
    assert rules.apply_rules("Clod.", whole, "en") == "Claude."
    assert rules.apply_rules("Clod hopper.", whole, "en") == "Clod hopper."


@pytest.mark.parametrize(
    ("original", "expected"),
    [
        ("Commit, merge, domain", "Commit, merge to bananas"),
        ("Commit, merge to me", "Commit, merge to main"),
        ("Run it in src/domain", "Run it in src/to main"),
        ('Commit, merge, domain "', 'Commit, merge to main "'),
    ],
)
def test_one_sided_edits_still_need_a_similar_spelling_and_plain_words(original, expected):
    assert rules.candidate(merges((original, expected))[0]) is None


@pytest.mark.parametrize("every_report", [False, True])
def test_a_phrase_misheard_another_way_confirms_a_repeated_fix(every_report):
    repeated = [
        ("Commit, merge, domain", "Commit, merge to main"),
        ("Commit, PR, merge, domain.", "Commit, PR, merge to main."),
    ]
    assert rules.evaluate(merges(*repeated), [], every_report=every_report)[0] == []
    # Whisper heard "merge to main" another way here; the report still wrote it.
    misheard = ("Commit, PR, merged, been", "Commit, PR, merge to main")
    selected, metrics = rules.evaluate(merges(*repeated, misheard), [], every_report=every_report)
    assert [(r["source"], r["replacement"]) for r in selected] == [(["merge", "domain"], ["to", "main"])]
    assert metrics["accepted"] == 1
    # The phrase written mid-take is not the end-of-take phrase the rule writes.
    elsewhere = ("Commit, merged, been then deploy", "Commit, merge to main then deploy")
    assert rules.evaluate(merges(*repeated, elsewhere), [], every_report=every_report)[0] == []


def test_rules_learned_before_edges_keep_matching_as_they_did():
    legacy = {"id": "old", "source": ["use", "voice", "box", "today"], "replacement": ["Voicebox"], "language": None}
    compiled = rules.compile_rules([legacy])
    assert rules.apply_rules("Please use voice box today.", compiled) == "Please use Voicebox today."
    assert rules.apply_rules("Please use, voice box today.", compiled) == "Please use, voice box today."


def test_a_rule_learned_before_edges_with_only_its_anchors_changes_nothing():
    legacy = {"id": "old", "source": ["voice", "box"], "replacement": ["Voicebox"], "language": None}
    compiled = rules.compile_rules([legacy])
    assert rules.apply_rules("Please use voice box today.", compiled) == "Please use voice box today."


@pytest.fixture
def storage(tmp_path, monkeypatch):
    engine = create_engine(f"sqlite:///{tmp_path}/test.db")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    monkeypatch.setattr(config, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(learning, "_state", None)
    monkeypatch.setattr(learning, "_path", None)
    monkeypatch.setattr(learning, "_compiled", ())
    with factory() as db:
        for example in samples():
            db.add(
                Capture(
                    id=example.id,
                    audio_path="unused.wav",
                    source="dictation",
                    transcript_raw=example.original,
                    transcript_refined=example.original,
                )
            )
            db.commit()
            file_report(db, example.id, example.expected)
    yield factory
    engine.dispose()


def learn(examples=None):
    """Save rules learned from ``examples`` as the server does, and reload them."""
    import json

    selected, metrics = rules.evaluate(examples or samples(), [], every_report=True)
    state = {**learning._empty(), "revision": 1, "rules": selected, "metrics": metrics, "outcome": "updated"}
    (config.get_data_dir() / "correction-learning.json").write_text(json.dumps(state))
    return learning.reload()


def test_reload_reads_the_servers_rules(storage):
    learning.initialize()
    assert learning.apply_learned_corrections(samples()[0].original) == samples()[0].original
    status = learn()
    assert status["active_rules"] == 1
    assert status["revision"] == 1
    assert learning.apply_learned_corrections(samples()[0].original) == samples()[0].expected


def test_corrupt_state_falls_back_to_no_rules(storage):
    (config.get_data_dir() / "correction-learning.json").write_text("{broken")
    assert learning.status()["active_rules"] == 0
    assert learning.reload()["active_rules"] == 0


def test_numeric_changes_and_long_live_inputs_are_not_applied(storage):
    assert rules.candidate(rules.Example("1", "1", "Meet at 15 today.", "Meet at 50 today.", None)) is None
    learn()
    text = samples()[0].original * 200
    assert learning.apply_learned_corrections(text) == text
