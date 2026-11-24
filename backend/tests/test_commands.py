"""Command Mode on the worker: the prompt, the rewrite and its post-processing."""

import pytest

from backend.services import commands


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


# --- Loading the cleanup model ------------------------------------------------------


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
