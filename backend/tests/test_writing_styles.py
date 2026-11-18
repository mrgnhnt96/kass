"""Per-app writing styles: migration, assignment, suggestions, and what each style learns."""

import json
from types import SimpleNamespace

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import config
from backend.database import session as database_session
from backend.database.models import Base, Capture, CaptureFeedback, CaptureSettings
from backend.services import correction_notes, personal_examples, refinement, styles, writing_style
from backend.services.refinement import RefinementFlags, build_refinement_prompt, style_first_word

from .style_rows import add_style, drop_style, put_app, seed_styles

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
        seed_styles(db)


def create(storage, name, **settings):
    with storage() as db:
        return add_style(db, name, **settings)


def assign(storage, app, style_id):
    with storage() as db:
        put_app(db, app, style_id)


def correct(storage, capture_id, app, said, meant, refined=None, teaches=None):
    with storage() as db:
        db.add(
            Capture(id=capture_id, audio_path="a.wav", transcript_raw=said, app_bundle_id=app, teaches_style_id=teaches)
        )
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


def test_a_fresh_install_starts_with_personal_only(storage):
    seed(storage)
    (personal,) = styles.snapshot().styles
    assert (personal.name, personal.is_default, personal.punctuation_style) == ("Personal", True, "standard")


def test_an_assigned_app_picks_its_styles_flags(storage):
    seed(storage)
    chat = create(storage, "Chat", punctuation_style="casual")
    assign(storage, SLACK, chat)
    snapshot = styles.snapshot()
    assert snapshot.for_app(SLACK).id == chat
    assert snapshot.apps == {SLACK: chat}
    flags = styles.flags_for(snapshot.for_app(SLACK), SimpleNamespace(self_correction=False))
    # Filler removal and a capitalized first word are no longer settings.
    assert flags == RefinementFlags(self_correction=False, punctuation_style="casual", style=chat)


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


def test_corrections_left_with_a_style_teach_it_until_it_is_deleted(storage):
    seed(storage)
    formal, other = create(storage, "Formal"), create(storage, "Other")
    assign(storage, MAIL, formal)
    # Mail moved to Formal and left these with Personal and Other.
    correct(storage, "m1", MAIL, "hi priya the build is broken", "Hi Priya, the build is broken.", teaches="personal")
    correct(storage, "m2", MAIL, "thanks so much for the help", "Thanks so much for the help.", teaches=other)
    correct(storage, "m3", MAIL, "see you tomorrow then", "See you tomorrow then.")
    assert said("personal") == {"hi priya the build is broken"}
    assert said(other) == {"thanks so much for the help"}
    assert said(formal) == {"see you tomorrow then"}
    # A deleted style's corrections come back to the app's style.
    with storage() as db:
        drop_style(db, other)
    assert said(formal) == {"thanks so much for the help", "see you tomorrow then"}


def feedback(storage, capture_id, app):
    """A correction that turns sentence breaks into commas: a learned habit."""
    correct(storage, capture_id, app, "x", "it works, ship it", refined="It works. Ship it.")


def out_of_prompt(style_id):
    """The style's examples left out of its prompt: what its notes are summarized from."""
    shown = {example["id"] for example in personal_examples.in_prompt(style_id)}
    return [example for example in personal_examples.all_examples(style_id) if example["id"] not in shown]


def test_an_apps_corrections_become_pending_rules_for_its_new_style(storage, monkeypatch):
    seed(storage)
    formal = create(storage, "Formal")
    monkeypatch.setattr(personal_examples, "MAX_PROMPT_EXAMPLES", 1)
    for index in range(3):
        correct(storage, f"m{index}", MAIL, f"so the thing number {index}", f"The thing number {index}.")
    assert len(out_of_prompt("personal")) == 2
    assign(storage, MAIL, formal)
    assert out_of_prompt("personal") == []
    assert len(out_of_prompt(formal)) == 2


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
    from backend.services import llm

    prompt = {"system": "x" * 3800, "examples": []}
    client = TestClient(_worker_app())
    monkeypatch.setattr(llm, "get_llm_model", lambda: Backend(tokens=2151))
    # 2151 tokens and a dictation round up to 2304 cached tokens, as measured.
    assert client.post("/worker/cleanup/cache-bytes", json={"model_size": "4B", "prompts": [prompt]}).json() == {
        "bytes": [2304 * 147456]
    }
    assert client.post("/worker/cleanup/cache-bytes", json={"model_size": "0.6B", "prompts": [prompt]}).json() == {
        "bytes": [2304 * 114688]
    }
    # Without the tokenizer loaded, the prompt's length estimates it: 1000 tokens.
    monkeypatch.setattr(llm, "get_llm_model", lambda: Backend())
    assert client.post("/worker/cleanup/cache-bytes", json={"model_size": "4B", "prompts": [prompt]}).json() == {
        "bytes": [1280 * 147456]
    }
    # Without the model's config there is nothing to count.
    monkeypatch.setattr(llm, "get_llm_model", lambda: object())
    assert client.post("/worker/cleanup/cache-bytes", json={"model_size": "4B", "prompts": [prompt]}).json() == {
        "bytes": [None]
    }


def _worker_app():
    from backend.routes.stream_models import router

    app = FastAPI()
    app.include_router(router)
    return app


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
    # The Rust server counts these (tests/learned.rs); this is what it saves.
    shown_meant = [("Command and push.", "Commit and push.")] * 4 + [("Commit.", "Commit"), ("Status.", "Status")]
    counts = writing_style._merge(*(writing_style.observe(s, m, deliberate=False) for s, m in shown_meant))
    profile = {**writing_style._empty_style(), "feedback_counts": counts}
    writing_style._save({**writing_style._load(), "styles": {"personal": profile}})
    assert writing_style.is_ready("personal")
    assert writing_style.status("personal")["habits"] == ["drop_final_period"]
    assert writing_style.apply_learned("Commit. Then push.", "personal") == "Commit. Then push"

    flags = RefinementFlags(punctuation_style="learned", style="personal")
    _, examples = refinement._prompt(flags, True, None, None)
    assert ("command and push", "Commit and push") in examples
