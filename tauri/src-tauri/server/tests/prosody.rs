//! Expressive dictation in Rust against Python's.
//!
//! `prosody/python.json` is what Python's `prosody.py` and
//! `expression_learning.py` made of takes built from the parity recordings
//! (`scripts/parity/audio`), with Whisper's word alignments, recorded before
//! that code was removed. Played into `stream::prosody` the same way, the
//! takes must give the same voice, words, measurements and written text,
//! and the made-up edits the same learned rules.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use kass_server::stream::prosody::learning::{gather, learn, Learned, Saved, SavedCapture};
use kass_server::stream::prosody::pitch::{Frames, PitchTrack};
use kass_server::stream::prosody::store::{load_baseline, load_learned};
use kass_server::stream::prosody::voice::{
    exclaim, measure_voice, measure_written, semitones, stretch, written_sentences, Baseline,
    Statement, StretchRule, Voice,
};
use kass_server::stream::prosody::{Expression, Word};
use rusqlite::{params, Connection};
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn golden() -> &'static Value {
    static GOLDEN: OnceLock<Value> = OnceLock::new();
    GOLDEN.get_or_init(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/prosody/python.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    })
}

fn clip(name: &str) -> Vec<i16> {
    let mut reader =
        hound::WavReader::open(repo().join("scripts/parity/audio").join(name)).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (spec.sample_rate, spec.channels, spec.bits_per_sample),
        (16_000, 1, 16)
    );
    reader.samples::<i16>().map(Result::unwrap).collect()
}

/// Clips (a file name) and silences (seconds) one after another, at 16 kHz.
fn take(parts: &[Value]) -> Vec<i16> {
    parts
        .iter()
        .flat_map(|part| match part {
            Value::String(name) => clip(name),
            seconds => vec![0; (seconds.as_f64().unwrap() * 16_000.0).round_ties_even() as usize],
        })
        .collect()
}

/// 16 kHz audio held sample by sample at `rate`, as the recording did.
fn at_rate(audio: &[i16], rate: i64) -> Vec<i16> {
    let count = audio.len() as i64 * rate / 16_000;
    (0..count)
        .map(|i| audio[(i * 16_000 / rate) as usize])
        .collect()
}

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(f64::NAN)
}

fn numbers(value: &Value) -> Vec<f64> {
    value.as_array().unwrap().iter().map(number).collect()
}

fn words(value: &Value) -> Vec<Word> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|word| Word {
            text: word[0].as_str().unwrap().to_string(),
            start: number(&word[1]),
            end: number(&word[2]),
        })
        .collect()
}

fn pair(value: &Value) -> (f64, f64) {
    (number(&value[0]), number(&value[1]))
}

fn baseline_of(value: &Value) -> Baseline {
    Baseline {
        level: pair(&value["level"]),
        span: pair(&value["span"]),
        loudness: pair(&value["loudness"]),
    }
}

fn rule_of(value: &Value) -> StretchRule {
    StretchRule {
        pace: number(&value["pace"]),
        louder_db: number(&value["louder_db"]),
        plateau: pair(&value["plateau"]),
        words: value["words"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>(),
    }
}

fn learned_json(learned: &Learned) -> Value {
    let rule = &learned.rule;
    serde_json::json!({
        "cutoff": learned.cutoff,
        "pace": rule.pace,
        "louder_db": rule.louder_db,
        "plateau": [rule.plateau.0, rule.plateau.1],
        "words": rule.words.iter().collect::<Vec<_>>(),
    })
}

fn same(a: f64, b: f64, tolerance: f64) -> bool {
    (a.is_nan() && b.is_nan()) || (a - b).abs() <= tolerance
}

/// Where `got` and `want` differ: numbers within `tolerance`, everything
/// else exactly, keys in the same order.
fn differences(got: &Value, want: &Value, path: &str, tolerance: f64, found: &mut Vec<String>) {
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => {
            if !same(a.as_f64().unwrap(), b.as_f64().unwrap(), tolerance) {
                found.push(format!("{path}: {a} for {b}"));
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (index, (a, b)) in a.iter().zip(b).enumerate() {
                differences(a, b, &format!("{path}[{index}]"), tolerance, found);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            let (keys_a, keys_b): (Vec<_>, Vec<_>) = (a.keys().collect(), b.keys().collect());
            if keys_a != keys_b {
                found.push(format!("{path}: keys {keys_a:?} for {keys_b:?}"));
                return;
            }
            for (key, value) in a {
                differences(value, &b[key], &format!("{path}.{key}"), tolerance, found);
            }
        }
        _ if got == want => {}
        _ => found.push(format!("{path}: {got} for {want}")),
    }
}

fn assert_same(got: &Value, want: &Value, path: &str) {
    let mut found = Vec::new();
    differences(got, want, path, 1e-9, &mut found);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// Python's frames for a take, fed to everything after the pitch tracker,
/// so each stage is checked on its own.
fn python_frames(case: &Value) -> Frames {
    Frames {
        hz: numbers(&case["hz"]),
        db: numbers(&case["db"]),
    }
}

/// The pitch tracker on the takes as the app sends them. Python correlates
/// by FFT and sums loudness in float32, so values agree to rounding; a frame
/// right at the voicing threshold may tip either way.
#[test]
fn tracks_pitch_as_python_does() {
    let mut frames_seen = 0;
    let mut flips = 0;
    let mut worst = (0f64, 0f64);
    for case in golden()["takes"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let rate = case["rate"].as_i64().unwrap();
        let audio = at_rate(&take(case["parts"].as_array().unwrap()), rate);
        let mut track = PitchTrack::new(rate as u32);
        for frame in audio.chunks(case["frame"].as_u64().unwrap() as usize) {
            track.feed(frame);
        }
        let want = python_frames(case);
        let got = track.frames();
        assert_eq!(got.hz.len(), want.hz.len(), "{name}: frames");
        for (index, (got_hz, want_hz)) in got.hz.iter().zip(&want.hz).enumerate() {
            frames_seen += 1;
            match (got_hz.is_nan(), want_hz.is_nan()) {
                (true, true) => {}
                (false, false) => {
                    let off = (got_hz - want_hz).abs() / want_hz;
                    assert!(
                        off < 1e-3,
                        "{name} frame {index}: {got_hz} Hz for {want_hz}"
                    );
                    worst.0 = worst.0.max(off);
                }
                _ => flips += 1,
            }
        }
        for (index, (got_db, want_db)) in got.db.iter().zip(&want.db).enumerate() {
            let off = (got_db - want_db).abs();
            assert!(
                off < 1e-3,
                "{name} frame {index}: {got_db} dB for {want_db}"
            );
            worst.1 = worst.1.max(off);
        }
    }
    assert!(
        flips * 1000 <= frames_seen,
        "{flips} of {frames_seen} frames voiced differently"
    );
    eprintln!("pitch: {frames_seen} frames, {flips} voicing flips, worst {worst:?}");
}

#[test]
fn places_phrase_words_as_python_does() {
    for case in golden()["takes"].as_array().unwrap() {
        let mut expression = Expression::new(case["rate"].as_u64().unwrap() as u32);
        for phrase in case["phrases"].as_array().unwrap() {
            expression.phrase(phrase["start"].as_u64().unwrap(), words(&phrase["words"]));
        }
        assert_eq!(
            expression.words(),
            words(&case["words"]),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn measures_the_voice_as_python_does() {
    let golden = golden();
    let baseline = baseline_of(&golden["baseline"]);
    let lax = rule_of(&golden["lax"]);
    for case in golden["takes"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let frames = python_frames(case);
        let want: Vec<f64> = numbers(&case["semitones"]);
        let got = semitones(&frames.hz);
        assert_eq!(got.len(), want.len(), "{name}");
        for (index, (a, b)) in got.iter().zip(&want).enumerate() {
            assert!(same(*a, *b, 1e-9), "{name} semitone {index}: {a} for {b}");
        }

        let voice = Voice::new(&frames, words(&case["words"]));
        let measured = measure_voice(&voice, Some(&baseline), &StretchRule::default());
        assert_same(
            &serde_json::to_value(&measured).unwrap(),
            &case["measured"],
            name,
        );
        let loose = measure_voice(&voice, None, &lax);
        assert_same(&serde_json::to_value(&loose).unwrap(), &case["lax"], name);
    }
}

#[test]
fn writes_the_text_as_python_does() {
    let golden = golden();
    let baseline = baseline_of(&golden["baseline"]);
    let cutoff = number(&golden["cutoff"]);
    let lax = rule_of(&golden["lax"]);
    let mut changed = 0;
    for case in golden["takes"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let voice = Voice::new(&python_frames(case), words(&case["words"]));
        let stretched = measure_voice(&voice, None, &lax).stretched;
        for texts in case["texts"].as_array().unwrap() {
            let text = texts["text"].as_str().unwrap();
            let written: Vec<Value> = written_sentences(text, &voice, Some(&baseline))
                .into_iter()
                .map(|sentence| {
                    serde_json::json!({
                        "text": sentence.text,
                        "end": text[..sentence.end].chars().count(),
                        "energy": sentence.energy,
                        "said_period": sentence.said_period,
                    })
                })
                .collect();
            assert_same(
                &Value::from(written),
                &texts["written"],
                &format!("{name}: {text}"),
            );
            let exclaimed = exclaim(text, &voice, Some(&baseline), cutoff);
            assert_eq!(exclaimed, texts["exclaimed"], "{name}");
            assert_eq!(
                stretch(text, &voice, &stretched),
                texts["stretched"],
                "{name}"
            );
            let expressed = stretch(&exclaimed, &voice, &stretched);
            assert_eq!(expressed, texts["expressed"], "{name}");
            changed += usize::from(expressed != text);
            let saved = measure_written(&voice, Some(&baseline), text);
            assert_same(
                &serde_json::to_value(&saved).unwrap(),
                &texts["saved"],
                name,
            );
        }
    }
    // The takes would check little if nothing were ever written differently.
    assert!(changed >= 10, "only {changed} texts changed");
}

#[test]
fn finds_the_baseline_as_python_does() {
    let golden = golden();
    let sentences: Vec<Statement> =
        serde_json::from_value(golden["baseline_sentences"].clone()).unwrap();
    assert_eq!(
        Baseline::of(&sentences),
        Some(baseline_of(&golden["baseline"]))
    );
    assert_eq!(Baseline::of(&sentences[..19]), None);
}

#[test]
fn learns_from_edits_as_python_does() {
    for case in golden()["learning"].as_array().unwrap() {
        let captures: Vec<SavedCapture> = case["captures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|capture| SavedCapture {
                id: capture[0].as_str().unwrap().to_string(),
                saved: serde_json::from_value::<Saved>(capture[1].clone()).unwrap(),
                delivered: capture[2].as_str().unwrap().to_string(),
            })
            .collect();
        let edits: HashMap<String, String> = serde_json::from_value(case["edits"].clone()).unwrap();
        let redictated: HashSet<String> =
            serde_json::from_value(case["redictated"].clone()).unwrap();
        let learned = learn(&gather(&captures, &edits, &redictated));
        assert_same(
            &learned_json(&learned),
            &case["learned"],
            &format!("seed {}", case["seed"]),
        );
    }
}

/// The two tables `load` and `load_baseline` read, with only their columns.
fn database(case: &Value) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE captures (id TEXT PRIMARY KEY, prosody TEXT, transcript_refined TEXT, created_at DATETIME);
         CREATE TABLE capture_feedback (id TEXT PRIMARY KEY, capture_id TEXT, target TEXT,
             expected_text TEXT, source TEXT, created_at DATETIME);",
    )
    .unwrap();
    for capture in case["captures"].as_array().unwrap() {
        conn.execute(
            "INSERT INTO captures (id, prosody, transcript_refined, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                capture["id"].as_str(),
                capture["prosody"].as_str(),
                capture["refined"].as_str(),
                capture["created_at"].as_str(),
            ],
        )
        .unwrap();
    }
    for report in case["reports"].as_array().unwrap() {
        conn.execute(
            "INSERT INTO capture_feedback (id, capture_id, target, expected_text, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                report["id"].as_str(),
                report["capture_id"].as_str(),
                report["target"].as_str(),
                report["expected_text"].as_str(),
                report["source"].as_str(),
                report["created_at"].as_str(),
            ],
        )
        .unwrap();
    }
    conn
}

#[test]
fn loads_from_the_database_as_python_does() {
    for case in golden()["database"].as_array().unwrap() {
        let seed = &case["seed"];
        let conn = database(case);
        let baseline = load_baseline(&conn).unwrap();
        let want = (!case["baseline"].is_null()).then(|| baseline_of(&case["baseline"]));
        assert_eq!(baseline, want, "seed {seed}");
        let learned = load_learned(&conn).unwrap();
        assert_same(
            &learned_json(&learned),
            &case["learned"],
            &format!("seed {seed}"),
        );
    }
}
