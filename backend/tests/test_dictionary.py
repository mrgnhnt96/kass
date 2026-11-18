"""Dictionaries: scopes, what a dictation's merged dictionary does, and the worker's routes (docs/plans/DICTIONARIES.md)."""

import re
import statistics
import time
from datetime import datetime, timedelta

import pytest
from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend.database import session as database_session
from backend.database.models import Base, DictionaryEntry
from backend.services import dictionary, phrase_seams
from backend.services.correction_rules import MAX_TEXT
from backend.services.dictionary import Entry

SLACK, ZED = "com.tinyspeck.slackmacgap", "dev.zed.Zed"
COMMON = {"mark", "slack", "open", "source", "voice", "box"}
START = datetime(2026, 9, 1)


@pytest.fixture(autouse=True)
def common_words(monkeypatch):
    # The real check reads Whisper's vocabulary and the system word list.
    monkeypatch.setattr(phrase_seams, "_common_word", lambda word: word.casefold() in COMMON)


def entry(scope, written, spoken=None, scope_id=None, minutes=0, match_sound=True, phrase=False):
    return Entry(
        f"{scope}-{written}-{spoken}",
        scope,
        scope_id,
        None,
        written,
        spoken,
        START + timedelta(minutes=minutes),
        match_sound=match_sound,
        phrase=phrase,
    )


def merged(*entries, bundle_id=ZED, style_id="code"):
    return dictionary.build(dictionary.resolve(entries, bundle_id, style_id))


def test_the_most_specific_scope_wins_for_the_same_word_said():
    resolved = dictionary.resolve(
        [
            entry("global", "VoiceBox", "voice box"),
            entry("style", "Voice Box", "voice box", "code"),
            entry("app", "Voicebox", "Voice  Box", ZED),
            entry("app", "Other", "voice box", SLACK),
            entry("style", "Nope", "nope", "chat"),
        ],
        ZED,
        "code",
    )

    assert [(e.scope, e.written, overridden) for e, overridden in resolved] == [
        ("app", "Voicebox", False),
        ("style", "Voice Box", True),
        ("global", "VoiceBox", True),
    ]
    assert merged(*[e for e, _ in resolved]).apply("open voice box now") == "open Voicebox now"


def test_terms_come_most_specific_first_and_newest_first_within_a_scope():
    found = merged(
        entry("global", "Kubernetes", minutes=5),
        entry("global", "Tailscale", minutes=9),
        entry("app", "Zed", scope_id=ZED),
        entry("app", "Voicebox", "voice box", ZED),
        entry("style", "mrgnhnt96", scope_id="code"),
    )

    assert found.terms == ("Zed", "mrgnhnt96", "Tailscale", "Kubernetes", "Voicebox")
    assert found.names == {"Zed", "Tailscale", "Kubernetes", "Voicebox"}


def test_replacements_match_whole_words_in_any_case_and_spacing():
    found = merged(entry("global", "Voicebox", "voice box"), entry("global", "Morgan@example.com", "my work email"))

    assert found.apply("Voice box, voice-box and VOICE BOX.") == "Voicebox, Voicebox and Voicebox."
    assert found.apply("Send it to my work email.") == "Send it to Morgan@example.com."
    # Inside other words, nothing changes.
    assert found.apply("Two voice boxes, a voice boxer.") == "Two voice boxes, a voice boxer."


def test_the_longest_spoken_phrase_wins_where_two_overlap():
    found = merged(entry("global", "Voicebox", "voice box"), entry("global", "Voicebox app", "voice box app"))

    assert found.apply("the voice box app") == "the Voicebox app"


def test_a_phrase_writes_its_text_exactly_and_is_never_a_term():
    found = merged(
        entry("global", "you@example.com", "insert my email", phrase=True),
        entry("global", "Thanks,\nMorgan Hunt", "sign off", phrase=True),
        entry("global", "Kubernetes"),
        entry("global", "Morgan"),
    )

    assert found.apply("Send it to insert my email. Sign off.") == "Send it to you@example.com. Thanks,\nMorgan Hunt."
    # What a phrase writes is text to insert: not prompted, not a spelling.
    assert found.terms == ("Kubernetes", "Morgan")
    assert found.apply("you@example.com") == "you@example.com"


def test_what_a_replacement_wrote_is_never_respelled():
    found = merged(
        entry("global", "kubernetis rocks", "my motto", phrase=True),
        entry("global", "Kubernetes"),
    )

    # The term fixes what was heard, but not what the phrase wrote.
    assert found.apply("kubernetis says my motto") == "Kubernetes says kubernetis rocks"


def test_terms_get_their_own_capitals_unless_they_are_common_words():
    found = merged(
        entry("global", "Kubernetes"),
        entry("global", "mrgnhnt96"),
        entry("global", "Mark"),
        entry("global", "Open Source"),
    )

    assert found.apply("kubernetes for MRGNHNT96") == "Kubernetes for mrgnhnt96"
    # "Mark" is a common word: "mark this" is the verb.
    assert found.apply("mark this open source") == "mark this open source"
    # It still counts as a name, so dictation keeps it capitalized mid-sentence.
    assert "Mark" in found.names


def test_terms_are_fixed_where_whisper_heard_them_a_little_wrong():
    found = merged(
        entry("global", "Kubernetes"),
        entry("global", "Saggar"),
        entry("global", "Tailscale"),
        entry("global", "Anthropic"),
    )

    # Spelled close, or sounding the same.
    assert found.apply("Deploy it on Kubernetis.") == "Deploy it on Kubernetes."
    assert found.apply("Ask Sagar about the Anthropik docs.") == "Ask Saggar about the Anthropic docs."
    # Split into words Whisper knows.
    assert found.apply("Open cuber netes and tail scale.") == "Open Kubernetes and Tailscale."
    assert found.apply("Open cuber-netes.") == "Open Kubernetes."


def test_heard_terms_never_take_common_words_or_cross_punctuation():
    found = merged(entry("global", "Voxbox"), entry("global", "Tailscale"), entry("global", "Mark"))

    # "box" is a common word: the user said it.
    assert found.apply("a box") == "a box"
    # Not one phrase: a sentence ends, or a possessive.
    assert found.apply("the tail. Scale it") == "the tail. Scale it"
    assert found.apply("the tail's scale") == "the tail's scale"
    # A term that is a common word is only ever recased where it is exact.
    assert found.apply("mack this") == "mack this"


def test_a_term_that_is_a_common_word_is_still_fixed_where_misheard():
    found = merged(entry("global", "Slack"))

    # "slack" is the word; "Slak" can only be the term.
    assert found.apply("some slack in Slak") == "some slack in Slack"


def test_short_names_need_to_sound_the_same():
    found = merged(entry("global", "Herga"), entry("global", "Morgan"))

    assert found.apply("Helga met Morgen") == "Helga met Morgan"
    assert found.apply("Herrga") == "Herga"


def test_a_term_heard_like_another_term_stays_itself():
    found = merged(entry("global", "Saggar", minutes=1), entry("global", "Sagar"))

    assert found.apply("Sagar and Saggar") == "Sagar and Saggar"
    # A near miss goes to the closest, most specific term.
    assert found.apply("Saggarr") == "Saggar"


def test_a_term_without_sound_matching_leaves_names_that_sound_like_it_alone():
    megan = "Megan, Meagan and Meghann met meghan."

    assert merged(entry("global", "Meghan")).apply(megan) == "Meghan, Meghan and Meghan met Meghan."
    spelled = merged(entry("global", "Meghan", match_sound=False))
    # Only the exact spelling is recased; the others are other names.
    assert spelled.apply(megan) == "Megan, Meagan and Meghann met Meghan."
    # It is still prompted, and still a known name.
    assert spelled.terms == ("Meghan",)
    assert "Meghan" in spelled.names
    assert dictionary.prompt(spelled.terms) == "Meghan."


def test_a_term_without_sound_matching_is_never_taken_for_another_term():
    found = merged(entry("global", "Meghan", match_sound=False), entry("global", "Megan"))

    assert found.apply("Meghan and Megan and Meagan") == "Meghan and Megan and Megan"


def test_the_most_specific_entry_decides_sound_matching():
    found = merged(
        entry("app", "Meghan", scope_id=ZED),
        entry("global", "Meghan", "meg h", match_sound=False),
    )

    assert found.apply("Megan") == "Meghan"


def test_an_empty_dictionary_leaves_text_alone():
    assert dictionary.EMPTY.apply("anything at all") == "anything at all"
    assert merged().terms == ()


def test_terms_fill_the_prompt_budget_in_order():
    count = len  # one token per character, for the test

    fit, dropped = dictionary.fit_terms(["aaaa", "bbbbbbbbbb", "cc", "dd"], count, budget=19)

    # 1 for the final period, then each term plus its separator: 5 + 11 = 17; "cc" needs 3 more.
    assert fit == ["aaaa", "bbbbbbbbbb"]
    # A later, smaller term never jumps ahead of one that didn't fit.
    assert dropped == ["cc", "dd"]
    assert dictionary.prompt(fit) == "aaaa, bbbbbbbbbb."
    assert dictionary.prompt([]) == ""


def test_span_covers_the_longest_match():
    found = merged(entry("global", "Voicebox app", "the voice box app"), entry("global", "Visual Studio Code"))
    assert found.span == 4
    # A term may be heard as one word more than it has.
    assert merged(entry("global", "Kubernetes")).span == 2


def test_grouping_terms_by_first_letter_matches_what_one_flat_list_does():
    terms = [
        "voice box",
        "voice box app",
        "Voicebox",
        "VS Code",
        "vscode",
        "kubernetes",
        "Kass",
        "k8s",
        "3D",
        "3D printer",
        "iOS",
        "iPhone",
        "O'Brien",
        ".NET",
        "C#",
        "\u00c9lodie",
        "\u0130stanbul",
        "a",
    ]
    patterns = [dictionary._phrase(term) for term in terms]
    ordered = sorted(set(patterns), key=len, reverse=True)
    flat = re.compile(r"(?<![\w'\u2019-])(?:" + "|".join(ordered) + r")(?![\w'\u2019-])", re.IGNORECASE)
    grouped = dictionary._bounded(patterns)
    texts = [
        "open the voice box app, then the voice-box and VOICEBOX",
        "vs   code or VS-Code or vscode; Kubernetes on k8s with KASS",
        "a 3D printer and 3d and 3Dprinter, ios iphone IOS",
        "o'brien and O'Brien's .net c# \u00e9lodie \u00c9LODIE \u0130stanbul istanbul",
        "a a-a aa, voice box apps, kassa",
    ]
    for text in texts:
        assert [(m.span(), m.group()) for m in grouped.finditer(text)] == [
            (m.span(), m.group()) for m in flat.finditer(text)
        ], text


def test_a_large_dictionary_stays_under_five_milliseconds():
    entries = [entry("global", f"Term{i}x", f"spoken phrase {i}", minutes=i) for i in range(250)]
    entries += [entry("global", f"Product{i}q", minutes=i) for i in range(250)]
    found = merged(*entries)
    text = ("say spoken phrase 42 and product7q with term3x then more words. " * 100)[:MAX_TEXT]
    found.apply(text)  # compiles once

    timings = []
    for _ in range(25):
        started = time.perf_counter()
        found.apply(text)
        timings.append((time.perf_counter() - started) * 1000)

    assert statistics.median(timings) <= 5
    assert "Term42x" in found.apply("spoken phrase 42")
    assert "Product7q" in found.apply("prodduct 7q")


# -- storage and API -----------------------------------------------------------


@pytest.fixture
def storage(monkeypatch):
    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    Base.metadata.create_all(engine)
    make = sessionmaker(bind=engine)
    monkeypatch.setattr(database_session, "SessionLocal", make)
    return make


@pytest.fixture
def worker(storage):
    from backend.routes.dictionary import router

    app = FastAPI()
    app.include_router(router)
    return TestClient(app)


def test_the_worker_counts_terms_with_whisper_or_leaves_it_to_the_server(worker, monkeypatch):
    from backend.routes import dictionary as routes

    monkeypatch.setattr(routes, "_tokenizer", lambda: None)
    assert worker.post("/worker/dictionary/token-counts", json={"terms": ["Zed"]}).json() == {"counts": None}

    class Tokenizer:
        def encode(self, text):
            return text.split(" ")

    monkeypatch.setattr(routes, "_tokenizer", Tokenizer)
    counted = worker.post("/worker/dictionary/token-counts", json={"terms": ["Zed", "voice box"]}).json()
    assert counted == {"counts": [2, 3]}
    assert worker.post("/worker/dictionary/token-counts", json={"terms": []}).json() == {"counts": []}


def test_dictation_reads_the_entries_the_server_saved(storage):
    # An entry from before groups, as the app's migration leaves it (server/src/db.rs).
    with storage() as db:
        db.add(DictionaryEntry(id="old", scope="global", scope_id="", written="Kass", key="kass"))
        db.commit()
    [saved] = dictionary.entries()
    assert (saved.group_id, saved.match_sound, saved.phrase) == ("old", True, False)
    assert dictionary.for_app(ZED).apply("Ask Cass to fix it") == "Ask Kass to fix it"


@pytest.mark.parametrize(
    ("original", "corrected", "words"),
    [
        # Voice training keeps the ones that sound alike (bank.py).
        ("Test that Pikto works.", "Test that Picto works.", [("Pikto", "Picto")]),
        ("ask Sagar about it", "ask Saggar about it", [("Sagar", "Saggar")]),
        ("open Clard code now", "open Claude code now", [("Clard", "Claude")]),
        ("start a cloud session", "start a Claude session", [("cloud", "Claude")]),
        # Common words, drawn-out spellings and numbers are not vocabulary.
        ("Complete it and push.", "Commit it and push.", []),
        ("it was way better", "it was wayyy better", []),
        ("ticket T3 or 4 is done", "ticket T304 is done", []),
        # Only words swapped for others, not ones added.
        ("send the notes", "send the notes to Picto", []),
        # Words only dropped were heard right; a recased name is still a term.
        ("Whenever I. I. Don't have it", "Whenever I don't have it", []),
        ("push it to github", "push it to GitHub", [("github", "GitHub")]),
    ],
)
def test_a_correction_finds_the_terms_it_fixed(original, corrected, words, monkeypatch):
    spoken = "a about and ask better code commit complete cloud done is it now open push send session start test"
    common = COMMON | set(spoken.split()) | {"that", "the", "notes", "to", "ticket", "was", "way", "works", "or"}
    monkeypatch.setattr(phrase_seams, "_common_word", lambda word: word.casefold() in common)
    assert dictionary.misheard_terms(original, corrected) == words
