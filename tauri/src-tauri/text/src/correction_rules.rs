//! The word fixes Kass learned from the user's corrections, applied to a
//! dictation.
//!
//! The model-improvement job learns each rule from one correction (a short
//! replacement anchored on unchanged words, or on the start or end of the
//! take) and keeps them in `correction-learning.json`. A dictation applies the
//! saved rules from memory: no disk, database or model during dictation.
//!
//! Ported from the backend's `correction_rules` and
//! `correction_learning.apply_learned_corrections`. Learning a rule
//! ([`candidate`], [`evaluate`]) is pure too: the server's job reads the
//! reports and saves what this returns.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use serde_json::{json, Value};

use crate::difflib::{self, SequenceMatcher, Tag};
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

    /// The rule as `correction-learning.json` keeps it.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "id": self.id,
            "source": self.source,
            "replacement": self.replacement,
            "language": self.language,
        });
        if let Some((start, end)) = self.edges {
            value["edges"] = json!([start, end]);
        }
        value
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
    compiled: Vec<(Rule, Arc<Re>)>,
}

/// Compiled patterns by pattern text: learning compiles the same few rules
/// for every report it checks them against.
static PATTERNS: LazyLock<Mutex<HashMap<String, Arc<Re>>>> = LazyLock::new(Default::default);

/// Rules come and go with reports; the cache stays small.
const MAX_PATTERNS: usize = 4096;

fn compile(rule: &Rule) -> Arc<Re> {
    let pattern = rule.pattern();
    let mut cache = PATTERNS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = cache.get(&pattern) {
        return found.clone();
    }
    if cache.len() >= MAX_PATTERNS {
        cache.clear();
    }
    let compiled = Arc::new(Re::new(&pattern));
    cache.insert(pattern, compiled.clone());
    compiled
}

impl Rules {
    pub fn new(rules: Vec<Rule>) -> Rules {
        let compiled = rules
            .into_iter()
            .map(|rule| {
                let pattern = compile(&rule);
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
                // A rule learned before edges needs a misheard word between
                // its anchors; one without (a saved state written by hand)
                // has nothing to replace.
                if start > end {
                    continue;
                }
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

/// Independent, unchanged examples: candidates must preserve all of these.
const KNOWN_GOOD: [&str; 16] = [
    "Tell me a joke about databases.",
    "What time is the meeting?",
    "Remind me to call Mom tomorrow.",
    "The meeting is at four pm on Tuesday.",
    "Run npm install and open index.tsx.",
    "Keep the API key out of the logs.",
    "I mean what I said.",
    "Actually, that is correct.",
    "The voice box is part of the larynx.",
    "Put the git hub on the workbench.",
    "Send the report to Morgan.",
    "Do not delete the database.",
    "The total is $15.50, not $50.",
    "We need three hundred records.",
    "Open the box and close the door.",
    "The file is in src/components.",
];

/// The slowest the rules may make a worst-size dictation, in milliseconds.
const LATENCY_MS: f64 = 5.0;

/// One report: the take as it was shown and as the user wanted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    pub id: String,
    pub capture_id: String,
    pub original: String,
    pub expected: String,
    pub language: Option<String>,
    /// The report's source: "manual", "voice_fix" or "redictation".
    pub source: String,
}

/// Python's `repr` of a string, which a rule's id hashes.
fn repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !printable(c) => {
                let code = c as u32;
                if code <= 0xff {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable` for the characters dictations hold: control
/// characters, spaces other than " ", and invisible format marks aren't.
fn printable(c: char) -> bool {
    let code = c as u32;
    !(code < 0x20
        || (0x7f..=0xa0).contains(&code)
        || code == 0xad
        || code == 0x1680
        || (0x2000..=0x200f).contains(&code)
        || (0x2028..=0x202f).contains(&code)
        || (0x205f..=0x206f).contains(&code)
        || code == 0x3000
        || code == 0xfeff)
}

/// What the job last learned from, as Python fingerprints it: the SHA-256 of
/// `repr(examples)`. The same reports give the same fingerprint in either,
/// so moving the job from Python doesn't make it learn again.
pub fn fingerprint(examples: &[Example]) -> String {
    use sha2::{Digest, Sha256};
    let items: Vec<String> =
        examples
            .iter()
            .map(|e| {
                format!(
                "Example(id={}, capture_id={}, original={}, expected={}, language={}, source={})",
                repr(&e.id),
                repr(&e.capture_id),
                repr(&e.original),
                repr(&e.expected),
                e.language.as_deref().map_or_else(|| "None".to_string(), repr),
                repr(&e.source),
            )
            })
            .collect();
    let digest = Sha256::digest(format!("[{}]", items.join(", ")).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn repr_list(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| repr(item)).collect();
    format!("[{}]", items.join(", "))
}

fn python_bool(value: bool) -> &'static str {
    if value {
        "True"
    } else {
        "False"
    }
}

/// The id Python gives a rule: the first 16 hex digits of the SHA-256 of
/// `repr((source, replacement, language, edges))`, so a rule learned in
/// either keeps its id (and its blocks) in the other.
fn rule_id(
    source: &[String],
    replacement: &[String],
    language: Option<&str>,
    edges: (bool, bool),
) -> String {
    use sha2::{Digest, Sha256};
    let key = format!(
        "({}, {}, {}, [{}, {}])",
        repr_list(source),
        repr_list(replacement),
        language.map_or_else(|| "None".to_string(), repr),
        python_bool(edges.0),
        python_bool(edges.1),
    );
    let digest = Sha256::digest(key.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Learn one short replacement anchored on both sides.
///
/// An anchor is an unchanged word, or the start or end of the take where
/// the edit has no word on that side ("Commit, merge, domain" -> "Commit,
/// merge to main"). Context stays in the matching key so a correction never
/// becomes a global single-word replacement. Insertions, deletions and
/// multi-edit rewrites are not learned.
pub fn candidate(example: &Example) -> Option<Rule> {
    let old = words(&example.original);
    let new = words(&example.expected);
    let edits: Vec<_> = SequenceMatcher::new(&old, &new, false)
        .get_opcodes()
        .into_iter()
        .filter(|op| op.tag != Tag::Equal)
        .collect();
    let [edit] = edits[..] else {
        return None;
    };
    let (a, b, c, d) = (edit.i1, edit.i2, edit.j1, edit.j2);
    if edit.tag != Tag::Replace || !(1..=3).contains(&(b - a)) || !(1..=3).contains(&(d - c)) {
        return None;
    }
    let edges = (a == 0, b == old.len());
    let matches = word().finditer(&example.original);
    if edges.0 && !py::strip(&example.original[..matches[0].start()]).is_empty() {
        return None;
    }
    if edges.1
        && re!(TAKE_END)
            .fullmatch(&example.original[matches[matches.len() - 1].end()..])
            .is_none()
    {
        return None;
    }
    // Only word phrases, where a comma may stand for a pause; never learn
    // edits to code punctuation.
    let first = a - usize::from(!edges.0);
    let last = b - usize::from(edges.1);
    let phrase = &example.original[matches[first].start()..matches[last].end()];
    re!(format!(r"[\w'’\-]+(?:{PAUSE}[\w'’\-]+)*")).fullmatch(phrase)?;
    let source: Vec<String> = old[first..=last].iter().map(|w| w.to_string()).collect();
    let replacement: Vec<String> = new[c..d].iter().map(|w| w.to_string()).collect();
    let old_term = old[a..b].join(" ");
    let new_term = replacement.join(" ");
    if format!("{old_term}{new_term}").chars().any(py::is_digit) {
        return None;
    }
    // Letters, not word breaks: Whisper splits and joins words it mishears
    // ("domain" for "to main").
    let letters = |term: &str| py::casefold(term).replace(' ', "");
    if difflib::ratio(&letters(&old_term), &letters(&new_term)) < 0.6 {
        return None;
    }
    let language = example.language.clone();
    Some(Rule {
        id: rule_id(&source, &replacement, language.as_deref(), edges),
        source,
        replacement,
        language,
        edges: Some(edges),
    })
}

/// Token edit distance, punctuation and capitalization included.
pub fn loss(actual: &str, expected: &str) -> usize {
    let tokens = |text: &str| -> Vec<String> {
        re!(r"\w+|[^\w\s]")
            .findall(text)
            .into_iter()
            .map(String::from)
            .collect()
    };
    let (a, b) = (tokens(actual), tokens(expected));
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, token) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, target) in b.iter().enumerate() {
            let replace = previous[j] + usize::from(token != target);
            current.push((current[j] + 1).min(previous[j + 1] + 1).min(replace));
        }
        previous = current;
    }
    previous[b.len()]
}

fn with(before: &[Rule], rule: &Rule) -> Rules {
    let mut rules = before.to_vec();
    rules.push(rule.clone());
    Rules::new(rules)
}

fn improves(rule: &Rule, before: &[Rule], example: &Example) -> bool {
    let language = example.language.as_deref();
    let prior = Rules::new(before.to_vec()).apply(&example.original, language);
    let after = with(before, rule).apply(&example.original, language);
    loss(&after, &example.expected) < loss(&prior, &example.expected)
}

/// The words the rule writes, with its anchor words, and the edges it is
/// anchored to.
fn corrected(rule: &Rule) -> (Vec<String>, bool, bool) {
    let (start, end) = rule.edges.unwrap_or((false, false));
    let mut phrase = Vec::new();
    if !start {
        phrase.extend(rule.source.first().cloned());
    }
    phrase.extend(rule.replacement.iter().cloned());
    if !end {
        phrase.extend(rule.source.last().cloned());
    }
    (phrase, start, end)
}

fn contains(haystack: &[&str], needle: &[String], start: bool, end: bool) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    let last = haystack.len() - needle.len();
    (0..=last).any(|i| {
        (!start || i == 0)
            && (!end || i == last)
            && haystack[i..i + needle.len()].iter().eq(needle.iter())
    })
}

/// The report wrote the rule's corrected phrase that its take misheard some
/// other way.
///
/// Whisper mishears a phrase differently each time ("merge, domain",
/// "merged, been"): a report fixing another mishearing to the same words is
/// a further recording where the user meant them.
fn rewrites(rule: &Rule, example: &Example) -> bool {
    let (phrase, start, end) = corrected(rule);
    contains(&words(&example.expected), &phrase, start, end)
        && !contains(&words(&example.original), &phrase, start, end)
}

/// Two recordings teach a rule and a third confirms it; every report counts
/// at once.
///
/// The third is fixed by the rule or writes its corrected phrase where its
/// take misheard it another way ([`rewrites`]). At least one teaching report
/// must be explicit (a redictation only backs a rule up). Active rules are
/// checked the same way, so one that lost its support (a withdrawn or
/// deleted report) is dropped.
struct Evidence<'e> {
    examples: &'e [Example],
    /// Rule id -> the examples whose candidate it is.
    teachers: HashMap<String, Vec<usize>>,
}

impl Evidence<'_> {
    fn confirmed(&self, rule: &Rule, before: &[Rule]) -> bool {
        let improved: Vec<usize> = (0..self.examples.len())
            .filter(|&i| improves(rule, before, &self.examples[i]))
            .collect();
        let taught: Vec<usize> = self
            .teachers
            .get(&rule.id)
            .map(|group| {
                group
                    .iter()
                    .copied()
                    .filter(|i| improved.contains(i))
                    .collect()
            })
            .unwrap_or_default();
        let mut confirming: HashSet<&str> = improved
            .iter()
            .map(|&i| self.examples[i].capture_id.as_str())
            .collect();
        for (i, example) in self.examples.iter().enumerate() {
            if !improved.contains(&i) && rewrites(rule, example) {
                confirming.insert(&example.capture_id);
            }
        }
        let taught_takes: HashSet<&str> = taught
            .iter()
            .map(|&i| self.examples[i].capture_id.as_str())
            .collect();
        confirming.len() >= 3
            && taught_takes.len() >= 2
            && taught
                .iter()
                .any(|&i| self.examples[i].source != "redictation")
    }
}

/// What [`evaluate`] measured, as `correction-learning.json` keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Metrics {
    pub examples: usize,
    pub candidates: usize,
    pub accepted: usize,
    pub withdrawn: usize,
    pub median_rule_ms: f64,
    pub latency_passed: bool,
}

impl Metrics {
    pub fn to_json(&self) -> Value {
        json!({
            "examples": self.examples,
            "candidates": self.candidates,
            "accepted": self.accepted,
            "withdrawn": self.withdrawn,
            "median_rule_ms": self.median_rule_ms,
            "latency_passed": self.latency_passed,
        })
    }
}

/// The rules to keep and add: one is added only on independent evidence and
/// never regresses a saved example. Duplicate takes can't supply independent
/// evidence. `blocked` rules are never proposed.
///
/// Python's `evaluate(..., every_report=True)`, the only way the job calls it.
pub fn evaluate(examples: &[Example], active: &[Rule], blocked: &[String]) -> (Vec<Rule>, Metrics) {
    // The last report of each take and language, where that take first came.
    let mut order: Vec<(Option<&str>, &str)> = Vec::new();
    let mut unique: HashMap<(Option<&str>, &str), &Example> = HashMap::new();
    for example in examples {
        let key = (example.language.as_deref(), example.original.as_str());
        if unique.insert(key, example).is_none() {
            order.push(key);
        }
    }
    let examples: Vec<Example> = order.iter().map(|key| unique[key].clone()).collect();

    let mut teachers: HashMap<String, Vec<usize>> = HashMap::new();
    let mut candidates: HashMap<String, Rule> = HashMap::new();
    let mut seen: Vec<String> = Vec::new();
    for (i, example) in examples.iter().enumerate() {
        if let Some(rule) = candidate(example) {
            if !teachers.contains_key(&rule.id) {
                seen.push(rule.id.clone());
            }
            teachers.entry(rule.id.clone()).or_default().push(i);
            candidates.insert(rule.id.clone(), rule);
        }
    }
    let evidence = Evidence {
        examples: &examples,
        teachers,
    };
    let mut proposed: Vec<Rule> = seen
        .iter()
        .filter(|id| !blocked.contains(id))
        .filter(|id| {
            let takes: HashSet<&str> = evidence.teachers[*id]
                .iter()
                .map(|&i| examples[i].capture_id.as_str())
                .collect();
            takes.len() >= 2
        })
        .map(|id| candidates[id].clone())
        .collect();
    let retained: Vec<Rule> = active
        .iter()
        .filter(|rule| evidence.confirmed(rule, &[]))
        .cloned()
        .collect();

    let mut languages: Vec<Option<&str>> = vec![None];
    for example in &examples {
        if !languages.contains(&example.language.as_deref()) {
            languages.push(example.language.as_deref());
        }
    }
    let mut validation: Vec<(&str, &str, Option<&str>)> = Vec::new();
    for example in &examples {
        validation.push((
            &example.original,
            &example.expected,
            example.language.as_deref(),
        ));
    }
    for example in &examples {
        validation.push((
            &example.expected,
            &example.expected,
            example.language.as_deref(),
        ));
    }
    for text in KNOWN_GOOD {
        for language in &languages {
            validation.push((text, text, *language));
        }
    }

    // Require each addition to be confirmed and regress nowhere.
    let mut selected = retained.clone();
    let mut accepted = 0;
    proposed.sort_by(|a, b| a.id.cmp(&b.id));
    for rule in &proposed {
        if selected.iter().any(|kept| kept.id == rule.id) || selected.len() >= MAX_RULES {
            continue;
        }
        let before = Rules::new(selected.clone());
        let after = with(&selected, rule);
        let regresses = validation.iter().any(|(text, expected, language)| {
            loss(&after.apply(text, *language), expected)
                > loss(&before.apply(text, *language), expected)
        });
        if regresses || !evidence.confirmed(rule, &selected) {
            continue;
        }
        selected.push(rule.clone());
        accepted += 1;
    }

    let latency = median_latency(&examples, &selected, &languages);
    let passed = latency <= LATENCY_MS;
    let metrics = Metrics {
        examples: examples.len(),
        candidates: proposed.len(),
        accepted: if passed { accepted } else { 0 },
        withdrawn: active.len() - retained.len(),
        median_rule_ms: (latency * 1000.0).round() / 1000.0,
        latency_passed: passed,
    };
    // Dropping unsupported rules only makes the layer faster.
    (if passed { selected } else { retained }, metrics)
}

/// The median time, over 25 runs, the rules take on a worst-size input in
/// every language, in milliseconds.
fn median_latency(examples: &[Example], rules: &[Rule], languages: &[Option<&str>]) -> f64 {
    let rules = Rules::new(rules.to_vec());
    let originals: Vec<&str> = examples.iter().map(|e| e.original.as_str()).collect();
    let probe = format!("{}{}", originals.join(" "), KNOWN_GOOD.join(" "));
    let probe = py::head(&probe, MAX_TEXT);
    let repeat = MAX_TEXT / py::len(probe).max(1) + 1;
    let probe = format!("{probe} ").repeat(repeat);
    let probe = py::head(&probe, MAX_TEXT);
    let mut timings: Vec<f64> = (0..25)
        .map(|_| {
            let start = Instant::now();
            for language in languages {
                std::hint::black_box(rules.apply(probe, *language));
            }
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    timings.sort_by(|a, b| a.total_cmp(b));
    timings[timings.len() / 2]
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

    fn example(id: usize, original: &str, expected: &str, source: &str) -> Example {
        Example {
            id: id.to_string(),
            capture_id: id.to_string(),
            original: original.into(),
            expected: expected.into(),
            language: None,
            source: source.into(),
        }
    }

    fn samples(sources: [&str; 3]) -> Vec<Example> {
        ["home", "work", "school"]
            .iter()
            .zip(sources)
            .enumerate()
            .map(|(i, (place, source))| {
                example(
                    i,
                    &format!("Please use voice box today at {place}."),
                    &format!("Please use Voicebox today at {place}."),
                    source,
                )
            })
            .collect()
    }

    #[test]
    fn rule_ids_match_python() {
        let mut fixed = example(
            1,
            "I can't see clod's code.",
            "I can't see Claude's code.",
            "manual",
        );
        fixed.language = Some("en".into());
        let rule = candidate(&fixed).unwrap();
        assert_eq!(rule.id, "023a52498879794e");
        assert_eq!(rule.source, ["see", "clod's", "code"]);
        let rule = candidate(&example(
            1,
            "Commit, PR, merge, domain.",
            "Commit, PR, merge to main.",
            "manual",
        ))
        .unwrap();
        assert_eq!(rule.id, "f653b0409b015cd6");
        assert_eq!(rule.edges, Some((false, true)));
    }

    #[test]
    fn two_takes_teach_and_a_third_confirms() {
        let examples = samples(["manual"; 3]);
        assert!(evaluate(&examples[..2], &[], &[]).0.is_empty());
        let (selected, metrics) = evaluate(&examples, &[], &[]);
        assert_eq!(selected.len(), 1);
        assert_eq!((metrics.examples, metrics.accepted), (3, 1));
        assert!(evaluate(&vec![examples[0].clone(); 6], &[], &[])
            .0
            .is_empty());
        let same_take: Vec<Example> = examples
            .iter()
            .map(|e| Example {
                capture_id: "same".into(),
                ..e.clone()
            })
            .collect();
        assert!(evaluate(&same_take, &[], &[]).0.is_empty());
        assert!(evaluate(&examples, &[], &[selected[0].id.clone()])
            .0
            .is_empty());
    }

    #[test]
    fn redictations_only_back_up_an_explicit_report() {
        for (sources, learned) in [
            (["redictation"; 3], false),
            (["manual", "redictation", "redictation"], true),
            (["voice_fix", "redictation", "voice_fix"], true),
        ] {
            assert_eq!(
                !evaluate(&samples(sources), &[], &[]).0.is_empty(),
                learned,
                "{sources:?}"
            );
        }
    }

    #[test]
    fn an_active_rule_without_support_is_dropped() {
        let (selected, _) = evaluate(&samples(["manual"; 3]), &[], &[]);
        let (kept, metrics) = evaluate(&samples(["manual"; 3])[..2], &selected, &[]);
        assert!(kept.is_empty());
        assert_eq!(metrics.withdrawn, 1);
        assert_eq!(
            evaluate(&samples(["manual"; 3]), &selected, &[]).0,
            selected
        );
    }

    #[test]
    fn deletions_and_broad_rewrites_are_not_learned() {
        let original = "Please use voice box today at home.";
        for expected in [
            "",
            "Completely different wording.",
            "Please use voice box today at home. Extra instructions.",
        ] {
            assert_eq!(candidate(&example(1, original, expected, "manual")), None);
        }
        assert_eq!(
            candidate(&example(1, "Call me at five.", "Call me at 5.", "manual")),
            None
        );
    }

    #[test]
    fn fingerprints_match_python() {
        let mut first = example(0, "Hi \"there\" it's", "Hi there", "manual");
        first.id = "a".into();
        first.capture_id = "c1".into();
        first.language = Some("en".into());
        let mut second = example(0, "x\ty", "z", "voice_fix");
        second.id = "b".into();
        second.capture_id = "c2".into();
        assert_eq!(
            fingerprint(&[first, second]),
            "970d8d524b7e36f250197b99d5b2a21988c8fbee778a1572cc33543452e69c82"
        );
        assert_eq!(
            fingerprint(&[]),
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        );
    }

    #[test]
    fn loss_counts_token_edits() {
        assert_eq!(loss("Hello, world.", "Hello, world."), 0);
        assert_eq!(loss("hello world", "Hello, world."), 3);
        assert_eq!(loss("", "a b"), 2);
    }

    #[test]
    fn a_rule_keeps_its_shape_in_json() {
        let rule = candidate(&example(
            1,
            "Commit, PR, merge, domain.",
            "Commit, PR, merge to main.",
            "manual",
        ))
        .unwrap();
        assert_eq!(Rule::from_json(&rule.to_json()), Some(rule));
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
    fn a_rule_learned_before_edges_with_only_its_anchors_changes_nothing() {
        let rules = Rules::new(vec![rule(&["voice", "box"], &["Voicebox"], None)]);
        assert_eq!(
            rules.apply("Please use voice box today.", None),
            "Please use voice box today."
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
