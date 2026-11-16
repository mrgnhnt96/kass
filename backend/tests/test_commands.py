"""Command Mode: prompt, transform names, rewrite post-processing, endpoint and stream session."""

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import Session, sessionmaker
from sqlalchemy.pool import StaticPool

from backend.database import get_db
from backend.database.models import Base, Capture, CaptureSettings
from backend.models import CaptureSettingsResponse
from backend.services import commands
from backend.services.refinement import RefinementFlags, build_refinement_prompt


class FakeLLM:
    """Records generate calls and answers with a fixed reply."""

    model_size = "1.7B"

    def __init__(self, reply="Rewritten."):
        self.reply = reply
        self.calls = []

    async def generate(self, **kwargs):
        from backend.backends.qwen_llm_backend import generation_hint

        self.calls.append({**kwargs, "hint": generation_hint.get()})
        return self.reply


# --- Prompt -------------------------------------------------------------------------


def test_command_prompt_allows_translation_and_asks_for_the_edited_text_only():
    prompt = commands.build_command_prompt()
    assert "Change the language only when the instruction names another one" in prompt
    assert "Do not translate" not in prompt
    assert "edited passage only" in prompt


def test_dictation_prompt_still_forbids_translation():
    assert "Do not translate" in build_refinement_prompt(RefinementFlags())


def test_selection_comes_first_so_it_can_be_prefilled_before_the_instruction():
    message = commands.command_message("Hello there.", " make it formal ")
    assert message.startswith(commands.selection_block("Hello there."))
    assert message.endswith("Instruction: make it formal")


def test_every_example_stays_in_the_passage_language():
    # An example in another language pulled "more formal" into that language on
    # 0.6B wherever it sat; the rules alone keep translation working.
    assert all(
        result.isascii() and "translate" not in instruction for _, instruction, result in commands.COMMAND_EXAMPLES
    )
    examples = commands.command_examples()
    assert len(examples) == len(commands.COMMAND_EXAMPLES)
    assert all(user.startswith("<text>\n") and "\nInstruction: " in user for user, _ in examples)


# --- Transforms ---------------------------------------------------------------------


@pytest.mark.parametrize(
    "spoken",
    [
        "polish",
        "Polish.",
        "polish this",
        "Polish this, please.",
        "please polish the text",
        "Okay, run polish on this.",
        "can you polish that",
        "apply polish to the selection",
    ],
)
def test_saying_a_transform_name_runs_its_instruction(spoken):
    instruction, transform = commands.resolve_instruction(spoken, commands.default_transforms())
    assert transform["name"] == "Polish"
    assert instruction == commands.DEFAULT_TRANSFORMS[0]["instruction"]


@pytest.mark.parametrize("spoken", ["prompt engineer", "Prompt engineer this.", "use prompt engineer"])
def test_multi_word_names_match_whatever_their_case_and_punctuation(spoken):
    _, transform = commands.resolve_instruction(spoken, commands.default_transforms())
    assert transform["name"] == "Prompt Engineer"


@pytest.mark.parametrize(
    "spoken",
    [
        "make this more concise",
        "polish this and translate it to French",
        "polishing",
        "don't polish this",
        "",
    ],
)
def test_anything_else_is_a_free_form_instruction(spoken):
    instruction, transform = commands.resolve_instruction(spoken, commands.default_transforms())
    assert transform is None
    assert instruction == spoken.strip()


def test_a_name_ending_in_a_reference_to_the_selection_still_matches():
    transforms = [{"id": "x", "name": "Fix this", "instruction": "Fix it."}]
    assert commands.match_transform("fix this", transforms)["id"] == "x"
    assert commands.match_transform("please fix this", transforms)["id"] == "x"


def test_transforms_are_validated_before_they_are_saved():
    saved = commands.normalize_transforms([{"name": " Shorten ", "instruction": " Make it shorter. "}])
    assert saved[0]["name"] == "Shorten"
    assert saved[0]["instruction"] == "Make it shorter."
    assert saved[0]["id"]
    with pytest.raises(ValueError, match="called"):
        commands.normalize_transforms([{"name": "Polish", "instruction": "a"}, {"name": "polish!", "instruction": "b"}])
    with pytest.raises(ValueError, match="name and an instruction"):
        commands.normalize_transforms([{"name": "Empty", "instruction": " "}])
    with pytest.raises(ValueError, match="said aloud"):
        commands.normalize_transforms([{"name": "!!!", "instruction": "x"}])


def test_new_installs_get_the_default_transforms_and_command_chord():
    engine = create_engine("sqlite:///:memory:")
    Base.metadata.create_all(engine)
    with Session(engine) as db:
        from backend.services.settings import get_capture_settings

        response = CaptureSettingsResponse.model_validate(get_capture_settings(db))
    assert [t.name for t in response.command_transforms] == ["Polish", "Prompt Engineer"]
    assert response.chord_command_keys == ["MetaRight", "ShiftRight"]


def test_existing_installs_are_migrated_with_the_defaults(tmp_path):
    import json

    from sqlalchemy import text

    from backend.database.migrations import run_migrations

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    with engine.begin() as connection:
        connection.execute(text("CREATE TABLE capture_settings (id INTEGER PRIMARY KEY, stt_model VARCHAR)"))
        connection.execute(text("INSERT INTO capture_settings (id, stt_model) VALUES (1, 'turbo')"))
        connection.execute(text("CREATE TABLE captures (id VARCHAR PRIMARY KEY, audio_path VARCHAR)"))
    run_migrations(engine)
    run_migrations(engine)
    with engine.connect() as connection:
        transforms, chord = connection.execute(
            text("SELECT command_transforms, chord_command_keys FROM capture_settings")
        ).one()
        columns = [row[1] for row in connection.execute(text("PRAGMA table_info(captures)"))]
    # The defaults' apostrophes ("don't") survive the SQL literal.
    assert json.loads(transforms) == commands.default_transforms()
    assert json.loads(chord) == ["MetaRight", "ShiftRight"]
    assert {"command_selection", "command_instruction", "command_transform"} <= set(columns)


def test_databases_with_the_retired_command_model_column_keep_working(tmp_path):
    # Command Mode once had its own model setting; existing databases keep the column.
    from sqlalchemy import text

    from backend.database.migrations import run_migrations
    from backend.services.settings import get_capture_settings
    from backend.tests.settings_rows import save_settings

    engine = create_engine(f"sqlite:///{tmp_path / 'old.db'}")
    Base.metadata.create_all(engine)
    with engine.begin() as connection:
        connection.execute(
            text("ALTER TABLE capture_settings ADD COLUMN command_llm_model VARCHAR NOT NULL DEFAULT '1.7B'")
        )
    run_migrations(engine)
    with Session(engine) as db:
        get_capture_settings(db)
        row = save_settings(db, llm_model="4B")
        assert CaptureSettingsResponse.model_validate(row).llm_model == "4B"


# --- Rewrite ------------------------------------------------------------------------


def test_the_selection_keeps_its_surrounding_whitespace():
    assert commands.finish_rewrite("\n  hello there \n", "Hi.") == "\n  Hi. \n"


def test_echoed_markup_and_fences_the_selection_lacked_are_removed():
    assert commands.finish_rewrite("hi", "<text>\nHello.\n</text>") == "Hello."
    assert commands.finish_rewrite("hi", "```\nHello.\n```") == "Hello."
    fenced = "```py\nprint(1)\n```"
    assert commands.finish_rewrite(fenced, fenced) == fenced


def test_markdown_hard_breaks_are_dropped_unless_the_selection_has_them():
    assert commands.finish_rewrite("a, b", "- a  \n- b") == "- a\n- b"
    assert commands.finish_rewrite("a  \nb", "a  \nc") == "a  \nc"


@pytest.mark.asyncio
async def test_rewrite_uses_the_command_prompt_with_lookup_decoding():
    llm = FakeLLM("  The rewrite.  ")
    text, model = await commands.rewrite("the text", "make it better", "1.7B", backend_override=llm)
    assert (text, model) == ("The rewrite.", "1.7B")
    call = llm.calls[0]
    assert call["system"] == commands.build_command_prompt()
    assert call["prompt"] == commands.command_message("the text", "make it better")
    assert call["model_size"] == "1.7B"
    assert call["hint"] == ""


@pytest.mark.asyncio
async def test_prefill_shares_the_rewrites_prompt_up_to_the_instruction():
    llm = FakeLLM()
    await commands.prefill("the text", "1.7B", backend_override=llm)
    await commands.rewrite("the text", "shorter", "1.7B", backend_override=llm)
    warm, real = llm.calls
    assert warm["max_tokens"] == 1
    assert (warm["system"], warm["examples"]) == (real["system"], real["examples"])
    assert real["prompt"].startswith(warm["prompt"])


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("selection", "instruction", "error"),
    [("  ", "shorter", "Select text"), ("x", " ", "No instruction"), ("x" * 16_001, "shorter", "too long")],
)
async def test_bad_requests_never_reach_the_model(selection, instruction, error):
    llm = FakeLLM()
    with pytest.raises(ValueError, match=error):
        await commands.rewrite(selection, instruction, "1.7B", backend_override=llm)
    assert not llm.calls


@pytest.mark.asyncio
async def test_an_empty_rewrite_is_an_error_not_an_empty_replacement():
    with pytest.raises(ValueError, match="empty"):
        await commands.rewrite("text", "delete the filler", "1.7B", backend_override=FakeLLM("<text>\n\n</text>"))


# --- Endpoint -----------------------------------------------------------------------


@pytest.fixture
def api(monkeypatch):
    from backend.routes.commands import router

    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)

    def override():
        with make() as db:
            yield db

    llm = FakeLLM("Hola.")
    monkeypatch.setattr(commands, "_llm", lambda: llm)
    monkeypatch.setattr(commands, "ensure_model_ready", lambda size: None)
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_db] = override
    client = TestClient(app)
    client.llm, client.db = llm, make
    return client


def test_endpoint_rewrites_and_saves_a_command_capture(api):
    response = api.post(
        "/commands/run",
        json={"selection": "Hello.", "instruction": "translate to Spanish", "app_name": "Notes"},
    )
    assert response.status_code == 200
    body = response.json()
    assert body["source"] == "command"
    assert body["transcript_refined"] == "Hola."
    assert body["command_selection"] == "Hello."
    assert body["command_instruction"] == "translate to Spanish"
    assert body["command_transform"] is None
    assert body["app_name"] == "Notes"
    assert body["llm_model"] == CaptureSettingsResponse().llm_model
    with api.db() as db:
        assert db.query(Capture).one().audio_path == ""


def test_endpoint_rewrites_on_the_dictation_cleanup_model(api):
    # One model for both, so a command never switches models.
    with api.db() as db:
        db.add(CaptureSettings(id=1, llm_model="4B"))
        db.commit()
    body = api.post("/commands/run", json={"selection": "Hello.", "instruction": "shorter"}).json()
    assert body["llm_model"] == "4B"
    assert api.llm.calls[0]["model_size"] == "4B"


def test_endpoint_runs_a_transform_by_name(api):
    body = api.post("/commands/run", json={"selection": "hi", "instruction": "Polish"}).json()
    assert body["command_transform"] == "Polish"
    assert body["command_instruction"] == commands.DEFAULT_TRANSFORMS[0]["instruction"]
    assert api.llm.calls[0]["prompt"].endswith(commands.DEFAULT_TRANSFORMS[0]["instruction"])


def test_endpoint_uses_the_users_own_transforms(api):
    with api.db() as db:
        db.add(
            CaptureSettings(
                id=1, command_transforms=[{"id": "t", "name": "Pirate", "instruction": "Talk like a pirate."}]
            )
        )
        db.commit()
    body = api.post("/commands/run", json={"selection": "hi", "instruction": "pirate this"}).json()
    assert body["command_instruction"] == "Talk like a pirate."


def test_endpoint_turns_an_uploaded_command_recording_into_its_rewrite(api):
    with api.db() as db:
        db.add(Capture(id="rec", audio_path="captures/rec.wav", source="command", transcript_raw="make it Spanish"))
        db.commit()
    body = api.post("/commands/run", json={"selection": "Hello.", "capture_id": "rec"}).json()
    assert body["id"] == "rec"
    assert body["transcript_raw"] == "make it Spanish"
    assert body["transcript_refined"] == "Hola."
    assert api.post("/commands/run", json={"selection": "x", "capture_id": "missing"}).status_code == 404


def test_endpoint_reports_bad_requests(api):
    response = api.post("/commands/run", json={"selection": "", "instruction": "shorter"})
    assert response.status_code == 400
    assert "Select text" in response.json()["detail"]
    assert not api.llm.calls


# --- Loading the cleanup model ------------------------------------------------------


@pytest.mark.asyncio
async def test_a_dictation_loads_its_cleanup_model_before_the_first_phrase(monkeypatch):
    from backend.services import llm as llm_service, refinement

    prepared = []

    class Backend:
        supports_adapters = False

        async def prepare(self, model_size, adapter_path=None):
            prepared.append((model_size, adapter_path))

    monkeypatch.setattr(llm_service, "get_llm_model", lambda: Backend())
    await refinement.load_cleanup_model(RefinementFlags(), "0.6B")
    assert prepared == [("0.6B", None)]


def test_preparing_swaps_the_adapter_and_size_exactly_as_generate_would(monkeypatch):
    from backend.backends.qwen_llm_backend import MLXQwenLLMBackend

    backend = MLXQwenLLMBackend()
    calls = []
    monkeypatch.setattr(backend, "unload_model", lambda: calls.append("unload"))
    monkeypatch.setattr(backend, "_ensure_loaded_sync", lambda size: calls.append(size))
    backend._ensure_ready_sync("1.7B", None)
    assert calls == ["1.7B"]
    backend._ensure_ready_sync("0.6B", "adapters/a")
    assert calls == ["1.7B", "unload", "0.6B"]
    assert backend._adapter_path == "adapters/a"
