"""A speaker's own "!" and stretched words, learned from their edits; strict until then."""

import json
from datetime import datetime, timedelta

import pytest
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

from backend.database import session as database_session
from backend.database.models import Base, Capture, CaptureFeedback
from backend.services import expression_learning
from backend.services.expression_learning import Learned, gather, learn
from backend.services.prosody import EXCLAIM_ENERGY, StretchRule


def said(*sentences, shapes=()):
    """A capture's saved measurements: its written sentences as (text, energy), and word shapes."""
    return dict(
        written=[dict(text=text, energy=energy) for text, energy in sentences],
        shapes=[list(shape) for shape in shapes],
    )


def capture(name, *sentences, shapes=(), delivered=None):
    measured = said(*sentences, shapes=shapes)
    return name, measured, delivered or " ".join(text for text, _ in sentences)


def plain(count, energy=0.5):
    return [capture(f"plain{index}", ("Just a statement.", energy)) for index in range(count)]


def test_nothing_taught_keeps_the_strict_defaults():
    assert learn(gather(plain(10), {})) == Learned()


def test_one_edit_is_not_enough():
    edits = {"a": "That's great news!"}
    assert learn(gather([capture("a", ("That's great news.", 2.0))], edits)).cutoff == EXCLAIM_ENERGY


def test_periods_changed_to_exclamation_marks_lower_the_cutoff():
    captures = [capture("a", ("That's great news.", 2.0)), capture("b", ("We did it.", 2.2), ("Then home.", 0.1))]
    edits = {"a": "That's great news!", "b": "We did it! Then home."}
    assert learn(gather(captures, edits)).cutoff == pytest.approx(1.75)


def test_never_lower_than_a_few_of_the_speakers_plain_sentences():
    captures = [capture("a", ("That's great news.", 2.0)), capture("b", ("We did it.", 2.2))]
    edits = {"a": "That's great news!", "b": "We did it!"}
    loud_plain = [capture(f"plain{index}", ("Just a statement.", index / 50)) for index in range(100)]
    # 2% of these 100 plain sentences score above 1.94.
    assert learn(gather(captures + loud_plain, edits)).cutoff == pytest.approx(1.94, abs=0.01)


def test_an_exclamation_mark_typed_on_a_calm_sentence_teaches_nothing():
    captures = [capture("a", ("Thanks.", 0.2)), capture("b", ("See you.", 0.4))]
    edits = {"a": "Thanks!", "b": "See you!"}
    assert learn(gather(captures, edits)).cutoff == EXCLAIM_ENERGY


def test_an_exclamation_mark_taken_back_raises_the_cutoff():
    edits = {"a": "That's great news."}
    learned = learn(gather([capture("a", ("That's great news!", 2.7))], edits))
    assert learned.cutoff == pytest.approx(2.95)


def test_words_written_drawn_out_teach_the_speakers_own_stretches():
    captures = [
        capture("a", ("This is way better.", 0.5), shapes=[("is", 1.0, 0.0, 0.1), ("way", 2.2, 0.9, 0.25)]),
        capture("b", ("That was so good.", 0.5), shapes=[("was", 1.0, 0.0, 0.1), ("so", 2.6, 1.2, 0.3)]),
    ]
    edits = {"a": "This is wayyy better.", "b": "That was sooo good."}
    rule = learn(gather(captures, edits)).rule
    assert rule.pace == pytest.approx(1.98)
    assert rule.louder_db == pytest.approx(0.4)
    assert rule.plateau == pytest.approx((0.2, 0.6))
    # "so" is drawn out by this speaker, though not by the default rule.
    assert rule.words == frozenset(["so"])
    assert rule.holds(dict(word="so", stretch=2.5, louder=1.0, plateau=0.3))
    assert not StretchRule().holds(dict(word="so", stretch=2.5, louder=1.0, plateau=0.3))
    # A word said at the speaker's pace still isn't.
    assert not rule.holds(dict(word="way", stretch=1.0, louder=1.0, plateau=0.3))


def test_a_learned_rule_slows_down_until_plain_words_rarely_pass():
    captures = [
        capture("a", ("This is way better.", 0.5), shapes=[("way", 2.2, 0.9, 0.25)]),
        capture("b", ("That was so good.", 0.5), shapes=[("so", 2.6, 1.2, 0.3)]),
    ]
    edits = {"a": "This is wayyy better.", "b": "That was sooo good."}
    # The speaker often says words this slowly without meaning them drawn out.
    plain_words = [
        capture(f"plain{index}", ("Some words.", 0.5), shapes=[("words", 2.1, 1.0, 0.3)]) for index in range(20)
    ]
    rule = learn(gather(captures + plain_words, edits)).rule
    assert rule.pace > 2.1
    assert not rule.holds(dict(word="words", stretch=2.1, louder=1.0, plateau=0.3))


def test_a_stretch_written_plainly_again_is_not_drawn_out_again():
    captures = [capture("a", ("That was really faaar away.", 0.5), shapes=[("far", 7.4, 2.0, 0.4)])]
    edits = {"a": "That was really far away."}
    rule = learn(gather(captures, edits)).rule
    assert not rule.holds(dict(word="far", stretch=7.4, louder=2.0, plateau=0.4))
    assert rule.pace == pytest.approx(7.5)


def test_a_redictation_makes_the_rules_stricter_never_looser():
    captures = [
        capture("a", ("This is way better.", 0.5), shapes=[("way", 2.2, 0.9, 0.25)]),
        capture("b", ("That was so good.", 0.5), shapes=[("so", 2.6, 1.2, 0.3)]),
        capture("c", ("That's great news.", 2.0)),
        capture("d", ("We did it.", 2.2)),
    ]
    edits = {"a": "This is wayyy better.", "b": "That was sooo good.", "c": "That's great news!", "d": "We did it!"}
    assert learn(gather(captures, edits, frozenset(edits))) == Learned()
    stricter = [
        capture("e", ("That was really faaar away.", 0.5), shapes=[("far", 7.4, 2.0, 0.4)]),
        capture("f", ("That's great news!", 2.7)),
    ]
    edits = {"e": "That was really far away.", "f": "That's great news."}
    learned = learn(gather(stricter, edits, frozenset(edits)))
    assert learned.rule.pace == pytest.approx(7.5)
    assert learned.cutoff == pytest.approx(2.95)


@pytest.fixture
def database(monkeypatch):
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    factory = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", factory)
    yield factory
    engine.dispose()


def add_capture(db, capture_id, measured, refined):
    db.add(
        Capture(
            id=capture_id,
            audio_path=f"captures/{capture_id}.wav",
            source="dictation",
            transcript_raw=refined.lower(),
            transcript_refined=refined,
            stt_model="turbo",
            prosody=json.dumps(measured),
        )
    )


def test_learned_from_the_newest_report_of_each_capture(database):
    with database() as db:
        add_capture(db, "a", said(("That's great news.", 2.0)), "That's great news.")
        add_capture(db, "b", said(("We did it.", 2.2)), "We did it.")
        db.flush()
        start = datetime(2026, 10, 5)
        reports = [("a", "That's great news!"), ("b", "We did it?"), ("b", "We did it!")]
        for minute, (capture_id, text) in enumerate(reports):
            db.add(
                CaptureFeedback(
                    capture_id=capture_id,
                    target="refined",
                    expected_text=text,
                    snapshot="{}",
                    created_at=start + timedelta(minutes=minute),
                )
            )
        # A report of the raw transcript isn't about the text written.
        db.add(CaptureFeedback(capture_id="a", target="raw", expected_text="that's great news", snapshot="{}"))
        db.commit()
    assert expression_learning.load().cutoff == pytest.approx(1.75)


def test_a_redictation_report_teaches_only_stricter_rules(database):
    with database() as db:
        add_capture(db, "a", said(("That's great news.", 2.0)), "That's great news.")
        add_capture(db, "b", said(("We did it.", 2.2)), "We did it.")
        add_capture(db, "c", said(("That's great news!", 2.7)), "That's great news!")
        db.flush()
        start = datetime(2026, 10, 5)
        reports = [("a", "That's great news!"), ("b", "We did it!"), ("c", "That's great news.")]
        for minute, (capture_id, text) in enumerate(reports):
            db.add(
                CaptureFeedback(
                    capture_id=capture_id,
                    target="refined",
                    expected_text=text,
                    snapshot="{}",
                    source="redictation",
                    created_at=start + timedelta(minutes=minute),
                )
            )
        db.commit()
    assert expression_learning.load().cutoff == pytest.approx(2.95)


def test_unreadable_measurements_are_skipped(database):
    with database() as db:
        db.add(
            Capture(
                id="bad",
                audio_path="captures/bad.wav",
                source="dictation",
                transcript_raw="x",
                stt_model="turbo",
                prosody="{not json",
            )
        )
        db.commit()
    assert expression_learning.load() == Learned()
