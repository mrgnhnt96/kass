"""Dictionaries: scopes, what a dictation's merged dictionary does, and the API (docs/plans/DICTIONARIES.md)."""

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

from backend.database import get_db, session as database_session
from backend.database.models import Base
from backend.services import dictionary, phrase_seams, styles
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
    with make() as db:
        styles.ensure_styles(db)
    return make


@pytest.fixture
def client(storage):
    from backend.routes.dictionary import router

    app = FastAPI()
    app.include_router(router)

    def db():
        with storage() as session:
            yield session

    app.dependency_overrides[get_db] = db
    return TestClient(app)


GLOBAL = [{"scope": "global"}]


def add(client, written, spoken=None, places=GLOBAL):
    return client.post("/dictionary", json={"written": written, "spoken": spoken, "places": places})


def test_the_api_adds_lists_and_rejects_the_same_word_twice(client):
    added = add(client, "  Voicebox ", "voice  box")
    assert added.status_code == 200
    assert added.json() | {"id": "x", "created_at": None} == {
        "id": "x",
        "written": "Voicebox",
        "spoken": "voice box",
        "places": [{"scope": "global", "scope_id": None, "app_name": None}],
        "created_at": None,
        "match_sound": True,
        "source": "user",
        "phrase": False,
    }

    duplicate = add(client, "VoiceBox", "Voice Box")
    assert duplicate.status_code == 409
    assert duplicate.json()["detail"] == "That word is already in the dictionary for everywhere"

    # The same word said is fine in another place.
    in_app = add(client, "voicebox", "voice box", [{"scope": "app", "scope_id": ZED, "app_name": "Zed"}])
    assert in_app.status_code == 200
    assert [e["written"] for e in client.get("/dictionary").json()["entries"]] == ["voicebox", "Voicebox"]


def test_the_api_adds_a_phrase_that_keeps_its_lines(client):
    added = client.post(
        "/dictionary",
        json={"written": "\n Thanks,  \n\n  Morgan \n", "spoken": " sign  off ", "places": GLOBAL, "phrase": True},
    )
    assert added.status_code == 200
    assert (added.json()["written"], added.json()["spoken"], added.json()["phrase"]) == (
        "Thanks,\n\nMorgan",
        "sign off",
        True,
    )
    assert dictionary.for_app(ZED).apply("Sign off") == "Thanks,\n\nMorgan"
    # Longer than a word may be, not than a phrase may.
    assert (
        client.post(
            "/dictionary", json={"written": "x" * 900, "spoken": "long one", "places": GLOBAL, "phrase": True}
        ).status_code
        == 200
    )
    assert (
        client.post(
            "/dictionary", json={"written": "x" * 1001, "spoken": "too long", "places": GLOBAL, "phrase": True}
        ).status_code
        == 400
    )

    # A phrase is said: it needs what you say, and keeps it.
    assert client.post("/dictionary", json={"written": "hi", "places": GLOBAL, "phrase": True}).status_code == 400
    patched = client.patch(f"/dictionary/{added.json()['id']}", json={"spoken": None})
    assert patched.status_code == 400
    edited = client.patch(f"/dictionary/{added.json()['id']}", json={"written": "Cheers,\nMorgan"})
    assert (edited.json()["written"], edited.json()["phrase"]) == ("Cheers,\nMorgan", True)


def test_the_api_rejects_empty_words_and_unknown_places(client):
    assert add(client, "   ").status_code == 400
    assert add(client, "Zed", places=[{"scope": "style", "scope_id": "nope"}]).status_code == 400
    assert add(client, "Zed", places=[{"scope": "app"}]).status_code == 400
    assert add(client, "Zed", places=[]).status_code == 422
    assert add(client, "x" * 201).status_code == 400


def test_one_entry_applies_in_several_places_and_edits_them_together(client, storage):
    with storage() as db:
        code = styles.create_style(db, "Code")
    slack = {"scope": "app", "scope_id": SLACK, "app_name": "Slack"}
    added = add(client, "VoiceBox", "voice box", [{"scope": "style", "scope_id": code.id}, slack]).json()

    assert [p["scope"] for p in added["places"]] == ["style", "app"]
    assert len(client.get("/dictionary").json()["entries"]) == 1
    assert dictionary.for_app(SLACK).apply("voice box") == "VoiceBox"
    assert dictionary.for_app(ZED).apply("voice box") == "voice box"

    # Moving it: Slack stays, the style goes, Zed joins; the words change everywhere.
    zed = {"scope": "app", "scope_id": ZED, "app_name": "Zed"}
    edited = client.patch(f"/dictionary/{added['id']}", json={"written": "Voicebox", "places": [slack, zed]}).json()
    assert [(p["scope"], p["scope_id"]) for p in edited["places"]] == [("app", SLACK), ("app", ZED)]
    assert edited["created_at"] == added["created_at"]
    assert dictionary.for_app(ZED).apply("voice box") == "Voicebox"

    # Everywhere replaces the rest.
    everywhere = client.patch(f"/dictionary/{added['id']}", json={"places": [zed, {"scope": "global"}]}).json()
    assert everywhere["places"] == [{"scope": "global", "scope_id": None, "app_name": None}]

    assert client.delete(f"/dictionary/{added['id']}").json() == {"deleted": True}
    assert client.get("/dictionary").json()["entries"] == []


def test_moving_an_entry_onto_the_same_word_is_refused(client):
    zed = {"scope": "app", "scope_id": ZED, "app_name": "Zed"}
    add(client, "VoiceBox", "voice box", [zed])
    other = add(client, "Voicebox", "voice box").json()

    moved = client.patch(f"/dictionary/{other['id']}", json={"places": [zed]})

    assert moved.status_code == 409
    assert moved.json()["detail"] == "That word is already in the dictionary for Zed"


def test_editing_and_deleting_change_what_dictation_uses(client):
    added = add(client, "Kubernetes").json()
    assert dictionary.for_app(ZED).terms == ("Kubernetes",)

    edited = client.patch(f"/dictionary/{added['id']}", json={"written": "k8s", "spoken": "kates"})
    assert edited.json()["spoken"] == "kates"
    assert dictionary.for_app(ZED).apply("run kates") == "run k8s"

    assert client.delete(f"/dictionary/{added['id']}").json() == {"deleted": True}
    assert dictionary.for_app(ZED).terms == ()
    assert client.delete(f"/dictionary/{added['id']}").status_code == 404


def test_an_app_gets_its_styles_entries_and_resolved_shows_overrides(client, storage):
    with storage() as db:
        code = styles.create_style(db, "Code")
        styles.assign_app(db, ZED, "Zed", code.id)
    add(client, "VoiceBox", "voice box")
    add(client, "mrgnhnt96", places=[{"scope": "style", "scope_id": code.id}])
    mine = add(client, "Voicebox", "voice box", [{"scope": "app", "scope_id": ZED}]).json()

    resolved = client.get("/dictionary/resolved", params={"bundle_id": ZED}).json()

    assert [(e["scope"], e["written"], e["overridden"]) for e in resolved["entries"]] == [
        ("app", "Voicebox", False),
        ("style", "mrgnhnt96", False),
        ("global", "VoiceBox", True),
    ]
    assert resolved["entries"][0]["id"] == mine["id"]
    assert resolved["prompt_terms"] == ["mrgnhnt96", "Voicebox"]
    assert resolved["dropped_terms"] == []
    # Slack is in the default style: only the global entry.
    assert dictionary.for_app(SLACK).apply("voice box") == "VoiceBox"


def test_the_api_turns_sound_matching_off_and_on(client):
    added = add(client, "Meghan").json()
    assert added["match_sound"] is True
    assert added["source"] == "user"
    assert dictionary.for_app(ZED).apply("Megan") == "Meghan"

    edited = client.patch(f"/dictionary/{added['id']}", json={"match_sound": False}).json()
    assert edited["match_sound"] is False
    assert dictionary.for_app(ZED).apply("Megan") == "Megan"
    resolved = client.get("/dictionary/resolved", params={"bundle_id": ZED}).json()
    assert [(e["written"], e["match_sound"]) for e in resolved["entries"]] == [("Meghan", False)]
    assert resolved["prompt_terms"] == ["Meghan"]

    exact = client.post("/dictionary", json={"written": "Saggar", "places": GLOBAL, "match_sound": False}).json()
    assert exact["match_sound"] is False
    assert dictionary.for_app(ZED).apply("Sagar") == "Sagar"


@pytest.mark.parametrize(
    ("letters", "heard", "written"),
    [
        ("MEGHAN", "Megan", "Meghan"),
        ("meghan", None, "Meghan"),
        ("MEGHAN.", "meghan", "Meghan"),
        ("NASA", "NASA", "NASA"),
        ("IPHONE", "iPhone", "iPhone"),
        ("McKAY", None, "McKAY"),
        ("MRGNHNT96", None, "MRGNHNT96"),
        ("mrgnhnt96", None, "mrgnhnt96"),
    ],
)
def test_a_spelled_word_is_written_as_a_name(letters, heard, written):
    assert dictionary.spelled_word(letters, heard) == written


def test_a_spelled_fix_adds_a_word_that_never_respells_others(storage):
    dictionary.for_app(ZED)
    with storage() as db:
        added = dictionary.add_spelled_word("MEGHAN", ZED, "Megan", db)

    assert (added.written, added.spoken, added.match_sound, added.source) == ("Meghan", None, False, "spoken_fix")
    assert [(p.scope, p.scope_id) for p in added.places] == [("global", None)]
    # The caches were cleared: dictation uses it at once, in every app.
    for app in (ZED, SLACK):
        found = dictionary.for_app(app)
        assert found.terms == ("Meghan",)
        assert found.apply("meghan met Megan and Meagan") == "Meghan met Megan and Meagan"


def test_a_spelled_fix_twice_adds_the_word_once(storage, client):
    with storage() as db:
        first = dictionary.add_spelled_word("MEGHAN", ZED, db=db)
    with storage() as db:
        again = dictionary.add_spelled_word("meghan", SLACK, "Meghan", db)

    assert again.id == first.id
    [listed] = client.get("/dictionary").json()["entries"]
    assert (listed["written"], listed["match_sound"], listed["source"]) == ("Meghan", False, "spoken_fix")


def test_a_spelled_fix_never_changes_the_users_entry(storage, client):
    mine = add(client, "Meghan").json()
    zed = {"scope": "app", "scope_id": ZED, "app_name": "Zed"}
    theirs = add(client, "Saggar", "sagar", [zed]).json()

    with storage() as db:
        kept = dictionary.add_spelled_word("MEGHAN", ZED, db=db)
        # Said "sagar" everywhere is taken only in Zed: everywhere gets the word.
        elsewhere = dictionary.add_spelled_word("SAGAR", SLACK, db=db)
        # In Zed, "sagar" already writes something else; that stays.
        assert dictionary.add_spelled_word("SAGAR", ZED, db=db) is None

    assert kept.id == mine["id"]
    assert (kept.match_sound, kept.source) == (True, "user")
    assert elsewhere.written == "Sagar"
    assert dictionary.for_app(ZED).apply("sagar met Megan") == "Saggar met Meghan"
    entries = {e["id"]: e for e in client.get("/dictionary").json()["entries"]}
    assert (entries[mine["id"]]["match_sound"], entries[theirs["id"]]["spoken"]) == (True, "sagar")


def test_a_spelled_fix_an_entry_already_said_that_way_is_left_alone(storage, client):
    add(client, "Megan", "meghan")

    with storage() as db:
        assert dictionary.add_spelled_word("MEGHAN", ZED, db=db) is None

    assert [e["written"] for e in client.get("/dictionary").json()["entries"]] == ["Megan"]


def test_editing_a_spelled_fix_makes_it_the_users(storage, client):
    with storage() as db:
        added = dictionary.add_spelled_word("MEGHAN", db=db)

    edited = client.patch(f"/dictionary/{added.id}", json={"match_sound": True}).json()

    assert (edited["match_sound"], edited["source"]) == (True, "user")


def test_a_spelled_fix_without_a_word_or_database_adds_nothing(storage, monkeypatch):
    with storage() as db:
        assert dictionary.add_spelled_word("  . ", ZED, db=db) is None
    monkeypatch.setattr(database_session, "SessionLocal", None)
    assert dictionary.add_spelled_word("MEGHAN", ZED) is None


def test_a_spelled_fix_opens_its_own_session(storage):
    added = dictionary.add_spelled_word("MEGHAN", ZED)

    with storage() as db:
        assert [g.id for g in dictionary.list_groups(db)] == [added.id]


def test_entries_from_before_groups_are_their_own_entry(storage):
    from backend.database.models import DictionaryEntry

    with storage() as db:
        db.add(DictionaryEntry(id="old", scope="global", scope_id="", written="Zed", key="zed"))
        db.commit()
        [group] = dictionary.list_groups(db)
        assert group.id == "old"
        dictionary.update_group(db, "old", {"places": [{"scope": "app", "scope_id": ZED}]})
        assert [(p.scope, p.scope_id) for p in dictionary.list_groups(db)[0].places] == [("app", ZED)]


def test_deleting_a_style_moves_its_entries_to_the_default(storage):
    with storage() as db:
        code = styles.create_style(db, "Code")
        default = styles.default_id()
        dictionary.add_group(db, "Kubernetes", None, [{"scope": "style", "scope_id": code.id}])
        dictionary.add_group(db, "Zed", None, [{"scope": "style", "scope_id": code.id}])
        dictionary.add_group(db, "zed", None, [{"scope": "style", "scope_id": default}])
        styles.delete_style(db, code.id)

        moved = dictionary.list_entries(db)

    assert sorted((e.scope_id == default, e.written) for e in moved) == [(True, "Kubernetes"), (True, "zed")]


def test_the_migration_adds_groups_to_an_existing_dictionary():
    from sqlalchemy import inspect, text

    from backend.database.migrations import run_migrations

    engine = create_engine("sqlite://", connect_args={"check_same_thread": False}, poolclass=StaticPool)
    with engine.connect() as conn:
        conn.execute(
            text(
                "CREATE TABLE dictionary_entries (id VARCHAR PRIMARY KEY, scope VARCHAR NOT NULL, "
                "scope_id VARCHAR NOT NULL, app_name VARCHAR, written VARCHAR NOT NULL, spoken VARCHAR, "
                "key VARCHAR NOT NULL, created_at DATETIME)"
            )
        )
        conn.execute(
            text("INSERT INTO dictionary_entries VALUES ('old', 'global', '', NULL, 'Zed', NULL, 'zed', NULL)")
        )
        conn.commit()

    run_migrations(engine)
    run_migrations(engine)

    columns = {c["name"] for c in inspect(engine).get_columns("dictionary_entries")}
    assert {"group_id", "match_sound", "source", "phrase"} <= columns
    with engine.connect() as conn:
        # An existing entry keeps matching by sound, is the user's, and is a word.
        assert conn.execute(text("SELECT id, group_id, match_sound, source, phrase FROM dictionary_entries")).all() == [
            ("old", None, 1, None, 0)
        ]
    make = sessionmaker(bind=engine)
    with make() as db:
        [group] = dictionary.list_groups(db)
    assert (group.match_sound, group.source, group.phrase) == (True, "user", False)


def test_the_app_name_is_in_a_new_dictionary_once(storage, tmp_path):
    marker = tmp_path / "dictionary-defaulted"
    with storage() as db:
        dictionary.ensure_defaults(db, marker)
        [group] = dictionary.list_groups(db)
        assert (group.written, group.spoken, group.places) == ("Kass", None, (dictionary.Place("global"),))
        assert dictionary.for_app(ZED).apply("Ask Cass to fix it") == "Ask Kass to fix it"

        # Deleted, it stays deleted.
        dictionary.delete_group(db, group.id)
        dictionary.ensure_defaults(db, marker)
        assert dictionary.list_groups(db) == []


def test_an_existing_entry_for_the_app_name_is_left_alone(storage, tmp_path):
    with storage() as db:
        dictionary.add_group(db, "Kass", "Cass", [{"scope": "global"}])
        dictionary.ensure_defaults(db, tmp_path / "dictionary-defaulted")
        [group] = dictionary.list_groups(db)
        assert group.spoken == "Cass"


def test_a_word_already_said_as_the_app_name_keeps_startup_working(storage, tmp_path):
    with storage() as db:
        dictionary.add_group(db, "KASS-2", "kass", [{"scope": "global"}])
        dictionary.ensure_defaults(db, tmp_path / "dictionary-defaulted")
        assert [group.written for group in dictionary.list_groups(db)] == ["KASS-2"]
