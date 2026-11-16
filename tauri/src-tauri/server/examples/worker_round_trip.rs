//! What calling the worker over HTTP adds to a dictation, on the parity
//! recordings: each phrase window Python's stream sends to recognition
//! (tests/stream_audio/python.json) goes to `/worker/recognize`, then the
//! take's text to `/worker/cleanup/generate` with the default style's prompt.
//! The cost of a call is its time here less the model time the worker
//! reports. Before each take's last phrase the connection is left idle past
//! the pool's timeout, as after a pause, so the release pays for a new one.
//!
//! Start a worker on a spare data folder, then, from tauri/src-tauri:
//!
//!     cd ../.. && HF_HUB_OFFLINE=1 backend/venv/bin/python -m backend.main \
//!         --port 18494 --data-dir "$(mktemp -d)"
//!     backend/venv/bin/python -c 'import json; from backend.services.refinement \
//!         import *; f = RefinementFlags(); json.dump({"system": \
//!         build_refinement_prompt(f), "examples": refinement_examples(f, []), \
//!         "flags": f.to_dict()}, open("/tmp/prompt.json", "w"))'
//!     cargo run --release -p kass-server --example worker_round_trip -- \
//!         127.0.0.1:18494 /tmp/prompt.json /tmp/round-trip.json

use std::path::Path;
use std::time::{Duration, Instant};

use kass_server::proxy::Proxy;
use kass_server::stream::models::{self, CleanupPrompt, Generate, Recognize};
use serde_json::{json, Value};

const ROUNDS: usize = 3;
/// Past the pool's idle timeout, so the next call connects again.
const IDLE: Duration = Duration::from_millis(1_100);

fn clip(audio: &Path, name: &str) -> Vec<i16> {
    let reader = hound::WavReader::open(audio.join(name)).expect("a parity recording");
    assert_eq!(reader.spec().sample_rate, 16_000);
    reader
        .into_samples::<i16>()
        .map(|s| s.expect("PCM"))
        .collect()
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let position = (ordered.len() - 1) as f64 * fraction;
    let low = position.floor() as usize;
    let high = (low + 1).min(ordered.len() - 1);
    ordered[low] + (ordered[high] - ordered[low]) * (position - low as f64)
}

fn summary(values: &[f64]) -> Value {
    json!({
        "calls": values.len(),
        "p50_ms": percentile(values, 0.5) * 1000.0,
        "p95_ms": percentile(values, 0.95) * 1000.0,
        "max_ms": values.iter().cloned().fold(0.0, f64::max) * 1000.0,
    })
}

#[tokio::main]
async fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, worker, prompt, out] = &arguments[..] else {
        panic!("usage: worker_round_trip <worker address> <prompt.json> <out.json>");
    };
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let audio = here.join("../../../scripts/parity/audio");
    let python: Value = serde_json::from_slice(
        &std::fs::read(here.join("tests/stream_audio/python.json")).unwrap(),
    )
    .unwrap();
    let prompt: Value = serde_json::from_slice(&std::fs::read(prompt).unwrap()).unwrap();
    let proxy = Proxy::new(worker.parse().expect("host:port"));
    let cleanup = CleanupPrompt {
        model_size: "0.6B".into(),
        flags: Some(prompt["flags"].clone()),
        system: prompt["system"].as_str().unwrap().into(),
        examples: serde_json::from_value(prompt["examples"].clone()).unwrap(),
        cache_key: Some("cleanup:default".into()),
    };
    let cases: Vec<&Value> = python["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["rate"] == 16_000 && case["model"] == true)
        .collect();

    let (mut phrase, mut first_partial, mut cleanup_cost) = (vec![], vec![], vec![]);
    let (mut release, mut stop_calls, mut reconnects) = (vec![], vec![], vec![]);
    for round in 0..ROUNDS {
        let measured = round > 0;
        for (number, case) in cases.iter().enumerate() {
            let pcm: Vec<i16> = case["parts"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|part| match part.as_str() {
                    Some(name) => clip(&audio, name),
                    // Seconds of silence between recordings.
                    None => vec![0; (part.as_f64().unwrap() * 16_000.0).round() as usize],
                })
                .collect();
            let windows: Vec<&Value> = case["windows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|window| window["heard"] == true)
                .collect();
            let mut text = String::new();
            let mut last_recognize = 0.0;
            for (index, window) in windows.iter().enumerate() {
                let start = window["start"].as_u64().unwrap() as usize;
                let end = (start + window["size"].as_u64().unwrap() as usize).min(pcm.len());
                let last = index + 1 == windows.len();
                if last {
                    tokio::time::sleep(IDLE).await;
                }
                let request = Recognize {
                    rate: 16_000,
                    previous_text: Some(text.clone()),
                    ..Default::default()
                };
                let began = Instant::now();
                let answer = models::recognize(&proxy, &request, &pcm[start..end])
                    .await
                    .unwrap();
                let cost = began.elapsed().as_secs_f64() - answer.seconds;
                if measured {
                    if last {
                        reconnects.push(cost);
                    } else {
                        phrase.push(cost);
                    }
                }
                last_recognize = cost;
                text = format!("{text} {}", answer.text).trim().to_owned();
            }
            if text.is_empty() {
                continue;
            }
            let request = Generate {
                id: format!("round-trip-{round}-{number}"),
                prompt: text,
                hint: Some(String::new()),
                max_tokens: 2048,
                temperature: 0.2,
                cleanup: cleanup.clone(),
            };
            let began = Instant::now();
            let mut first = None;
            let done = models::generate(&proxy, &request, |_| {
                first.get_or_insert_with(|| began.elapsed().as_secs_f64());
            })
            .await
            .unwrap();
            let cost = began.elapsed().as_secs_f64() - done.seconds;
            let began = Instant::now();
            models::stop(&proxy, &request.id).await;
            if measured {
                cleanup_cost.push(cost);
                stop_calls.push(began.elapsed().as_secs_f64());
                first_partial.extend(first);
                release.push(last_recognize + cost);
            }
        }
        eprintln!("round {} of {ROUNDS} done", round + 1);
    }

    let report = json!({
        "takes": cases.len(),
        "rounds_measured": ROUNDS - 1,
        "recognize_phrase": summary(&phrase),
        "recognize_after_idle": summary(&reconnects),
        "cleanup": summary(&cleanup_cost),
        "cleanup_first_partial": summary(&first_partial),
        "stop": summary(&stop_calls),
        "release_added": summary(&release),
        "samples": {
            "recognize_phrase": phrase,
            "recognize_after_idle": reconnects,
            "cleanup": cleanup_cost,
            "release_added": release,
        },
    });
    std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&report["release_added"]).unwrap()
    );
}
