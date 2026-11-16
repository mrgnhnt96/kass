"""Per-app writing styles: migration, assignment, suggestions, and what each style learns."""

import json

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import get_db, session as database_session
from backend.database.models import AppStyle, Base, Capture, CaptureFeedback, CaptureSettings, WritingStyle
from backend.models import CaptureSettingsResponse
from backend.services import correction_notes, personal_examples, refinement, styles, writing_style
from backend.services.refinement import RefinementFlags, build_refinement_prompt, style_first_word

SLACK, MAIL = "com.tinyspeck.slackmacgap", "com.apple.mail"


@pytest.fixture
def storage(tmp_path, monkeypatch):
    monkeypatch.setattr(config, "_data_dir", tmp_path)
    monkeypatch.setattr(writing_style, "_state", None)
    monkeypatch.setattr(correction_notes, "_state", None)
    monkeypatch.setattr(personal_examples, "_cache", None)
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", make)
    return make


def seed(storage, **saved):
    with storage() as db:
        if saved:
            db.add(CaptureSettings(id=1, **saved))
            db.commit()
        styles.ensure_styles(db)


def create(storage, name, **settings):
    with storage() as db:
        style = styles.create_style(db, name)
        if settings:
            styles.update_style(db, style.id, settings)
        return style.id


def assign(storage, app, style_id, corrections="bring"):
    with storage() as db:
        styles.assign_app(db, app, None, style_id, corrections)


def correct(storage, capture_id, app, said, meant, refined=None):
    with storage() as db:
        db.add(Capture(id=capture_id, audio_path="a.wav", transcript_raw=said, app_bundle_id=app))
        db.add(
            CaptureFeedback(
                capture_id=capture_id,
                target="refined",
                expected_text=meant,
                snapshot=json.dumps(
                    {"transcript_raw": said, "transcript_refined": refined or said, "app_bundle_id": app}
                ),
            )
        )
        db.commit()
    personal_examples.invalidate()


def said(style_id):
    return {e["said"] for e in personal_examples.all_examples(style_id)}


def test_one_personal_style_takes_the_global_settings(storage):
    seed(storage, preserve_technical=False, punctuation_style="learned")
    snapshot = styles.snapshot()
    assert [s.name for s in snapshot.styles] == ["Personal"]
    personal = snapshot.default
    assert personal.id == "personal"
    # Dictation keeps working exactly as before until an app is assigned.
    assert personal.punctuation_style == "learned"
    assert not personal.preserve_technical
    assert snapshot.for_app(SLACK) == personal
    # Seeding again changes nothing.
    seed(storage)
    assert styles.snapshot().styles == snapshot.styles


def test_a_fresh_install_starts_with_personal_only(storage):
    seed(storage)
    (personal,) = styles.snapshot().styles
    assert (personal.name, personal.is_default, personal.punctuation_style) == ("Personal", True, "standard")


def test_at_most_six_styles(storage):
    seed(storage)
    for index in range(styles.MAX_STYLES - 1):
        create(storage, f"Style {index}")
    with storage() as db, pytest.raises(ValueError, match="at most 6"):
        styles.create_style(db, "One too many")


def test_assigning_an_app_confirms_it_and_picks_its_flags(storage):
    seed(storage)
    chat = create(storage, "Chat", punctuation_style="casual")
    assign(storage, SLACK, chat)
    snapshot = styles.snapshot()
    assert snapshot.for_app(SLACK).id == chat
    assert snapshot.apps == {SLACK: chat}
    flags = styles.flags_for_app(SLACK, CaptureSettingsResponse(self_correction=False))
    # Filler removal and a capitalized first word are no longer settings.
    assert flags == RefinementFlags(self_correction=False, punctuation_style="casual", style=chat)


def test_confirming_new_apps_keeps_them_in_their_style(storage):
    seed(storage)
    chat = create(storage, "Chat")
    assign(storage, SLACK, chat)
    with storage() as db:
        styles.confirm_apps(db, [(SLACK, "Slack"), (MAIL, "Mail")])
    assert styles.snapshot().apps == {SLACK: chat, MAIL: "personal"}


def test_styles_are_created_renamed_and_deleted(storage):
    seed(storage)
    with storage() as db:
        notes = styles.create_style(db, "  Notes   app ")
        assert notes.name == "Notes app"
        with pytest.raises(ValueError, match="already a style"):
            styles.create_style(db, "personal")
        styles.assign_app(db, MAIL, "Mail", notes.id)
        styles.update_style(db, notes.id, {"name": "Journal", "punctuation_style": "casual"})
        with pytest.raises(ValueError, match="default"):
            styles.delete_style(db, "personal")
        assert styles.delete_style(db, notes.id)
    # Its app goes back to the default.
    assert styles.snapshot().for_app(MAIL).id == "personal"
    journal = create(storage, "Journal")
    with storage() as db:
        assert db.query(AppStyle).count() == 0
        styles.update_style(db, journal, {"is_default": True})
        assert [row.id for row in db.query(WritingStyle).filter(WritingStyle.is_default)] == [journal]


def test_version_1_profiles_move_into_personal(storage, tmp_path):
    (tmp_path / "writing-style.json").write_text(
        json.dumps(
            {
                "version": 1,
                "runs": 2,
                "last_run_at": "2026-09-21T21:38:19+00:00",
                "examples": [{"shown": "Hi. There.", "written": "hi, there", "created_at": "x"}],
                "recent_paragraphs": ["a"],
                "feedback_counts": {"boundary": {"period": 0, "comma": 3, "none": 0}},
                "hidden_examples": ["correction:gone"],
            }
        )
    )
    (tmp_path / "correction-notes.json").write_text(
        json.dumps({"version": 1, "notes": ["Write Voicebox as one word."], "considered_ids": ["c1"]})
    )
    seed(storage)
    other = create(storage, "Chat")
    assert writing_style.status("personal")["runs"] == 2
    assert writing_style.status(other)["runs"] == 0
    assert writing_style.hidden_examples() == ["correction:gone"]
    assert correction_notes.notes("personal") == ["Write Voicebox as one word."]
    assert correction_notes.notes(other) == []
    # Nothing is lost when the file is written back in version 2.
    writing_style.hide_example("correction:also-gone")
    saved = json.loads((tmp_path / "writing-style.json").read_text())
    assert saved["version"] == 2
    assert saved["styles"]["personal"]["runs"] == 2
    assert saved["styles"]["personal"]["examples"][0]["written"] == "hi, there"


def test_corrections_teach_only_their_apps_style(storage):
    seed(storage)
    chat = create(storage, "Chat")
    assign(storage, SLACK, chat)
    correct(storage, "s", SLACK, "yeah so the build is broken", "yeah the build is broken")
    correct(storage, "m", MAIL, "hi priya the build is broken", "Hi Priya, the build is broken.")
    correct(storage, "u", None, "old one from before apps", "Old one from before apps.")
    assert said(chat) == {"yeah so the build is broken"}
    assert said("personal") == {"hi priya the build is broken", "old one from before apps"}
    assert personal_examples.all_examples(chat)[0]["app_bundle_id"] == SLACK


def test_moving_an_app_brings_its_corrections_or_leaves_them(storage):
    seed(storage)
    formal, other = create(storage, "Formal"), create(storage, "Other")
    correct(storage, "m1", MAIL, "hi priya the build is broken", "Hi Priya, the build is broken.")
    correct(storage, "m2", MAIL, "thanks so much for the help", "Thanks so much for the help.")
    with storage() as db:
        assert styles.app_corrections(db) == {MAIL: 2}

    # Leave them: Personal keeps learning from them, Formal starts empty.
    assign(storage, MAIL, formal, "leave")
    assert said("personal") == {"hi priya the build is broken", "thanks so much for the help"}
    assert said(formal) == set()
    with storage() as db:
        assert styles.app_corrections(db) == {}
    # A correction made in Mail now teaches Formal.
    correct(storage, "m3", MAIL, "see you tomorrow then", "See you tomorrow then.")
    assert said(formal) == {"see you tomorrow then"}

    # Bring them: only what Mail teaches Formal moves on; what was left in Personal stays there.
    assign(storage, MAIL, other, "bring")
    assert said(other) == {"see you tomorrow then"}
    assert said(formal) == set()
    assert said("personal") == {"hi priya the build is broken", "thanks so much for the help"}


def test_left_corrections_come_back_to_the_app_when_their_style_is_deleted(storage):
    seed(storage)
    formal = create(storage, "Formal")
    other = create(storage, "Other")
    assign(storage, MAIL, other)
    correct(storage, "m1", MAIL, "hi priya the build is broken", "Hi Priya, the build is broken.")
    assign(storage, MAIL, formal, "leave")
    assert said(other) == {"hi priya the build is broken"}
    with storage() as db:
        styles.delete_style(db, other)
    assert said(formal) == {"hi priya the build is broken"}


def feedback(storage, capture_id, app):
    """A correction that turns sentence breaks into commas: a learned habit."""
    correct(storage, capture_id, app, "x", "it works, ship it", refined="It works. Ship it.")


def test_habits_follow_the_choice_too(storage):
    seed(storage)
    chat = create(storage, "Chat")
    for index in range(3):
        feedback(storage, f"s{index}", SLACK)
    with storage() as db:
        writing_style.refresh_feedback(db)
    assert "boundary_comma" in writing_style.status("personal")["habits"]

    assign(storage, SLACK, chat, "leave")
    assert "boundary_comma" in writing_style.status("personal")["habits"]
    assert writing_style.status(chat)["habits"] == []

    other = create(storage, "Other")
    assign(storage, SLACK, chat)
    for index in range(3, 6):
        feedback(storage, f"s{index}", SLACK)
    assign(storage, SLACK, other, "bring")
    assert "boundary_comma" in writing_style.status(other)["habits"]
    assert writing_style.status(chat)["habits"] == []
    chat_flags = RefinementFlags(punctuation_style="learned", style=other)
    assert "Join related thoughts with commas" in build_refinement_prompt(chat_flags)
    assert writing_style.apply_learned("It works. Ship it.", other) == "it works, ship it"


def test_moved_corrections_become_pending_rules_for_the_new_style(storage, monkeypatch):
    seed(storage)
    formal = create(storage, "Formal")
    monkeypatch.setattr(personal_examples, "MAX_PROMPT_EXAMPLES", 1)
    for index in range(3):
        correct(storage, f"m{index}", MAIL, f"so the thing number {index}", f"The thing number {index}.")
    assert len(correction_notes.pending("personal")) == 2
    assign(storage, MAIL, formal, "bring")
    assert correction_notes.pending("personal") == []
    assert len(correction_notes.pending(formal)) == 2


# --- Suggestions ----------------------------------------------------------------


def snapshot_with(apps, *names):
    made = tuple(
        styles.Style(name.lower(), name, index, index == 0, "standard", True) for index, name in enumerate(names)
    )
    return styles.Snapshot(made, apps)


def test_a_new_app_gets_the_style_most_apps_of_its_category_use():
    snapshot = snapshot_with(
        {"cursor": "code", "xcode": "code", "terminal": "chat", "slack": "chat"}, "Personal", "Chat", "Code"
    )
    categories = {
        "cursor": "public.app-category.developer-tools",
        "xcode": "public.app-category.developer-tools",
        "terminal": "public.app-category.developer-tools",
        "slack": "public.app-category.business",
        "zed": "public.app-category.developer-tools",
        "teams": "public.app-category.business",
        "game": "public.app-category.games",
        "unknown": None,
    }
    assert styles.suggest_styles(categories, snapshot) == {
        "zed": "code",
        "teams": "chat",
        # No assigned app in its category, or no category: the default.
        "game": "personal",
        "unknown": "personal",
    }


def test_ties_go_to_the_style_listed_first():
    snapshot = snapshot_with({"a": "code", "b": "chat"}, "Personal", "Chat", "Code")
    categories = {"a": "tools", "b": "tools", "c": "tools"}
    assert styles.suggest_styles(categories, snapshot) == {"c": "chat"}


def test_the_listing_suggests_from_captured_categories(storage):
    seed(storage)
    code = create(storage, "Code")
    with storage() as db:
        for capture_id, app, category in (
            ("c1", "com.todesktop.cursor", "public.app-category.developer-tools"),
            ("z1", "dev.zed.Zed", None),
            ("z2", "dev.zed.Zed", "public.app-category.developer-tools"),
            ("g1", "com.example.game", "public.app-category.games"),
        ):
            db.add(Capture(id=capture_id, audio_path="a.wav", app_bundle_id=app, app_category=category))
        db.commit()
    assign(storage, "com.todesktop.cursor", code)
    from backend.services.captures import list_capture_apps

    with storage() as db:
        apps = {app.app_bundle_id: app for app in list_capture_apps(db).apps}
    assert apps["dev.zed.Zed"].suggested_style_id == code
    assert apps["com.example.game"].suggested_style_id == "personal"
    assert apps["com.todesktop.cursor"].suggested_style_id is None


# --- Cache size -----------------------------------------------------------------


class Backend:
    """kv_bytes_per_token as Qwen3-4B's config gives it: 36 layers, 8 KV heads of 128."""

    def __init__(self, tokens=None):
        self.tokens = tokens

    def kv_bytes_per_token(self, size):
        return {"0.6B": 28, "1.7B": 28, "4B": 36}[size] * 8 * 128 * 2 * 2

    def prompt_tokens(self, system, examples, size):
        return self.tokens


def test_cache_size_comes_from_the_model_and_the_prompt(monkeypatch):
    from backend.services import llm, refinement

    monkeypatch.setattr(llm, "get_llm_model", lambda: Backend(tokens=2151))
    flags = RefinementFlags(style="none")
    # 2151 tokens and a dictation round up to 2304 cached tokens, as measured.
    assert refinement.style_cache_bytes(flags, "4B") == 2304 * 147456
    assert refinement.style_cache_bytes(flags, "0.6B") == 2304 * 114688
    # Without the tokenizer loaded, the prompt's length estimates it.
    monkeypatch.setattr(llm, "get_llm_model", lambda: Backend())
    estimate = refinement.style_cache_bytes(flags, "4B")
    assert 256 * 147456 <= estimate <= 2048 * 147456


def test_the_real_4b_config_gives_147_kb_per_token():
    from backend.backends.qwen_llm_backend import MLXQwenLLMBackend

    per_token = MLXQwenLLMBackend("4B").kv_bytes_per_token("4B")
    if per_token is None:
        pytest.skip("Qwen3-4B is not downloaded")
    # Measured: a 2210-token cache held 325,877,760 bytes.
    assert per_token == 325877760 // 2210


def test_first_word_follows_the_style():
    lowercase = RefinementFlags(capitalize_first=False)
    assert style_first_word("So I looked at the build.", lowercase) == "so I looked at the build."
    assert style_first_word("I looked at the build.", lowercase) == "I looked at the build."
    assert style_first_word("API keys go here.", lowercase) == "API keys go here."
    assert style_first_word("Priya, the build is broken.", lowercase, frozenset({"Priya"})) == (
        "Priya, the build is broken."
    )
    assert style_first_word("So I looked.", RefinementFlags()) == "So I looked."


def test_flags_keep_old_snapshots_equal():
    assert RefinementFlags().to_dict() == {"smart_cleanup": True, "self_correction": True, "preserve_technical": True}
    flags = RefinementFlags(capitalize_first=False, style="chat")
    assert flags.to_dict()["capitalize_first"] is False
    assert RefinementFlags.from_dict(flags.to_dict()) == flags
    assert RefinementFlags.from_dict({"smart_cleanup": True}) == RefinementFlags()


def test_worker_style_routes(storage, monkeypatch):
    from backend.routes.styles import router
    from backend.services import correction_notes, llm, personal_examples, teach, writing_style

    monkeypatch.setattr(llm, "get_llm_model", lambda: Backend(tokens=1000))
    seed(storage)
    with storage() as db:
        db.add(CaptureSettings(id=1, llm_model="4B"))
        db.commit()
    calls = []
    monkeypatch.setattr(personal_examples, "invalidate", lambda: calls.append("examples"))
    monkeypatch.setattr(writing_style, "forget_style", lambda style: calls.append(("habits", style)))
    monkeypatch.setattr(teach, "forget_style", lambda style: calls.append(("teach", style)))
    monkeypatch.setattr(correction_notes, "forget_style", lambda style: calls.append(("notes", style)))
    monkeypatch.setattr(writing_style, "refresh_feedback", lambda db: calls.append("regroup"))

    def override():
        with storage() as db:
            yield db

    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_db] = override
    client = TestClient(app)

    state = client.post("/worker/styles/state").json()
    # 1000 prompt tokens and a dictation round up to 1280 cached tokens on 4B.
    assert state == {"cache_mb_per_style": round(1280 * 147456 / 1e6), "teaching_style": None}

    # The server wrote a style: the worker sees it once told.
    styles.snapshot()
    with storage() as db:
        db.add(WritingStyle(id="chat", name="Chat", position=1, is_default=False, punctuation_style="standard"))
        db.commit()
    assert client.post("/worker/styles/changed", json={}).json() == {"ok": True}
    assert [style.id for style in styles.snapshot().styles] == ["personal", "chat"]
    assert calls == []

    client.post("/worker/styles/changed", json={"examples": True, "deleted": "chat", "regroup": True})
    assert calls == ["examples", ("habits", "chat"), ("teach", "chat"), ("notes", "chat"), "regroup"]


def test_a_style_description_goes_into_only_its_own_cleanup_prompt(storage):
    seed(storage)
    chat = create(storage, "Chat", description="Mostly coworkers, so informal. No greetings.")
    assert "Mostly coworkers, so informal. No greetings." in build_refinement_prompt(RefinementFlags(style=chat))
    assert "coworkers" not in build_refinement_prompt(RefinementFlags(style="personal"))


def test_word_fixes_do_not_outvote_the_endings_the_user_dropped(storage):
    seed(storage)
    for index in range(4):
        correct(storage, f"w{index}", SLACK, "command and push", "Commit and push.", refined="Command and push.")
    for index, (shown, meant) in enumerate([("Commit.", "Commit"), ("Status.", "Status")]):
        correct(storage, f"d{index}", SLACK, meant.lower(), meant, refined=shown)
    with storage() as db:
        writing_style.refresh_feedback(db)
    assert writing_style.is_ready("personal")
    assert writing_style.status("personal")["habits"] == ["drop_final_period"]
    assert writing_style.apply_learned("Commit. Then push.", "personal") == "Commit. Then push"

    flags = RefinementFlags(punctuation_style="learned", style="personal")
    _, examples = refinement._prompt(flags, True, None, None)
    assert ("command and push", "Commit and push") in examples


def test_counts_from_an_older_observe_are_recounted_once(storage):
    seed(storage)
    correct(storage, "d", SLACK, "status", "Status", refined="Status.")
    writing_style._save({**writing_style._load(), "counting": 1})
    with storage() as db:
        writing_style.recount_if_stale(db)
    assert writing_style._profile("personal")["feedback_counts"]["final_period"] == {"kept": 0, "dropped": 1}
    assert writing_style._load()["counting"] == writing_style.COUNTING
