//! The cleanup a model-improvement run trains and tests its cleanup adapter
//! on, prepared the way dictation prepares it.
//!
//! The worker's training process asks for it over HTTP
//! (`/capture/learning/training/*`): it knows the transcripts, including the
//! ones it transcribes itself, but not the prompt or the text rules, which
//! live here. Training goes without the user's own examples and notes, so an
//! adapter learns the cleanup every style shares.

use std::path::Path;

use kass_text::correction_rules::{Rule, Rules};
use kass_text::py;
use kass_text::refinement::{self, Flags};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::{cleanup_inputs, styles};

/// What the cleanup model gets for each case (`{"text", "flags"}`): the
/// prepared `text`, the `bypass` when a spoken edit already finished it (the
/// model is skipped), and the `system` prompt and example turns for its
/// flags.
pub fn prompts(
    conn: &Connection,
    data_dir: &Path,
    cases: &[Value],
) -> rusqlite::Result<Vec<Value>> {
    let snapshot = styles::load(conn, None)?;
    let writing = cleanup_inputs::read_writing_style(data_dir);
    let (_, punctuation) = cleanup_inputs::learned_corrections(data_dir);
    Ok(cases
        .iter()
        .map(|case| {
            let flags = Flags::from_json(&case["flags"]);
            let transcript = case["text"].as_str().unwrap_or_default();
            let (text, bypass) =
                refinement::prepare_refinement(transcript, &flags, Some(&punctuation));
            // As Python had it: the style's own profile, else the default
            // style's; a deleted style's description is the default's.
            let profile_id = flags
                .style
                .clone()
                .unwrap_or_else(|| snapshot.default_style().id.clone());
            let profile = cleanup_inputs::profile(&writing, &profile_id);
            let description =
                py::strip(&snapshot.resolve(flags.style.as_deref()).description).to_string();
            let (system, examples) = refinement::prompt(&flags, &profile, &description, &[], None);
            json!({ "text": text, "bypass": bypass, "system": system, "examples": examples })
        })
        .collect())
}

/// Each case's `text` with the learned `rules` (as `correction-learning.json`
/// keeps them, frozen when the run was planned) applied for its `language`,
/// as a dictation applies them after cleanup.
pub fn fixed(rules: &Value, cases: &[Value]) -> Vec<String> {
    let rules = Rules::new(
        rules
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Rule::from_json)
            .collect(),
    );
    cases
        .iter()
        .map(|case| {
            let text = case["text"].as_str().unwrap_or_default();
            rules.apply_learned_corrections(text, case["language"].as_str())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data folder whose kass.db has one style, described.
    fn database() -> (std::path::PathBuf, Connection) {
        let dir = std::env::temp_dir().join(format!("kass-training-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kass.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../tests/kass_schema.sql"))
            .unwrap();
        conn.execute_batch(
            "INSERT INTO writing_styles (id, name, position, is_default, punctuation_style,
                 capitalize_first, smart_cleanup, preserve_technical, description)
             VALUES ('personal', 'Personal', 0, 1, 'standard', 1, 1, 1, ' Short and warm. ');",
        )
        .unwrap();
        crate::db::prepare(&path).unwrap();
        (dir, conn)
    }

    #[test]
    fn a_case_gets_the_prompt_dictation_would_use_without_personal_examples() {
        let (dir, conn) = database();
        let cases = [
            json!({ "text": "so um the meeting is at noon", "flags": null }),
            json!({ "text": "send it to john no wait send it to sarah", "flags": { "self_correction": true } }),
            json!({ "text": "delete that", "flags": {} }),
        ];
        let rendered = prompts(&conn, &dir, &cases).unwrap();
        let (system, examples) = refinement::prompt(
            &Flags::default(),
            &Default::default(),
            "Short and warm.",
            &[],
            None,
        );
        assert_eq!(rendered[0]["system"], json!(system));
        assert_eq!(rendered[0]["examples"], json!(examples));
        assert_eq!(rendered[0]["text"], "so um the meeting is at noon");
        assert_eq!(rendered[0]["bypass"], Value::Null);
        assert!(rendered[1]["text"].as_str().unwrap().contains("sarah"));
        assert!(!rendered[1]["text"].as_str().unwrap().contains("john"));
        assert_eq!(rendered.len(), 3);
    }

    #[test]
    fn the_frozen_rules_fix_each_case_in_its_language() {
        let rules = json!([{
            "id": "r", "source": ["to", "jit", "hub"], "replacement": ["GitHub"],
            "language": null, "edges": [false, true],
        }]);
        let cases = [
            json!({ "text": "Push it to jit hub.", "language": null }),
            json!({ "text": "Push it to jit hub.", "language": "fr" }),
        ];
        assert_eq!(
            fixed(&rules, &cases),
            ["Push it to GitHub.", "Push it to jit hub."]
        );
        assert_eq!(fixed(&Value::Null, &cases[..1]), ["Push it to jit hub."]);
    }
}
