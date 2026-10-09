"""Voice edits: fixing Kass's last take by saying so (docs/plans/VOICE_EDITS.md).

The spoken forms are what Whisper wrote for them (TTS runs, 2026-09-30).
"""

import asyncio
import struct
from unittest.mock import AsyncMock

import numpy as np
import pytest
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import Session

from backend.database.models import Base
from backend.services import capture_stream, correction_learning
from backend.services.dictionary import Dictionary
from backend.services.spelling import join_spelling
from backend.services.voice_edits import NOTHING_TO_FIX, Declined, Planned, parse, plan, starts_edit
from backend.tests.test_capture_stream import append, make_session, socket_app

TAKE = "Hi Megan, can we move the meeting to Tuesday? I actually think 3 p.m. works."


def after(said: str, take: str = TAKE) -> str:
    planned = plan(said, take)
    assert isinstance(planned, Planned), planned
    assert planned.before == take
    return planned.after


@pytest.mark.parametrize(
    "said",
    [
        "Fix that, Morgan not Megan.",
        "Fix that. Morgan not Megan.",
        "fix that morgan not megan",
        "Fix that, Morgan, not Megan!",
        "Fix that, it's Morgan not Megan.",
        "Fix that, not Megan, Morgan.",
        "Fix that, Morgan instead of Megan.",
        "Edit, change Megan to Morgan.",
        "Edit. Replace Megan with Morgan.",
        "Kass, change Megan to Morgan.",
        "Kass, fix that, Morgan not Megan.",
        "Cass, change Megan to Morgan.",
        "Kas, fix that, Morgan not Megan.",
        "Um, fix that, Morgan not Megan.",
    ],
)
def test_a_word_is_replaced_however_whisper_wrote_the_edit(said):
    assert after(said) == TAKE.replace("Megan", "Morgan")


def test_the_word_takes_the_case_of_where_it_goes():
    assert after("Fix that. Thursday not Tuesday.") == TAKE.replace("Tuesday", "Thursday")
    take = "we could do it tomorrow."
    # Capitalized only because Whisper started a sentence with it.
    assert after("Fix that. Today, not tomorrow.", take) == "we could do it today."
    assert after("Edit, change we to They.", take) == "They could do it tomorrow."


def test_change_x_to_y_keeps_the_order_cleanup_swapped():
    assert after("Edit, change Tuesday to Thursday.") == TAKE.replace("Tuesday", "Thursday")


@pytest.mark.parametrize(
    "said", ["Fix that! Delete actually!", "fix that delete actually", "Fix that, remove the word actually."]
)
def test_a_word_is_deleted_with_its_space(said):
    assert after(said) == TAKE.replace("actually ", "")


def test_deleting_the_first_word_of_a_sentence_capitalizes_the_next():
    assert after("Fix that, delete actually.", "Actually, I think so.") == "I think so."
    assert after("Fix that, delete really.", "It works really.") == "It works."


@pytest.mark.parametrize("said", ["fix that add tomorrow after meeting", "Fix that, at tomorrow after meeting."])
def test_words_are_added_after_a_word(said):
    assert after(said) == TAKE.replace("meeting", "meeting tomorrow")


def test_words_are_added_before_a_word():
    assert after("Fix that, add next before Tuesday.") == TAKE.replace("Tuesday", "next Tuesday")


@pytest.mark.parametrize("said", ["Fix that, 4pm not 3pm.", "Fix that, 4 p.m. not 3 p.m.", "Edit, change 3pm to 4pm."])
def test_times_match_however_they_are_written(said):
    changed = after(said)
    assert "3 p.m" not in changed
    assert "4pm" in changed or "4 p.m" in changed


def test_a_spelled_word_replaces_the_word_it_is_close_to():
    # "Meghan" said, then spelled: Whisper hears "Megan", spelling joins the letters.
    said = join_spelling("Fix that, Meghan, M-E-G-H-A-N.")
    assert said == "Fix that, Meghan, MEGHAN."
    assert after(said) == TAKE.replace("Megan", "Meghan")
    assert after("Fix that, Megan, MEGHAN.") == TAKE.replace("Megan", "Meghan")
    assert after("Fix that, it's spelled MEGHAN.") == TAKE.replace("Megan", "Meghan")
    planned = plan("Fix that, Megan, MEGHAN.", TAKE)
    assert (planned.spelled, planned.replaced) == ("MEGHAN", "Megan")


def test_spelled_letters_take_the_case_of_the_word_they_replace():
    assert after("Fix that, T-H-E-I-R.".replace("T-H-E-I-R", "THEIR"), "I saw there car.") == "I saw their car."
    assert after("Fix that, JON.", "Say hi to John for me.") == "Say hi to Jon for me."
    assert after("Edit, change Megan to MEGHAN.") == TAKE.replace("Megan", "Meghan")


def test_a_word_said_then_spelled_is_one_word_on_either_side_of_an_edit():
    # "Kass, I meant Vova, V-O-V-A, not Volva, V-U-L-V-A", as Whisper heard it.
    take = "I added vulva to the dictionary."
    fixed = "I added vova to the dictionary."
    assert after("Kass, I meant VOVA, VOVA, not VOLVA, VULVA.", take) == fixed
    assert after("Edit, change Volva, VULVA to Vova, VOVA.", take) == fixed


def test_a_short_spelled_word_reaches_a_word_two_sounds_off():
    # Four letters spelled for five heard: off by two in five.
    planned = plan("Kass, V-O-V-A.".replace("V-O-V-A", "VOVA"), "I added vulva to the dictionary.")
    assert (planned.after, planned.spelled) == ("I added vova to the dictionary.", "VOVA")
    # Two letters Whisper left dashed are spelled too.
    assert after("Fix that, B-O.", "Tell Boe hi.") == "Tell Bo hi."


def test_names_that_sound_alike_are_declined_rather_than_changed_to_themselves():
    # "replace Megan with Meghan", as Whisper hears it.
    declined = plan("Fix that, replace Megan with Megan.", TAKE)
    assert isinstance(declined, Declined)
    assert "spelling" in declined.message
    assert isinstance(plan("Fix that, John not John.", "Hi John"), Declined)


def test_no_takes_the_side_the_text_has_as_the_wrong_one():
    assert after("Fix that, Megan, no, Morgan.") == TAKE.replace("Megan", "Morgan")
    assert after("Fix that, Morgan, no, Megan.") == TAKE.replace("Megan", "Morgan")
    assert isinstance(plan("Fix that, Megan no Tuesday.", TAKE), Declined)


@pytest.mark.parametrize(
    "said",
    [
        "I want the red one, not the blue one.",
        "Change the meeting to Thursday.",
        "Delete the draft and start over.",
        "Edit the document before Friday.",
        "Fix that bug before the release.",
        "Morgan not Megan.",
        "Kass is not the app I meant.",
        "Cassie said she would call back.",
        "Replace Megan with Morgan.",
        "Fix the login bug before Friday.",
        "Fix Morgan not Megan.",
        "Fix it.",
    ],
)
def test_ordinary_dictation_is_not_an_edit(said):
    assert parse(said) is None
    assert plan(said, TAKE) is None
    assert not starts_edit(said)


def test_a_marked_trigger_holds_the_take_for_an_edit_that_may_follow():
    assert starts_edit("Fix that.")
    assert starts_edit("Edit,")
    assert starts_edit("fix that add tomorrow after meeting")
    assert not starts_edit("Edit the file.")
    # Nothing after it but a trigger: say what.
    assert isinstance(plan("Fix that.", TAKE), Declined)
    # A sentence after it that isn't an edit is dictation after all.
    assert plan("Fix that, the parser is fine.", TAKE) is None


def test_a_negated_verb_is_a_sentence_not_a_correction():
    assert parse("Fix that, the bug is not in the parser.") is None
    assert parse("Fix that, it doesn't not work.") is None


@pytest.mark.parametrize(
    "said",
    [
        "Fix that, it's Thursday.",
        "Fix, it's Thursday.",
        "fix it's Thursday",
        "Fix it should be Thursday.",
        "Kass, Thursday.",
        "Edit. It was Thursday.",
    ],
)
def test_a_word_said_alone_replaces_the_word_of_its_kind(said):
    assert after(said) == TAKE.replace("Tuesday", "Thursday")


@pytest.mark.parametrize("said", ["Kass, I mean Vova, not Volva.", "Cas, I meant Vova, not vulva."])
def test_i_mean_before_the_right_word_is_not_written(said):
    take = "Met with vulva about goals."
    assert after(said, take) == "Met with Vova about goals."


def test_kinds_of_word_are_days_months_and_numbers():
    take = "See you Monday in March. We may start at 2:30 p.m. or 4pm."
    assert after("Fix, it's tomorrow.", take) == take.replace("Monday", "tomorrow")
    assert after("Fix, it's April.", take) == take.replace("March", "April")
    # The verb "may" isn't a month.
    assert after("Fix, it's May.", take) == take.replace("March", "May")
    # Nearest the caret, keeping its half of the day.
    assert after("Fix, it's 3:30.", take) == take.replace("4pm", "3:30pm")
    assert after("Fix, it's 5 a.m.", take) == take.replace("4pm", "5 a.m")


def test_a_name_said_alone_replaces_the_one_that_sounds_like_it():
    assert after("Fix that, Morgan.") == TAKE.replace("Megan", "Morgan")
    declined = plan("Fix, Morgan.", "Megan and Meagan came.")
    assert isinstance(declined, Declined)
    assert "More than one" in declined.message


def test_a_word_said_alone_with_nothing_like_it_is_declined():
    assert plan("Fix, it's Thursday.", "No days here.") == Declined("No day in the text before the cursor")
    assert plan("Fix, Sarah.", TAKE) == Declined("Nothing like “Sarah” in the text before the cursor")
    assert plan("Fix, Tuesday.", TAKE) == Declined("“Tuesday” is already written that way")


def test_the_nearest_match_to_the_caret_wins():
    take = "Tuesday works, or Tuesday next week."
    assert after("Fix that, Thursday not Tuesday.", take) == "Tuesday works, or Thursday next week."


def test_an_exact_match_wins_over_a_close_spelling():
    take = "Megan and Megann came."
    assert after("Fix that, Morgan not Megann.", take) == "Megan and Morgan came."


def test_a_close_spelling_matches_long_words_only():
    assert after("Fix that, Thursday not Tusday.", "See you Tuesday.") == "See you Thursday."
    assert isinstance(plan("Fix that, Dan not Don.", "Hi Dun."), Declined)


def test_matches_are_word_aligned():
    assert isinstance(plan("Fix that, delete act.", "It was an actual act."), Planned)
    assert after("Fix that, delete act.", "It was an actual act.") == "It was an actual."
    assert isinstance(plan("Fix that, delete act.", "It was actual."), Declined)


def test_nothing_to_fix_without_a_last_take():
    for take in (None, "", "  "):
        declined = plan("Fix that, Morgan not Megan.", take)
        assert isinstance(declined, Declined)


def test_a_word_not_in_the_take_is_declined():
    declined = plan("Fix that, Morgan not Sarah.", TAKE)
    assert isinstance(declined, Declined)
    assert "Sarah" in declined.message


def test_the_edit_is_described_for_captures():
    assert plan("Fix that, Morgan not Megan.", TAKE).instruction == "“Megan” → “Morgan”"
    assert plan("Fix that, delete actually.", TAKE).instruction == "Delete “actually”"
    assert plan("fix that add tomorrow after meeting", TAKE).instruction == "Add “tomorrow” after “meeting”"


# -- in a streaming take ---------------------------------------------------------


async def speak_edit(tmp_path, monkeypatch, said, last_take=TAKE, **settings):
    """A streamed take of ``said`` after Kass typed ``last_take``, cleaned up by a fake."""
    session, events = make_session(tmp_path, monkeypatch, **settings)
    session.settings.auto_refine = True
    refine = AsyncMock(return_value=("Cleaned.", "0.6B"))
    monkeypatch.setattr(capture_stream, "refine_transcript", refine)
    monkeypatch.setattr(capture_stream, "known_names", lambda: frozenset())
    monkeypatch.setattr(correction_learning, "apply_learned_corrections", lambda text, _: text)
    session.recognize = AsyncMock(return_value=said)
    if last_take is not None:
        session.set_last_take(last_take, "capture-1")
    worker = asyncio.create_task(session.run())
    append(session, 1)
    session.finish()
    await worker
    return session, events, refine


def persisted(session):
    engine = create_engine("sqlite://")
    Base.metadata.create_all(engine)
    with Session(engine) as db:
        return session.persist(db)


@pytest.mark.asyncio
async def test_an_edit_take_is_never_cleaned_up_and_says_what_changes(tmp_path, monkeypatch):
    session, events, refine = await speak_edit(tmp_path, monkeypatch, "Fix that, Morgan not Megan.")
    refine.assert_not_awaited()
    assert session.edit_result() == dict(before=TAKE, after=TAKE.replace("Megan", "Morgan"))
    assert not [e for e in events if e["type"] in {"refined", "provisional"}]
    session.close()


@pytest.mark.asyncio
async def test_an_edit_is_saved_as_a_command_on_the_take_it_changed(tmp_path, monkeypatch):
    session, _, _ = await speak_edit(tmp_path, monkeypatch, "Fix that, Morgan not Megan.")
    capture = persisted(session)
    assert capture.source == "command"
    assert capture.command_transform == "Voice edit"
    assert capture.command_selection == TAKE
    assert capture.command_instruction == "“Megan” → “Morgan”"
    assert capture.transcript_refined == TAKE.replace("Megan", "Morgan")
    assert capture.transcript_raw == "Fix that, Morgan not Megan."
    assert capture.allow_auto_paste


@pytest.mark.asyncio
async def test_a_declined_edit_is_saved_without_a_result(tmp_path, monkeypatch):
    session, _, refine = await speak_edit(tmp_path, monkeypatch, "Fix that, Morgan not Megan.", last_take=None)
    refine.assert_not_awaited()
    assert session.edit_result() == dict(declined=NOTHING_TO_FIX)
    capture = persisted(session)
    assert (capture.source, capture.transcript_refined) == ("command", None)
    assert capture.command_instruction == NOTHING_TO_FIX


@pytest.mark.asyncio
async def test_a_take_that_opens_like_an_edit_but_is_not_one_is_cleaned_up_whole(tmp_path, monkeypatch):
    session, _, refine = await speak_edit(tmp_path, monkeypatch, "Fix that, the parser is fine.")
    assert refine.await_args.args[0] == "Fix that, the parser is fine."
    assert session.edit_result() is None
    assert session.refined
    session.close()


@pytest.mark.asyncio
async def test_with_voice_edits_off_an_edit_is_dictated(tmp_path, monkeypatch):
    session, _, refine = await speak_edit(tmp_path, monkeypatch, "Fix that, Morgan not Megan.", voice_edits=False)
    refine.assert_awaited()
    assert session.edit_result() is None
    assert "Kass" not in session.vocabulary
    session.close()


def test_kass_is_prompted_to_whisper_while_voice_edits_are_on(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.dictionary = Dictionary(terms=("Kubernetes", "Kass"))
    assert session.vocabulary == ("Kass", "Kubernetes")
    session.close()


@pytest.mark.asyncio
async def test_the_client_hears_of_an_edit_while_the_user_still_speaks(tmp_path, monkeypatch):
    session, events = make_session(tmp_path, monkeypatch)
    session.recognize = AsyncMock(return_value="Fix that,")
    session.set_last_take(TAKE)
    append(session, 3)
    assert session.style_peek_due()
    await session.peek_style()
    assert [e["type"] for e in events] == ["edit"]
    assert not session.style_peek_due()
    # The phrase confirms it without a second announcement.
    await session.accept("Fix that, Morgan not Megan.")
    assert [e["type"] for e in events].count("edit") == 1
    assert session.editing
    session.close()


def test_the_last_take_must_be_short_text(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    for bad in (None, 7, "x" * 1001):
        with pytest.raises(ValueError, match="last take"):
            session.set_last_take(bad)
    with pytest.raises(ValueError, match="capture id"):
        session.set_last_take("hi", 3)
    session.close()


def test_kass_s_part_of_the_text_defaults_to_all_of_a_take_with_a_capture(tmp_path, monkeypatch):
    session, _ = make_session(tmp_path, monkeypatch)
    session.set_last_take("Hi Megan.", "c1")
    assert session.last_take_own_chars == 9
    session.set_last_take("I typed this.")
    assert session.last_take_own_chars == 0
    session.set_last_take("I typed this. Hi Megan.", "c1", 9)
    assert session.last_take_own_chars == 9
    for bad in (-1, 24, "9", True):
        with pytest.raises(ValueError, match="own_chars"):
            session.set_last_take("I typed this. Hi Megan.", "c1", bad)
    session.close()


def test_only_a_fix_in_kass_s_part_is_its_own():
    from backend.services.voice_edits import changes_end

    text = "I typed Megan here. Hi Megan."
    own = len("Hi Megan.")
    in_kass_part = Planned(text, "I typed Megan here. Hi Morgan.", "", "Megan")
    in_user_part = Planned(text, "I typed Morgan here. Hi Megan.", "", "Megan")
    assert changes_end(in_kass_part, own)
    assert not changes_end(in_user_part, own)
    assert not changes_end(in_kass_part, 0)
    # A change that starts mid-word counts from the word.
    assert not changes_end(Planned("Hi Megan.", "Hi Megane.", "", "Megan"), 3)


@pytest.mark.asyncio
async def test_text_kass_did_not_write_is_fixed_but_not_reported(tmp_path, monkeypatch):
    learned = []
    monkeypatch.setattr(capture_stream.voice_edits, "learn_from", lambda *args: learned.append(args))
    session, _ = make_session(tmp_path, monkeypatch)
    session.settings.auto_refine = True
    monkeypatch.setattr(capture_stream, "known_names", lambda: frozenset())
    session.recognize = AsyncMock(return_value="Fix that, Morgan not Megan.")
    # The user typed "Megan"; Kass wrote only "See you soon."
    session.set_last_take("Thanks Megan. See you soon.", "capture-1", len("See you soon."))
    worker = asyncio.create_task(session.run())
    append(session, 1)
    session.finish()
    await worker
    assert session.edit_result() == dict(before="Thanks Megan. See you soon.", after="Thanks Morgan. See you soon.")
    session.learn_from_edit()
    (args,) = learned
    assert args[1] is None
    session.close()


def test_the_final_event_carries_the_edit(tmp_path, monkeypatch):
    event = streamed_final(tmp_path, monkeypatch)
    assert event["edit"] == dict(before=TAKE, after=TAKE.replace("Megan", "Morgan"))
    assert event["capture"]["source"] == "command"


def streamed_final(tmp_path, monkeypatch):
    """The final event of "Fix that, Morgan not Megan." streamed after TAKE."""
    app, _ = socket_app(tmp_path, monkeypatch)
    said = AsyncMock(return_value="Fix that, Morgan not Megan.")
    monkeypatch.setattr(capture_stream.StreamingCapture, "recognize", said)
    start = dict(type="start", protocol_version=1, sample_rate=16000, channels=1, encoding="pcm_s16le")
    with TestClient(app) as client, client.websocket_connect("/captures/stream") as socket:
        socket.send_json(start)
        socket.receive_json()
        socket.send_json(dict(type="last_take", text=TAKE, capture_id="capture-1"))
        socket.send_bytes(struct.pack("<II", 0, 0) + np.ones(1600, dtype="<i2").tobytes())
        socket.send_json(dict(type="finish"))
        while (event := socket.receive_json())["type"] != "final":
            pass
    return event


# -- what a fix teaches --------------------------------------------------------


def test_the_fix_is_found_in_the_capture_despite_edge_differences():
    from backend.services.voice_edits import corrected

    # The field dropped the final period and has a leading space.
    assert (
        corrected("Thanks Megan for the notes.", " Thanks Megan for the notes", " Thanks Morgan for the notes")
        == "Thanks Morgan for the notes."
    )
    assert corrected("Hi Megan, and Megan again.", "Hi Megan, and Megan again", "Hi Megan, and Morgan again") == (
        "Hi Megan, and Morgan again."
    )
    # Nothing like it in the capture: no report.
    assert corrected("Something else entirely.", "Thanks Megan", "Thanks Morgan") is None


@pytest.fixture
def learning_db(tmp_path, monkeypatch):
    from sqlalchemy.orm import sessionmaker

    from backend import config
    from backend.database import session as database_session
    from backend.database.models import Capture
    from backend.services import dictionary

    monkeypatch.setattr(config, "_data_dir", tmp_path)
    engine = create_engine(f"sqlite:///{tmp_path / 'db.sqlite'}")
    Base.metadata.create_all(engine)
    monkeypatch.setattr(database_session, "SessionLocal", sessionmaker(bind=engine))
    with Session(engine) as db:
        db.add(
            Capture(
                id="take",
                audio_path="captures/take.wav",
                source="dictation",
                transcript_raw="thanks megan for the notes",
                transcript_refined="Thanks Megan for the notes.",
                stt_model="turbo",
                llm_model="0.6B",
            )
        )
        db.commit()
    dictionary.invalidate()
    yield engine
    dictionary.invalidate()
    engine.dispose()


def test_a_fix_files_a_voice_fix_report_on_the_take_it_fixed(learning_db):
    from backend.database.models import CaptureFeedback
    from backend.services.voice_edits import learn_from

    learn_from(
        Planned(
            before="Thanks Megan for the notes",
            after="Thanks Morgan for the notes",
            instruction="“Megan” → “Morgan”",
            replaced="Megan",
        ),
        "take",
        "com.apple.TextEdit",
    )
    with Session(learning_db) as db:
        (report,) = db.query(CaptureFeedback).all()
        assert (report.capture_id, report.target, report.source) == ("take", "refined", "voice_fix")
        assert report.expected_text == "Thanks Morgan for the notes."


def test_fixes_one_after_another_stack_on_the_take(learning_db):
    from backend.database.models import CaptureFeedback
    from backend.services.voice_edits import learn_from

    learn_from(
        Planned(
            before="Thanks Megan for the notes", after="Thanks Morgan for the notes", instruction="", replaced="Megan"
        ),
        "take",
        "com.apple.TextEdit",
    )
    learn_from(
        Planned(
            before="Thanks Morgan for the notes", after="Thanks Morgan for the slides", instruction="", replaced="notes"
        ),
        "take",
        "com.apple.TextEdit",
    )
    with Session(learning_db) as db:
        reports = db.query(CaptureFeedback).order_by(CaptureFeedback.created_at).all()
        assert [r.expected_text for r in reports] == [
            "Thanks Morgan for the notes.",
            "Thanks Morgan for the slides.",
        ]


def test_a_spelled_fix_also_adds_the_word_spelling_only(learning_db):
    from backend.database.models import CaptureFeedback
    from backend.services import dictionary
    from backend.services.voice_edits import learn_from

    learn_from(
        Planned(
            before="Thanks Megan for the notes",
            after="Thanks Meghan for the notes",
            instruction="“Megan” → “Meghan”",
            replaced="Megan",
            spelled="MEGHAN",
        ),
        "take",
        "com.apple.TextEdit",
    )
    with Session(learning_db) as db:
        assert db.query(CaptureFeedback).one().expected_text == "Thanks Meghan for the notes."
    found = dictionary.for_app("com.apple.TextEdit")
    assert found.terms == ("Meghan",)
    assert found.apply("Megan met Meghan") == "Megan met Meghan"


def test_without_the_take_nothing_is_reported(learning_db):
    from backend.database.models import CaptureFeedback
    from backend.services.voice_edits import learn_from

    learn_from(Planned(before="a", after="b", instruction="", replaced="a"), None, None)
    with Session(learning_db) as db:
        assert db.query(CaptureFeedback).count() == 0


# -- deleting a voice edit -----------------------------------------------------


def _edit_capture(engine, capture_id="edit"):
    from backend.database.models import Capture

    with Session(engine) as db:
        db.add(
            Capture(
                id=capture_id,
                audio_path=f"captures/{capture_id}.wav",
                source="command",
                transcript_raw="Kass, it's Morgan, not Megan.",
                command_transform="Voice edit",
            )
        )
        db.commit()


def test_deleting_a_voice_edit_withdraws_the_fix_it_filed(learning_db, monkeypatch):
    from backend.database.models import CaptureFeedback
    from backend.services import captures
    from backend.services.voice_edits import learn_from

    relearned = []
    monkeypatch.setattr(correction_learning, "request_run", lambda retrain=False: relearned.append(retrain))
    _edit_capture(learning_db)
    learn_from(
        Planned(
            before="Thanks Megan for the notes",
            after="Thanks Morgan for the notes",
            instruction="“Megan” → “Morgan”",
            replaced="Megan",
        ),
        "take",
        "com.apple.TextEdit",
        "edit",
    )
    with Session(learning_db) as db:
        assert db.query(CaptureFeedback).one().filed_by == "edit"
        relearned.clear()
        assert captures.delete_capture("edit", db)
        assert db.query(CaptureFeedback).count() == 0
        # The take it fixed stays.
        assert captures.get_capture("take", db) is not None
    assert relearned == [True]


def test_deleting_a_voice_edit_removes_the_word_it_spelled(learning_db):
    from backend.services import captures, dictionary
    from backend.services.voice_edits import learn_from

    _edit_capture(learning_db)
    learn_from(
        Planned(
            before="Thanks Megan for the notes",
            after="Thanks Meghan for the notes",
            instruction="“Megan” → “Meghan”",
            replaced="Megan",
            spelled="MEGHAN",
        ),
        "take",
        "com.apple.TextEdit",
        "edit",
    )
    assert dictionary.for_app("com.apple.TextEdit").terms == ("Meghan",)
    with Session(learning_db) as db:
        captures.delete_capture("edit", db)
    assert dictionary.for_app("com.apple.TextEdit").terms == ()


def test_a_spelled_word_the_user_edited_outlives_the_voice_edit(learning_db):
    from backend.services import captures, dictionary
    from backend.services.voice_edits import learn_from

    _edit_capture(learning_db)
    learn_from(
        Planned(before="Hi Megan", after="Hi Meghan", instruction="", replaced="Megan", spelled="MEGHAN"),
        None,
        "com.apple.TextEdit",
        "edit",
    )
    with Session(learning_db) as db:
        (group,) = dictionary.list_groups(db)
        dictionary.update_group(db, group.id, {"match_sound": True})
        captures.delete_capture("edit", db)
    assert dictionary.for_app("com.apple.TextEdit").terms == ("Meghan",)


def test_deleting_another_capture_leaves_a_voice_edit_s_fix(learning_db):
    from backend.database.models import CaptureFeedback
    from backend.services import captures
    from backend.services.voice_edits import learn_from

    _edit_capture(learning_db)
    _edit_capture(learning_db, "other")
    learn_from(
        Planned(
            before="Thanks Megan for the notes",
            after="Thanks Morgan for the notes",
            instruction="",
            replaced="Megan",
        ),
        "take",
        None,
        "edit",
    )
    with Session(learning_db) as db:
        captures.delete_capture("other", db)
        assert db.query(CaptureFeedback).count() == 1
