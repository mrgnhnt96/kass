//! Prepares dictations the way cleanup does, for the shared-adapter scripts
//! (scripts/shared-adapters) to train and score with.
//!
//! Reads `{"samples": [{"raw", "flags"}]}` on stdin and writes
//! `{"cases": [{"text", "bypass", "system", "examples"}], "rehearsal": [[said, meant]]}`
//! to stdout: each sample through `prepare_refinement` and its style's prompt,
//! as a new install has them (no personal examples, notes, learned rules or
//! writing profile).

use std::io::Read;

use kass_text::refinement::{prepare_refinement, prompt, Flags, REFINEMENT_EXAMPLES};
use kass_text::writing_style::Profile;
use serde_json::{json, Value};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("read stdin");
    let request: Value = serde_json::from_str(&input).expect("stdin is JSON");
    let profile = Profile::default();
    let cases: Vec<Value> = request["samples"]
        .as_array()
        .expect("a samples list")
        .iter()
        .map(|sample| {
            let flags = Flags::from_json(&sample["flags"]);
            let raw = sample["raw"].as_str().expect("each sample has raw text");
            let (text, bypass) = prepare_refinement(raw, &flags, None);
            let (system, examples) = prompt(&flags, &profile, "", &[], None);
            json!({"text": text, "bypass": bypass, "system": system, "examples": examples})
        })
        .collect();
    println!(
        "{}",
        json!({"cases": cases, "rehearsal": REFINEMENT_EXAMPLES})
    );
}
