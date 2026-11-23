//! What cleanup in a style reads, from a real kass.db and the worker's JSON
//! files. Ported from backend/tests: test_spoken_style, test_writing_styles,
//! test_personal_examples and test_correction_notes.

use std::path::PathBuf;

use kass_server::cleanup_inputs::{self, load, PersonalExample, MAX_PROMPT_EXAMPLES};
use kass_server::styles::{self, spoken_style, Snapshot, Style};
use kass_text::refinement::{Flags, Punctuation};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const SLACK: &str = "com.tinyspeck.slackmacgap";
const MAIL: &str = "com.apple.mail";

fn style(id: &str, name: &str, position: i64, punctuation: &str) -> Style {
    Style {
        id: id.into(),
        name: name.into(),
        position,
        is_default: position == 0,
        punctuation_style: punctuation.into(),
        preserve_technical: true,
        description: String::new(),
    }
}

// -- spoken_style ----------------------------------------------------------------

fn spoken_styles() -> Snapshot {
    Snapshot {
        styles: vec![
            style("personal", "Personal", 0, "standard"),
            style("formal", "Formal", 1, "standard"),
            style("work-email", "Work Email", 2, "standard"),
            style("work", "Work", 3, "standard"),
            style("chat", "Chat mode", 4, "casual"),
            style("casual", "Casual", 5, "casual"),
        ],
        apps: Vec::new(),
        app_names: Default::default(),
        teaching: None,
    }
}

#[test]
fn the_style_named_at_the_start_is_found_and_dropped() {
    let cases: &[(&str, Option<&str>, &str)] = &[
        (
            "Use formal mode. Dear team, the build is green.",
            Some("formal"),
            "Dear team, the build is green.",
        ),
        ("use formal mode, dear team", Some("formal"), "Dear team"),
        ("Use Formal Mode: Dear team", Some("formal"), "Dear team"),
        (
            "Use formal-mode. iPhone sales are up.",
            Some("formal"),
            "iPhone sales are up.",
        ),
        ("Use the formal style. Hello.", Some("formal"), "Hello."),
        ("Use formal mode.", Some("formal"), ""),
        (
            "Use work email mode. Hi Sam.",
            Some("work-email"),
            "Hi Sam.",
        ),
        ("Use work mode. Hi Sam.", Some("work"), "Hi Sam."),
        // A name ending in "mode" isn't said twice.
        ("Use chat mode. lol", Some("chat"), "Lol"),
        // Other ways to ask, each standing alone before the text.
        ("Formal mode. Dear team.", Some("formal"), "Dear team."),
        ("In formal mode: dear team.", Some("formal"), "Dear team."),
        ("Switch to formal. Dear team.", Some("formal"), "Dear team."),
        (
            "Change over to the work email style. Hi Sam.",
            Some("work-email"),
            "Hi Sam.",
        ),
        ("Make this formal. Dear team.", Some("formal"), "Dear team."),
        (
            "Make it more personal, hey mom.",
            Some("personal"),
            "Hey mom.",
        ),
        (
            "Let's make this sound a bit more formal. Dear team.",
            Some("formal"),
            "Dear team.",
        ),
        ("Write this more formally", None, ""),
        (
            "I want this to be more personal. Hey mom.",
            Some("personal"),
            "Hey mom.",
        ),
        (
            "I'd like this to sound more formal. Dear team.",
            Some("formal"),
            "Dear team.",
        ),
        (
            "I would like it to be a little more casual.",
            Some("casual"),
            "",
        ),
        (
            "Okay, um, make this formal. Dear team.",
            Some("formal"),
            "Dear team.",
        ),
        // Words for the same register pick the style named with another.
        (
            "Make this more professional. Dear team.",
            Some("formal"),
            "Dear team.",
        ),
        (
            "I want this to be more informal. Hey.",
            Some("casual"),
            "Hey.",
        ),
        ("Relaxed mode. Hey.", Some("casual"), "Hey."),
    ];
    let styles = spoken_styles();
    for (said, expected, rest) in cases {
        let (found, remaining) = spoken_style(said, &styles);
        match expected {
            None => assert_eq!((found, remaining.as_str()), (None, *said), "{said}"),
            Some(id) => {
                assert_eq!(found.map(|style| style.id.as_str()), Some(*id), "{said}");
                assert_eq!(remaining, *rest, "{said}");
            }
        }
    }
}

#[test]
fn other_text_is_left_alone() {
    let styles = spoken_styles();
    for said in [
        "Remember to use formal mode for this.",
        "Use formal language here.",
        "Use funny mode. Hi.",
        "Use formal modest wording.",
        "Use personal model",
        // Looser lead-ins must stand alone; these open real sentences.
        "Formal mode is off by default.",
        "Make this formal letter shorter.",
        "I want this to be more personal than last year's card.",
        "Make it less formal.",
        "I want this to be more formal-looking, like a contract.",
        "Keep it casual.",
        "Switch to formal wear tonight.",
    ] {
        let (found, remaining) = spoken_style(said, &styles);
        assert!(found.is_none(), "{said}");
        assert_eq!(remaining, said);
    }
}

// -- a kass.db and its data folder -----------------------------------------------

struct Kass {
    dir: PathBuf,
    conn: Connection,
}

impl Kass {
    /// Personal (the default) and Chat, with Slack in Chat.
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kass-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = Connection::open(dir.join("kass.db")).unwrap();
        conn.execute_batch(include_str!("kass_schema.sql")).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
                 capitalize_first, smart_cleanup, preserve_technical, description)
             VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1, NULL),
                    ('chat', 'Chat', 1, 0, 'casual', 1, 1, 0,
                     '  Mostly coworkers, so informal. No greetings.  ');
             INSERT INTO app_styles (bundle_id, app_name, style_id) VALUES ('{SLACK}', 'Slack', 'chat');"
        ))
        .unwrap();
        Kass { dir, conn }
    }

    fn styles(&self) -> Snapshot {
        styles::load(&self.conn, None).unwrap()
    }

    fn write(&self, name: &str, value: Value) {
        std::fs::write(self.dir.join(name), value.to_string()).unwrap();
    }

    /// A report on a capture in `app` correcting `said` to `meant`.
    #[allow(clippy::too_many_arguments)]
    fn correct_with(
        &self,
        id: &str,
        capture: &str,
        app: Option<&str>,
        said: &str,
        meant: &str,
        target: &str,
        source: &str,
        at: &str,
    ) {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO captures (id, audio_path, source, transcript_raw, app_bundle_id,
                     audio_deleted, created_at)
                 VALUES (?1, 'a.wav', 'dictation', ?2, ?3, 0, ?4)",
                params![capture, said, app, at],
            )
            .unwrap();
        let snapshot = json!({
            "transcript_raw": said,
            "transcript_refined": said,
            "app_bundle_id": app,
            "app_name": app.map(|_| "App"),
        });
        self.conn
            .execute(
                "INSERT INTO capture_feedback (id, capture_id, target, expected_text, notes, snapshot,
                     source, created_at)
                 VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7)",
                params![id, capture, target, meant, snapshot.to_string(), source, at],
            )
            .unwrap();
    }

    fn correct(&self, id: &str, app: Option<&str>, said: &str, meant: &str, at: &str) {
        self.correct_with(id, id, app, said, meant, "refined", "manual", at);
    }

    fn examples(&self, style_id: &str) -> Vec<PersonalExample> {
        let writing = cleanup_inputs::read_writing_style(&self.dir);
        cleanup_inputs::all_examples(&self.conn, &writing, &self.styles(), style_id).unwrap()
    }

    fn for_prompt(&self, style_id: &str) -> Vec<(String, String)> {
        load(&self.conn, &self.dir, &self.styles(), Some(style_id), true)
            .unwrap()
            .personal
    }
}

impl Drop for Kass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `words` different words, so it isn't taken for one of Whisper's loops.
fn prose(words: usize) -> String {
    (0..words)
        .map(|index| format!("w{index}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pair(said: &str, meant: &str) -> (String, String) {
    (said.into(), meant.into())
}

// -- the style -------------------------------------------------------------------

#[test]
fn flags_come_from_the_style_and_self_correction_from_settings() {
    let kass = Kass::new();
    let inputs = load(&kass.conn, &kass.dir, &kass.styles(), Some("chat"), false).unwrap();
    // Filler removal and a capitalized first word are no longer settings.
    assert_eq!(
        inputs.flags,
        Flags {
            self_correction: false,
            preserve_technical: false,
            punctuation: Punctuation::Casual,
            style: Some("chat".into()),
            ..Flags::default()
        }
    );
    assert_eq!(inputs.style.id, "chat");
}

#[test]
fn a_style_description_goes_only_with_its_own_style() {
    let kass = Kass::new();
    let styles = kass.styles();
    let chat = load(&kass.conn, &kass.dir, &styles, Some("chat"), true).unwrap();
    assert_eq!(
        chat.description,
        "Mostly coworkers, so informal. No greetings."
    );
    let personal = load(&kass.conn, &kass.dir, &styles, Some("personal"), true).unwrap();
    assert_eq!(personal.description, "");
}

#[test]
fn none_or_a_deleted_style_is_the_default() {
    let kass = Kass::new();
    let styles = kass.styles();
    for asked in [None, Some("gone")] {
        let inputs = load(&kass.conn, &kass.dir, &styles, asked, true).unwrap();
        assert_eq!(inputs.style.id, "personal");
        assert_eq!(inputs.flags.style.as_deref(), Some("personal"));
    }
}

#[test]
fn nothing_learned_yet_reads_as_empty() {
    let kass = Kass::new();
    let inputs = load(
        &kass.conn,
        &kass.dir,
        &kass.styles(),
        Some("personal"),
        true,
    )
    .unwrap();
    assert!(!inputs.profile.is_ready());
    assert!(inputs.personal.is_empty());
    assert_eq!(inputs.notes, None);
    assert!(inputs.rules.is_empty());
    assert_eq!(inputs.punctuation, Default::default());
}

// -- writing-style.json ----------------------------------------------------------

#[test]
fn each_style_reads_its_own_profile() {
    let kass = Kass::new();
    kass.write(
        "writing-style.json",
        json!({
            "version": 2,
            "styles": {"chat": {
                "runs": 0,
                "examples": [],
                "feedback_counts": {"boundary": {"period": 0, "comma": 3, "none": 0}},
            }},
            "hidden_examples": [],
        }),
    );
    let styles = kass.styles();
    let chat = load(&kass.conn, &kass.dir, &styles, Some("chat"), true).unwrap();
    assert!(chat.profile.is_ready());
    assert!(chat
        .profile
        .prompt_section()
        .unwrap()
        .contains("Join related thoughts with commas"));
    assert_eq!(chat.profile.apply_learned("Hi. There."), "Hi, there.");
    let personal = load(&kass.conn, &kass.dir, &styles, Some("personal"), true).unwrap();
    assert!(!personal.profile.is_ready());
}

#[test]
fn version_1_profiles_and_notes_move_into_personal() {
    let kass = Kass::new();
    kass.write(
        "writing-style.json",
        json!({
            "version": 1,
            "runs": 2,
            "last_run_at": "2026-09-21T21:38:19+00:00",
            "examples": [{"shown": "Hi. There.", "written": "hi, there", "created_at": "x"}],
            "recent_paragraphs": ["a"],
            "feedback_counts": {"boundary": {"period": 0, "comma": 3, "none": 0}},
            "hidden_examples": ["correction:gone"],
        }),
    );
    kass.write(
        "correction-notes.json",
        json!({"version": 1, "notes": ["Write Voicebox as one word."], "considered_ids": ["c1"]}),
    );
    let writing = cleanup_inputs::read_writing_style(&kass.dir);
    assert_eq!(cleanup_inputs::profile(&writing, "personal").runs, 2);
    assert_eq!(cleanup_inputs::profile(&writing, "chat").runs, 0);
    assert_eq!(
        cleanup_inputs::hidden_examples(&writing),
        ["correction:gone"]
    );
    assert_eq!(
        cleanup_inputs::correction_notes(&kass.dir, "personal"),
        ["Write Voicebox as one word."]
    );
    assert!(cleanup_inputs::correction_notes(&kass.dir, "chat").is_empty());
}

#[test]
fn an_unreadable_profile_starts_fresh() {
    let kass = Kass::new();
    std::fs::write(kass.dir.join("writing-style.json"), "{not json").unwrap();
    let inputs = load(&kass.conn, &kass.dir, &kass.styles(), None, true).unwrap();
    assert!(!inputs.profile.is_ready());
}

// -- personal examples -----------------------------------------------------------

const AT: &str = "2026-11-01 10:00:00.000000";

#[test]
fn refined_corrections_become_examples() {
    let kass = Kass::new();
    kass.correct(
        "a",
        None,
        "so the release we need to push the release to friday",
        "We need to push the release to Friday.",
        AT,
    );
    kass.correct_with("b", "b", None, "teh cat", "the cat", "raw", "manual", AT);
    let examples = kass.examples("personal");
    assert_eq!(
        examples
            .iter()
            .map(|e| (e.id.as_str(), e.source, e.said.as_str(), e.meant.as_str()))
            .collect::<Vec<_>>(),
        [(
            "correction:a",
            "correction",
            "so the release we need to push the release to friday",
            "We need to push the release to Friday."
        )]
    );
    assert_eq!(
        examples[0].created_at.as_deref(),
        Some("2026-11-01T10:00:00")
    );
}

#[test]
fn only_explicit_reports_teach_the_style() {
    let kass = Kass::new();
    kass.correct_with(
        "v",
        "v",
        None,
        "meet megan at noon",
        "Meet Morgan at noon.",
        "refined",
        "voice_fix",
        AT,
    );
    kass.correct_with(
        "r",
        "r",
        None,
        "call megan at noon",
        "Call Rosalind at noon.",
        "refined",
        "redictation",
        AT,
    );
    let meant: Vec<String> = kass
        .examples("personal")
        .into_iter()
        .map(|e| e.meant)
        .collect();
    assert_eq!(meant, ["Meet Morgan at noon."]);
}

#[test]
fn the_latest_correction_of_a_capture_replaces_earlier_ones() {
    let kass = Kass::new();
    kass.correct_with(
        "one",
        "c",
        None,
        "ship it friday",
        "Ship it Friday.",
        "refined",
        "manual",
        AT,
    );
    kass.correct_with(
        "two",
        "c",
        None,
        "ship it friday",
        "Ship it on Friday.",
        "refined",
        "manual",
        "2026-11-01 10:05:00.250000",
    );
    let examples = kass.examples("personal");
    assert_eq!(examples.len(), 1);
    assert_eq!(examples[0].meant, "Ship it on Friday.");
    assert_eq!(
        examples[0].created_at.as_deref(),
        Some("2026-11-01T10:05:00.250000")
    );
}

#[test]
fn every_dictation_gets_the_same_examples_oldest_first() {
    let kass = Kass::new();
    kass.correct(
        "a",
        None,
        "lunch plans for thursday with the team",
        "Lunch with the team on Thursday.",
        "2026-11-01 10:00:00.000000",
    );
    kass.correct(
        "b",
        None,
        "push the release to friday the release",
        "Push the release to Friday.",
        "2026-11-01 11:00:00.000000",
    );
    let first = kass.for_prompt("personal");
    assert_eq!(kass.for_prompt("personal"), first);
    assert_eq!(
        first
            .iter()
            .map(|(said, _)| said.as_str())
            .collect::<Vec<_>>(),
        [
            "lunch plans for thursday with the team",
            "push the release to friday the release"
        ]
    );
    // A new correction goes after the existing ones.
    kass.correct(
        "c",
        None,
        "the dog needs a walk",
        "The dog needs a walk.",
        "2026-11-01 12:00:00.000000",
    );
    let after = kass.for_prompt("personal");
    assert_eq!(after[..first.len()], first[..]);
    assert_eq!(
        after.last(),
        Some(&pair("the dog needs a walk", "The dog needs a walk."))
    );
}

#[test]
fn only_the_most_recent_examples_fit() {
    let kass = Kass::new();
    for index in 0..MAX_PROMPT_EXAMPLES + 2 {
        kass.correct(
            &format!("id{index:02}"),
            None,
            &format!("example {index} said"),
            &format!("Example {index} meant."),
            &format!("2026-11-01 10:{index:02}:00.000000"),
        );
    }
    let said: Vec<String> = kass
        .for_prompt("personal")
        .into_iter()
        .map(|(said, _)| said)
        .collect();
    assert_eq!(said.len(), MAX_PROMPT_EXAMPLES);
    assert_eq!(said.first().unwrap(), "example 2 said");
    assert_eq!(
        said.last().unwrap(),
        &format!("example {} said", MAX_PROMPT_EXAMPLES + 1)
    );
}

#[test]
fn examples_past_the_prompt_budget_are_left_for_the_notes() {
    let kass = Kass::new();
    let long = prose(150);
    for index in 0..6 {
        kass.correct(
            &format!("id{index}"),
            None,
            &format!("{index} {long}"),
            &format!("{index} {long}!"),
            &format!("2026-11-01 10:0{index}:00.000000"),
        );
    }
    // Each is about 1,280 characters said and meant, so the newest four fit in 6,000.
    let said: Vec<String> = kass
        .for_prompt("personal")
        .into_iter()
        .map(|(said, _)| said)
        .collect();
    assert_eq!(said.len(), 4);
    assert!(said[0].starts_with("2 "));
}

#[test]
fn hidden_and_too_long_examples_are_left_out() {
    let kass = Kass::new();
    kass.correct(
        "a",
        None,
        "so we should we should ship it",
        "We should ship it.",
        AT,
    );
    kass.correct("b", None, &prose(200), "Short.", AT);
    kass.correct("c", None, "Same text.", "Same text.", AT);
    assert_eq!(kass.examples("personal").len(), 1);
    kass.write(
        "writing-style.json",
        json!({"version": 2, "styles": {}, "hidden_examples": ["correction:a"]}),
    );
    assert!(kass.examples("personal").is_empty());
}

#[test]
fn corrections_teach_only_their_apps_style() {
    let kass = Kass::new();
    kass.correct(
        "s",
        Some(SLACK),
        "yeah so the build is broken",
        "yeah the build is broken",
        AT,
    );
    kass.correct(
        "m",
        Some(MAIL),
        "hi priya the build is broken",
        "Hi Priya, the build is broken.",
        AT,
    );
    kass.correct(
        "u",
        None,
        "old one from before apps",
        "Old one from before apps.",
        AT,
    );
    let said = |style: &str| -> Vec<String> {
        let mut said: Vec<String> = kass.examples(style).into_iter().map(|e| e.said).collect();
        said.sort();
        said
    };
    assert_eq!(said("chat"), ["yeah so the build is broken"]);
    assert_eq!(
        said("personal"),
        ["hi priya the build is broken", "old one from before apps"]
    );
    assert_eq!(
        kass.examples("chat")[0].app_bundle_id.as_deref(),
        Some(SLACK)
    );
}

#[test]
fn corrections_left_with_a_style_keep_teaching_it() {
    let kass = Kass::new();
    kass.correct(
        "s",
        Some(SLACK),
        "yeah so the build is broken",
        "yeah the build is broken",
        AT,
    );
    kass.conn
        .execute(
            "UPDATE captures SET teaches_style_id = 'personal' WHERE id = 's'",
            [],
        )
        .unwrap();
    assert!(kass.examples("chat").is_empty());
    assert_eq!(kass.examples("personal").len(), 1);
    // A capture history retention deleted leaves the report's own copy.
    kass.conn
        .execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM captures WHERE id = 's';")
        .unwrap();
    kass.conn
        .execute(
            "UPDATE capture_feedback SET teaches_style_id = 'personal'",
            [],
        )
        .unwrap();
    assert_eq!(kass.examples("personal").len(), 1);
}

#[test]
fn taught_replies_that_differ_from_the_cleanup_are_examples() {
    let kass = Kass::new();
    kass.write(
        "writing-style.json",
        json!({
            "version": 2,
            "styles": {"personal": {"runs": 1, "examples": [
                {"said": "kept as shown", "shown": "Kept as shown.", "written": "Kept as shown.",
                 "created_at": "2026-10-01T09:00:00+00:00"},
                {"said": "voicebox cleanup", "shown": "Voicebox cleanup.", "written": "What I would send.",
                 "created_at": "2026-10-01T09:00:00+00:00"},
                {"said": null, "shown": "typed", "written": "typed"},
                {"shown": "Voicebox cleanup.", "written": "What I would send."},
            ]}},
            "hidden_examples": [],
        }),
    );
    let examples = kass.examples("personal");
    assert_eq!(
        examples
            .iter()
            .map(|e| (e.id.as_str(), e.source, e.said.as_str(), e.meant.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                "calibration:776010845f9c047a",
                "calibration",
                "voicebox cleanup",
                "What I would send."
            ),
            // A punctuation paragraph has no said; it is the shown text as spoken.
            (
                "calibration:ed8241c917e0b498",
                "calibration",
                "voicebox cleanup",
                "What I would send."
            ),
        ]
    );
    assert!(kass.examples("chat").is_empty());
}

#[test]
fn calibration_and_corrections_are_ordered_newest_first() {
    let kass = Kass::new();
    kass.correct(
        "a",
        None,
        "older correction",
        "Older correction.",
        "2026-09-01 10:00:00.000000",
    );
    kass.correct(
        "b",
        None,
        "newer correction",
        "Newer correction.",
        "2026-11-01 10:00:00.000000",
    );
    kass.write(
        "writing-style.json",
        json!({
            "version": 2,
            "styles": {"personal": {"runs": 1, "examples": [
                {"said": "calibration said", "shown": "Calibration said.", "written": "What I meant.",
                 "created_at": "2026-10-01T09:00:00+00:00"},
            ]}},
            "hidden_examples": [],
        }),
    );
    let said: Vec<String> = kass
        .examples("personal")
        .into_iter()
        .map(|e| e.said)
        .collect();
    assert_eq!(
        said,
        ["newer correction", "calibration said", "older correction"]
    );
}

// -- correction notes ------------------------------------------------------------

const RULE: &str = "Write Kass as one word.";

#[test]
fn notes_go_in_the_prompt_for_their_style() {
    let kass = Kass::new();
    kass.write(
        "correction-notes.json",
        json!({"version": 2, "styles": {
            "personal": {"notes": [RULE, "Drop 'basically'."], "considered_ids": []},
        }}),
    );
    let styles = kass.styles();
    let personal = load(&kass.conn, &kass.dir, &styles, Some("personal"), true).unwrap();
    assert_eq!(
        personal.notes.as_deref(),
        Some(
            "Rules this speaker taught in earlier corrections. Follow them:\n\
             - Write Kass as one word.\n- Drop 'basically'."
        )
    );
    let chat = load(&kass.conn, &kass.dir, &styles, Some("chat"), true).unwrap();
    assert_eq!(chat.notes, None);
}

#[test]
fn unsupported_notes_are_left_out() {
    let kass = Kass::new();
    kass.write(
        "correction-notes.json",
        json!({"version": 3, "notes": [RULE]}),
    );
    assert!(cleanup_inputs::correction_notes(&kass.dir, "personal").is_empty());
}

// -- correction learning ---------------------------------------------------------

fn learning(version: i64, rules: Value) -> Value {
    json!({
        "version": version,
        "revision": 1,
        "rules": rules,
        "punctuation": {"aliases": {"full stop": "."}, "kept": ["trial period"]},
    })
}

fn rule() -> Value {
    // A word on each side anchors the misheard one.
    json!({"id": "r1", "source": ["the", "cass", "app"], "replacement": ["Kass"], "language": null})
}

#[test]
fn learned_rules_and_punctuation_words_are_read() {
    let kass = Kass::new();
    kass.write("correction-learning.json", learning(1, json!([rule()])));
    let inputs = load(&kass.conn, &kass.dir, &kass.styles(), None, true).unwrap();
    assert!(!inputs.rules.is_empty());
    assert_eq!(
        inputs.rules.apply("open the cass app now", None),
        "open the Kass app now"
    );
    assert_eq!(
        inputs
            .punctuation
            .aliases
            .get("full stop")
            .map(String::as_str),
        Some(".")
    );
    assert!(inputs.punctuation.kept.contains("trial period"));
}

#[test]
fn a_state_python_would_refuse_gives_nothing() {
    let kass = Kass::new();
    for state in [
        learning(2, json!([rule()])),
        learning(1, Value::Array(vec![rule(); 33])),
        learning(1, json!([{"id": "bad"}])),
    ] {
        kass.write("correction-learning.json", state);
        let (rules, punctuation) = cleanup_inputs::learned_corrections(&kass.dir);
        assert!(rules.is_empty());
        assert_eq!(punctuation, Default::default());
    }
}

/// Building from reads taken once gives every style what loading it alone does.
#[test]
fn assembling_from_one_read_matches_loading_each_style() {
    let kass = Kass::new();
    kass.write(
        "writing-style.json",
        json!({"version": 2, "styles": {"chat": {"runs": 0, "examples": []}}}),
    );
    kass.write(
        "correction-notes.json",
        json!({"version": 2, "styles": {"chat": {"notes": ["Keep it short."], "considered_ids": []}}}),
    );
    kass.correct(
        "s",
        Some(SLACK),
        "yeah so its broken",
        "yeah it's broken",
        AT,
    );
    kass.correct("p", None, "the build is green", "The build is green.", AT);
    let styles = kass.styles();
    let files = cleanup_inputs::LearnedFiles::read(&kass.dir);
    let corrections = cleanup_inputs::CorrectionRows::read(&kass.conn)
        .unwrap()
        .examples(&styles);
    for style in [None, Some("personal"), Some("chat"), Some("deleted")] {
        let loaded = load(&kass.conn, &kass.dir, &styles, style, true).unwrap();
        let assembled = cleanup_inputs::assemble(&files, &styles, style, true, &corrections);
        assert_eq!(assembled.style, loaded.style, "{style:?}");
        assert_eq!(assembled.description, loaded.description, "{style:?}");
        assert_eq!(assembled.personal, loaded.personal, "{style:?}");
        assert_eq!(assembled.notes, loaded.notes, "{style:?}");
    }
}
