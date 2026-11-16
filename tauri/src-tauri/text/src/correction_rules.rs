//! The word fixes Kass learned from the user's corrections, applied to a
//! dictation.
//!
//! The model-improvement job learns each rule from one correction (a short
//! replacement anchored on unchanged words, or on the start or end of the
//! take) and keeps them in `correction-learning.json`. A dictation applies the
//! saved rules from memory: no disk, database or model during dictation.
//!
//! Ported from the applying half of the backend's `correction_rules` and
//! `correction_learning.apply_learned_corrections`; learning rules stays with
//! the job.

use serde_json::Value;

use crate::py::{self, Groups, Re};
use crate::re;

/// The most rules a saved state may hold.
pub const MAX_RULES: usize = 32;
/// Longer transcripts skip the rules, which are bounded and latency-tested up
/// to this many characters.
pub const MAX_TEXT: usize = 4000;

/// What Whisper writes for a pause between words of one phrase ("merge, domain").
const PAUSE: &str = r",?\s+";
/// After the last word of a take: closing punctuation only.
const TAKE_END: &str = r"[\s.!?,;:]*\Z";

fn word() -> &'static Re {
    re!(r"\w+(?:['’-]\w+)*")
}

/// The words of `text`, the way rules count them.
pub fn words(text: &str) -> Vec<&str> {
    word().findall(text)
}

/// One learned replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: String,
    /// The words matched, anchors included.
    pub source: Vec<String>,
    pub replacement: Vec<String>,
    pub language: Option<String>,
    /// Whether the rule is anchored on the take's start and end instead of a
    /// word. `None` for rules learned before edges: a word on each side,
    /// spaces only.
    pub edges: Option<(bool, bool)>,
}

impl Rule {
    /// A rule as `correction-learning.json` keeps it; `None` when malformed.
    pub fn from_json(value: &Value) -> Option<Rule> {
        let strings = |value: &Value| -> Option<Vec<String>> {
            value
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(String::from))
                .collect()
        };
        let edges = match value.get("edges") {
            None => None,
            Some(edges) => {
                let edges = edges.as_array()?;
                Some((edges.first()?.as_bool()?, edges.get(1)?.as_bool()?))
            }
        };
        Some(Rule {
            id: value["id"].as_str().unwrap_or_default().to_string(),
            source: strings(value.get("source")?)?,
            replacement: strings(value.get("replacement")?)?,
            language: value["language"].as_str().map(String::from),
            edges,
        })
    }

    fn pattern(&self) -> String {
        let escaped: Vec<String> = self.source.iter().map(|word| py::escape(word)).collect();
        let Some((start, end)) = self.edges else {
            return format!(r"(?<![\w'’-]){}(?![\w'’-])", escaped.join(r"\s+"));
        };
        format!(
            "{}{}{}",
            if start { r"\A\s*" } else { r"(?<![\w'’-])" },
            escaped.join(PAUSE),
            if end {
                format!("(?={TAKE_END})")
            } else {
                r"(?![\w'’-])".to_string()
            }
        )
    }
}

/// Rules ready to apply.
pub struct Rules {
    compiled: Vec<(Rule, Re)>,
}

impl Rules {
    pub fn new(rules: Vec<Rule>) -> Rules {
        let compiled = rules
            .into_iter()
            .map(|rule| {
                let pattern = Re::new(&rule.pattern());
                (rule, pattern)
            })
            .collect();
        Rules { compiled }
    }

    /// The rules of a saved `correction-learning.json`. A state Python would
    /// refuse (another version, too many rules, a malformed rule) gives none.
    pub fn from_state(state: &Value) -> Rules {
        let rules = (|| {
            if state.get("version")?.as_i64()? != 1 {
                return None;
            }
            let rules = state.get("rules")?.as_array()?;
            if rules.len() > MAX_RULES {
                return None;
            }
            rules
                .iter()
                .map(Rule::from_json)
                .collect::<Option<Vec<_>>>()
        })();
        Rules::new(rules.unwrap_or_default())
    }

    pub fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    /// `text` with the rules for `language` applied, at most one per span;
    /// context and punctuation are kept.
    pub fn apply(&self, text: &str, language: Option<&str>) -> String {
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        for (rule, pattern) in &self.compiled {
            if rule.language.as_deref() != language {
                continue;
            }
            for found in pattern.finditer(text) {
                let tokens = word().finditer(found.text());
                let mut replacement = rule.replacement.join(" ");
                let (start_edge, end_edge) = rule.edges.unwrap_or((false, false));
                let last = if end_edge {
                    tokens.last()
                } else {
                    tokens.len().checked_sub(2).and_then(|at| tokens.get(at))
                };
                let Some(last) = last else { continue };
                let end = found.start() + last.end();
                let start = if start_edge {
                    found.start() + tokens[0].start()
                } else if rule.edges.is_some() {
                    // From the anchor's end: a pause before the misheard words goes with them.
                    replacement = format!(" {replacement}");
                    found.start() + tokens[0].end()
                } else {
                    let Some(second) = tokens.get(1) else {
                        continue;
                    };
                    found.start() + second.start()
                };
                edits.push((start, end, replacement));
                if edits.len() > 128 {
                    return text.to_string();
                }
            }
        }
        // Overlapping candidates are ambiguous: leave those spans untouched.
        let mut safe: Vec<&(usize, usize, String)> = edits
            .iter()
            .filter(|edit| {
                !edits
                    .iter()
                    .any(|other| other != *edit && edit.0 < other.1 && other.0 < edit.1)
            })
            .collect();
        safe.sort();
        safe.dedup();
        let mut text = text.to_string();
        for (start, end, replacement) in safe.into_iter().rev() {
            text.replace_range(start..end, replacement);
        }
        text
    }

    /// What a dictation does with the learned rules: apply them, unless the
    /// transcript is too long for the latency-tested rule layer.
    pub fn apply_learned_corrections(&self, text: &str, language: Option<&str>) -> String {
        if py::len(text) <= MAX_TEXT {
            self.apply(text, language)
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn rule(source: &[&str], replacement: &[&str], edges: Option<(bool, bool)>) -> Rule {
        Rule {
            id: "r".into(),
            source: source.iter().map(|word| word.to_string()).collect(),
            replacement: replacement.iter().map(|word| word.to_string()).collect(),
            language: None,
            edges,
        }
    }

    #[test]
    fn applies_a_rule_between_anchors() {
        let rules = Rules::new(vec![rule(
            &["the", "jit", "hub", "page"],
            &["GitHub"],
            Some((false, false)),
        )]);
        assert_eq!(
            rules.apply("Open the jit hub page now.", None),
            "Open the GitHub page now."
        );
        assert_eq!(
            rules.apply("Open the jit hub page now.", Some("en")),
            "Open the jit hub page now."
        );
    }

    #[test]
    fn applies_a_rule_at_the_end_of_a_take() {
        let rules = Rules::new(vec![rule(
            &["merge", "domain"],
            &["to", "main"],
            Some((false, true)),
        )]);
        assert_eq!(
            rules.apply("Commit, merge, domain.", None),
            "Commit, merge to main."
        );
    }

    #[test]
    fn reads_saved_state() {
        let state = json!({"version": 1, "rules": [
            {"id": "a", "source": ["a", "b", "c"], "replacement": ["x"], "language": null, "edges": [false, false]}
        ]});
        assert!(!Rules::from_state(&state).is_empty());
        assert!(Rules::from_state(&json!({"version": 2, "rules": []})).is_empty());
    }
}
