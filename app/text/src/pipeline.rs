//! Which cleanup pipeline a trained cleanup adapter was tested with.
//!
//! An adapter learned to clean up text the way the prompt, the example turns
//! and the text rules prepared it. When any of those changes, its tests no
//! longer say how it does, so it isn't used: model improvement and the shared
//! adapters stamp each adapter with [`id`] and the worker drops one with
//! another.
//!
//! The id hashes what the pipeline does, not its code: every prompt and set
//! of example turns, and what the text rules make of [`PROBES`]. A refactor
//! that changes nothing keeps it; a rule that now writes a probe differently,
//! or a prompt reworded, changes it. Bump [`VERSION`] for a change the probes
//! don't show.
//!
//! The Python worker hashed its own functions' bytecode instead. Its last two
//! ids ([`LEGACY`]) name the pipeline this crate ports, so adapters stamped
//! with them are restamped with [`id`] rather than dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::correction_rules::{Rule, Rules};
use crate::refinement::{self, Example, Flags, Punctuation, REFINEMENT_EXAMPLES};
use crate::spoken_punctuation::Learned;
use crate::writing_style::{self, Profile};

/// Bump for a pipeline change the probes and prompts don't show.
pub const VERSION: u32 = 1;

/// The ids the Python worker gave the pipeline this crate ports: v0.10.0
/// betas 4 to 9, and main once `correction_rules` gained a guard that
/// changed its bytecode and nothing else.
pub const LEGACY: [&str; 2] = [
    "28ee71749df7b8db5eb200375e5333017b0661e9851156b8ee371d11c1e11876",
    "883d776a64e9ffc5b04febf0d5630fab048cfb2c527c18d5bc9a8ef094eca228",
];

/// Dictations the text rules are run on: fillers, retractions, spoken edits,
/// marks, breaks, case, spelling, laughter, loops and learned fixes.
pub const PROBES: &[&str] = &[
    "so um like the meeting is at 3pm you know on tuesday",
    "send it to john no wait send it to sarah",
    "the deadline is friday actually make that monday",
    "let's meet at noon scratch that let's meet at one",
    "buy milk new line buy eggs new paragraph buy bread",
    "hello comma how are you question mark",
    "this is in all caps important end caps",
    "open quote hello close quote she said",
    "my name is kass k a s s",
    "ha ha ha ha ha ha ha that was funny",
    "the the the the the the the the cat",
    "I I think we should we should go",
    "number one milk number two eggs number three bread",
    "run npm install then open index dot tsx",
    "can you tell me a joke about databases",
    "merge domain into main and push to jit hub",
    "the trial period ends soon full stop",
    "delete that",
    "wow bang that was great",
    "",
];

/// The learned fixes the probes are run through.
fn probe_rules() -> Rules {
    let rule =
        |id: &str, source: &[&str], replacement: &[&str], edges: Option<(bool, bool)>| Rule {
            id: id.into(),
            source: source.iter().map(|w| w.to_string()).collect(),
            replacement: replacement.iter().map(|w| w.to_string()).collect(),
            language: None,
            edges,
        };
    Rules::new(vec![
        rule("a", &["push", "to", "jit", "hub", "now"], &["GitHub"], None),
        rule(
            "b",
            &["merge", "domain", "into"],
            &["merged", "domain"],
            Some((true, false)),
        ),
        rule("c", &["to", "jit", "hub"], &["GitHub"], Some((false, true))),
        rule("d", &["to", "sarah"], &["Sara"], Some((false, true))),
    ])
}

/// The words the speaker taught for marks, in the probes.
fn probe_learned() -> Learned {
    Learned {
        aliases: BTreeMap::from([("bang".to_string(), "!".to_string())]),
        kept: BTreeSet::from(["trial".to_string()]),
    }
}

/// A style that learned how the speaker punctuates.
fn probe_profile() -> Profile {
    Profile {
        runs: 1,
        examples: vec![writing_style::Example {
            said: Some("see you tomorrow".into()),
            shown: "See you tomorrow.".into(),
            written: "see you tomorrow".into(),
        }],
        ..Profile::default()
    }
}

fn all_flags() -> Vec<Flags> {
    let mut all = Vec::new();
    for smart_cleanup in [false, true] {
        for self_correction in [false, true] {
            for preserve_technical in [false, true] {
                for punctuation in [
                    Punctuation::Standard,
                    Punctuation::Casual,
                    Punctuation::Learned,
                ] {
                    all.push(Flags {
                        smart_cleanup,
                        self_correction,
                        preserve_technical,
                        punctuation,
                        ..Flags::default()
                    });
                }
            }
        }
    }
    all
}

fn examples(examples: &[Example]) -> Value {
    json!(examples)
}

/// What the pipeline does, as hashed.
fn behaviour() -> Value {
    let profiles = [Profile::default(), probe_profile()];
    let personal: Vec<Example> = vec![("um send it now".into(), "Send it now.".into())];
    let learned = probe_learned();
    let rules = probe_rules();
    let mut prompts = Vec::new();
    let mut texts = Vec::new();
    for flags in all_flags() {
        for profile in &profiles {
            for (description, personal, notes) in [
                ("", &[][..], None),
                (
                    "Short, friendly, no periods.",
                    &personal[..],
                    Some("Rules: keep it short."),
                ),
            ] {
                let (system, turns) =
                    refinement::prompt(&flags, profile, description, personal, notes);
                prompts.push(json!([system, examples(&turns)]));
            }
        }
        // Only these two flags change what the text rules do.
        let text_flags = flags.preserve_technical && flags.punctuation == Punctuation::Standard;
        if !text_flags {
            continue;
        }
        for probe in PROBES {
            for taught in [None, Some(&learned)] {
                let (text, resolved) = refinement::prepare_refinement(probe, &flags, taught);
                let fixed = rules.apply_learned_corrections(&text, None);
                texts.push(json!([text, resolved, fixed]));
            }
        }
    }
    json!({
        "version": VERSION,
        "examples": REFINEMENT_EXAMPLES,
        "prompts": prompts,
        "texts": texts,
    })
}

static ID: LazyLock<String> = LazyLock::new(|| {
    let hash = Sha256::digest(behaviour().to_string().as_bytes());
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
});

/// The cleanup pipeline's id: a sha256, hex.
pub fn id() -> &'static str {
    &ID
}

/// The stamp an adapter stamped `pipeline` gets now: [`id`] for one of the
/// Python worker's ids for this pipeline; `None` keeps its own.
pub fn restamped(pipeline: &str) -> Option<&'static str> {
    LEGACY.contains(&pipeline).then(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_a_sha256_and_stable() {
        assert_eq!(id().len(), 64);
        assert!(id().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id(), id());
        assert!(!LEGACY.contains(&id()));
    }

    #[test]
    fn only_the_ported_python_ids_are_restamped() {
        assert_eq!(restamped(LEGACY[0]), Some(id()));
        assert_eq!(restamped(LEGACY[1]), Some(id()));
        assert_eq!(restamped(id()), None);
        assert_eq!(restamped("0123"), None);
    }

    #[test]
    fn the_probes_exercise_the_rules() {
        // A probe set the rules leave alone would hash nothing.
        let flags = Flags::default();
        let changed = PROBES
            .iter()
            .filter(|probe| {
                let (text, resolved) =
                    refinement::prepare_refinement(probe, &flags, Some(&probe_learned()));
                text != **probe || resolved.is_some()
            })
            .count();
        assert!(changed >= PROBES.len() / 2, "only {changed} probes changed");
        let (text, _) = refinement::prepare_refinement(PROBES[15], &flags, None);
        assert!(probe_rules().apply(&text, None).contains("GitHub"));
    }
}
