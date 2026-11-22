//! The Rust dictation stream's audio side against Python's.
//!
//! `stream_audio/python.json` is what Python's `capture_stream.py` did with
//! takes built from the parity recordings (`scripts/parity/audio`), with the
//! real Silero model, recorded before that code was removed. The same takes,
//! played frame by frame into `stream::intake`, must detect the same voice,
//! cut at the same pauses and send the same phrase windows to recognition.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use kass_server::stream::intake::{Intake, Next, Start, CONTINUOUS, FELL_BEHIND};
use kass_server::stream::speech::VoiceModel;
use serde_json::{json, Value};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn model() -> Arc<VoiceModel> {
    static MODEL: OnceLock<Arc<VoiceModel>> = OnceLock::new();
    MODEL
        .get_or_init(|| VoiceModel::load(&repo().join("backend/assets/silero_vad.onnx")).unwrap())
        .clone()
}

fn recording(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kass-stream-audio-{}-{name}.wav",
        std::process::id()
    ))
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
        .map(|k| audio[(k * 16_000 / rate) as usize])
        .collect()
}

fn frame(sequence: u32, offset: i64, pcm: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + pcm.len() * 2);
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    for sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn start(rate: i64, start_cue_ms: i64) -> Start {
    let message = json!({
        "type": "start", "protocol_version": 1, "sample_rate": rate,
        "channels": 1, "encoding": "pcm_s16le", "start_cue_ms": start_cue_ms,
    });
    Start::parse(&message, None, "base").unwrap()
}

/// Recognize every window the audio calls for, as Python's `_run` does
/// between frames. Recognition itself is stubbed out on both sides.
fn drain(intake: &mut Intake, finished: bool, windows: &mut Vec<Value>, full: &mut Vec<Value>) {
    loop {
        match intake.next(finished) {
            Next::Phrase(window) => {
                let heard = intake
                    .speech()
                    .heard(window.start, window.start + window.size);
                windows.push(json!({
                    "start": window.start, "size": window.size, "heard": heard, "paused": window.paused,
                }));
                intake.advance(&window);
            }
            Next::FullAudio => {
                let samples = intake.samples();
                full.push(json!({"samples": samples, "heard": intake.speech().heard(0, samples)}));
                intake.cover_all();
                return;
            }
            Next::Done | Next::Wait => return,
        }
    }
}

/// Play one recorded take into the Rust intake and record the same fields.
fn replay(case: &Value, index: usize) -> Value {
    let rate = case["rate"].as_i64().unwrap();
    let audio = at_rate(&take(case["parts"].as_array().unwrap()), rate);
    let uses_model = case["model"].as_bool().unwrap();
    let path = recording(&index.to_string());
    let mut intake = Intake::new(
        &start(rate, case["start_cue_ms"].as_i64().unwrap()),
        &path,
        uses_model.then(model),
    )
    .unwrap();
    let cues: Vec<(usize, Value, Value)> = case["cues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cue| {
            (
                cue[0].as_u64().unwrap() as usize,
                cue[1].clone(),
                cue[2].clone(),
            )
        })
        .collect();
    let size = case["frame"].as_u64().unwrap() as usize;
    let (mut windows, mut full, mut frames) = (Vec::new(), Vec::new(), Vec::new());
    for (sequence, pcm) in audio.chunks(size).enumerate() {
        intake
            .append(&frame(sequence as u32, intake.samples(), pcm))
            .unwrap();
        for (_, low, high) in cues.iter().filter(|(at, _, _)| *at == sequence) {
            intake.ignore_cue(low, high).unwrap();
        }
        drain(&mut intake, false, &mut windows, &mut full);
        let speech = intake.speech();
        frames.push(json!([
            speech.quiet(),
            speech.first_voice(),
            intake.last_cut()
        ]));
    }
    drain(&mut intake, true, &mut windows, &mut full);
    intake.close_recording().unwrap();
    let recorded = hound::WavReader::open(&path).unwrap().len() as usize;
    std::fs::remove_file(&path).unwrap();
    assert_eq!(recorded, audio.len(), "{}: recording length", case["name"]);
    let speech = intake.speech();
    json!({
        "samples": intake.samples(),
        "voiced": if uses_model { json!(speech.voiced()) } else { Value::Null },
        "analyzed": speech.analyzed(),
        "frames": frames,
        "windows": windows,
        "full_audio": full,
        "covered": intake.covered(),
        "offset": intake.offset(),
        "degraded_reason": intake.degraded_reason(),
    })
}

#[test]
fn matches_python_on_the_parity_audio() {
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("app/server/tests/stream_audio/python.json")).unwrap(),
    )
    .unwrap();
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 30);
    let mut mismatches = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let rust = replay(case, index);
        for (field, value) in rust.as_object().unwrap() {
            if &case[field] == value {
                continue;
            }
            let detail = match (field.as_str(), case[field].as_array(), value.as_array()) {
                ("frames" | "voiced", Some(python), Some(rust)) => {
                    let at = python
                        .iter()
                        .zip(rust)
                        .position(|(a, b)| a != b)
                        .unwrap_or(python.len().min(rust.len()));
                    format!(
                        "first differs at {at} of {}/{}: python {:?}, rust {:?}",
                        python.len(),
                        rust.len(),
                        python.get(at),
                        rust.get(at)
                    )
                }
                _ => format!("python {}, rust {value}", case[field]),
            };
            mismatches.push(format!("{}: {field}: {detail}", case["name"]));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} differences:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn start_messages_are_checked_like_python() {
    let base = json!({
        "type": "start", "protocol_version": 1, "sample_rate": 48000, "channels": 1, "encoding": "pcm_s16le",
    });
    let with = |field: &str, value: Value| {
        let mut message = base.clone();
        message[field] = value;
        Start::parse(&message, Some("en"), "base")
    };
    let start = Start::parse(&base, Some("en"), "turbo").unwrap();
    assert_eq!(start.sample_rate, 48_000);
    assert_eq!(start.source.as_str(), "dictation");
    assert_eq!(start.language.as_deref(), Some("en"));
    assert_eq!(start.stt_model, "turbo");
    assert!(!start.provisional);
    assert_eq!(start.start_cue_ms, 0);
    assert_eq!(
        Start::parse(&base, Some("auto"), "base").unwrap().language,
        None
    );
    assert_eq!(with("language", json!("auto")).unwrap().language, None);
    assert_eq!(
        with("language", json!("pt-BR"))
            .unwrap()
            .language
            .as_deref(),
        Some("pt-BR")
    );
    assert_eq!(
        with("stt_model", json!("small")).unwrap().stt_model,
        "small"
    );
    assert_eq!(with("stt_model", json!("")).unwrap().stt_model, "base");
    assert!(with("provisional", json!(true)).unwrap().provisional);
    assert_eq!(
        with("source", json!("command")).unwrap().source.as_str(),
        "command"
    );
    assert_eq!(with("start_cue_ms", json!(250)).unwrap().start_cue_ms, 250);

    let cases = [
        (
            "type",
            json!("stop"),
            "Expected protocol version 1 start message",
        ),
        (
            "protocol_version",
            json!(2),
            "Expected protocol version 1 start message",
        ),
        (
            "sample_rate",
            json!(8000),
            "sample_rate must be between 16000 and 48000",
        ),
        (
            "sample_rate",
            json!(48000.5),
            "sample_rate must be between 16000 and 48000",
        ),
        ("channels", json!(2), "Audio must be mono pcm_s16le"),
        ("encoding", json!("f32le"), "Audio must be mono pcm_s16le"),
        (
            "start_cue_ms",
            json!(1001),
            "start_cue_ms must be between 0 and 1000",
        ),
        (
            "start_cue_ms",
            json!(true),
            "start_cue_ms must be between 0 and 1000",
        ),
        ("source", json!("notes"), "Invalid streaming capture source"),
        ("language", json!("e"), "Invalid language"),
        ("language", json!("en_US"), "Invalid language"),
        ("stt_model", json!("huge"), "Invalid STT model"),
    ];
    for (field, value, error) in cases {
        assert_eq!(
            with(field, value.clone()).unwrap_err(),
            error,
            "{field} = {value}"
        );
    }
}

#[test]
fn frames_are_checked_and_recorded() {
    let path = recording("frames");
    let mut intake = Intake::new(&start(16_000, 0), &path, None).unwrap();
    let pcm: Vec<i16> = (0..1600).map(|k| (k * 7 - 5000) as i16).collect();
    assert_eq!(
        intake.append(&[0; 9]).unwrap_err(),
        "Invalid PCM frame size"
    );
    assert_eq!(
        intake.append(&[0; 11]).unwrap_err(),
        "Invalid PCM frame size"
    );
    assert_eq!(
        intake.append(&vec![0; 65_538]).unwrap_err(),
        "Invalid PCM frame size"
    );
    let gap = "Audio frames must have contiguous sequence and sample offsets";
    assert_eq!(intake.append(&frame(1, 0, &pcm)).unwrap_err(), gap);
    intake.append(&frame(0, 0, &pcm)).unwrap();
    assert_eq!(intake.append(&frame(1, 0, &pcm)).unwrap_err(), gap);
    intake.append(&frame(1, 1600, &pcm)).unwrap();
    assert_eq!(intake.samples(), 3200);
    intake.close_recording().unwrap();
    let saved: Vec<i16> = hound::WavReader::open(&path)
        .unwrap()
        .samples::<i16>()
        .map(Result::unwrap)
        .collect();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(saved, [pcm.clone(), pcm].concat());
}

#[test]
fn cue_offsets_are_checked() {
    let path = recording("cues");
    let mut intake = Intake::new(&start(48_000, 0), &path, None).unwrap();
    let samples = "Cue offsets must be sample counts";
    let second = "A cue lasts at most a second";
    assert_eq!(
        intake.ignore_cue(&json!(true), &json!(10)).unwrap_err(),
        samples
    );
    assert_eq!(
        intake.ignore_cue(&json!(1.5), &json!(10)).unwrap_err(),
        samples
    );
    assert_eq!(
        intake.ignore_cue(&json!(-1), &json!(10)).unwrap_err(),
        second
    );
    assert_eq!(
        intake.ignore_cue(&json!(10), &json!(5)).unwrap_err(),
        second
    );
    assert_eq!(
        intake.ignore_cue(&json!(0), &json!(48_001)).unwrap_err(),
        second
    );
    intake.ignore_cue(&json!(0), &json!(48_000)).unwrap();
    drop(intake);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn without_the_model_every_take_is_speech() {
    let path = recording("no-model");
    let mut intake = Intake::new(&start(16_000, 0), &path, None).unwrap();
    intake.append(&frame(0, 0, &[0; 1600])).unwrap();
    let speech = intake.speech();
    assert!(!speech.available());
    assert!(speech.heard(0, 1600));
    assert_eq!(speech.first_voice(), Some(0));
    assert_eq!(speech.quiet(), 0);
    assert_eq!(intake.next(false), Next::Wait);
    drop(intake);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_minute_behind_falls_back_to_the_whole_recording() {
    let path = recording("backlog");
    let mut intake = Intake::new(&start(16_000, 0), &path, None).unwrap();
    let second = vec![100i16; 16_000];
    for sequence in 0..61 {
        intake
            .append(&frame(sequence, intake.samples(), &second))
            .unwrap();
    }
    assert_eq!(intake.degraded_reason(), Some(FELL_BEHIND));
    assert!(intake.pending().is_empty());
    assert_eq!(intake.next(false), Next::Wait);
    assert_eq!(intake.next(true), Next::FullAudio);
    assert_eq!(intake.degraded_reason(), Some(FELL_BEHIND));
    drop(intake);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn speech_past_twenty_seconds_keeps_a_second_of_overlap() {
    let path = recording("forced");
    let mut intake = Intake::new(&start(16_000, 0), &path, None).unwrap();
    let second = vec![100i16; 16_000];
    for sequence in 0..25 {
        intake
            .append(&frame(sequence, intake.samples(), &second))
            .unwrap();
    }
    let Next::Phrase(window) = intake.next(false) else {
        panic!("expected a phrase window");
    };
    assert_eq!(
        (window.start, window.size, window.forced, window.paused),
        (0, 320_000, true, false)
    );
    intake.advance(&window);
    assert_eq!(intake.offset(), 304_000);
    assert_eq!(intake.covered(), 320_000);
    assert_eq!(intake.degraded_reason(), Some(CONTINUOUS));
    assert_eq!(intake.next(false), Next::Wait);
    assert_eq!(intake.next(true), Next::FullAudio);
    drop(intake);
    std::fs::remove_file(&path).unwrap();
}
