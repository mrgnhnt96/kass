//! Resolve explicit local clause revisions before generative refinement.

use crate::py::{self, Groups, Re, I, S};
use crate::re;

fn word_re() -> &'static Re {
    re!(r"\w+(?:['’]\w+)*")
}

const PRONOUN_VERBS: &[(&str, &str)] = &[
    ("it's", "is"),
    ("that's", "is"),
    ("it is", "is"),
    ("that is", "is"),
    ("it was", "was"),
    ("that was", "was"),
    ("they're", "are"),
    ("they are", "are"),
    ("they were", "were"),
];

/// Resolve 'X is A; no, no, it's B' when one local predicate is available.
fn replace_pronoun_value(clause: &str, revision: &str) -> Option<String> {
    let restated = re!(
        r"^(it['’]s|that['’]s|they['’]re|(?:it|that|they)\s+(?:is|are|was|were))\s+(\S.*)$",
        I | S
    )
    .match_(revision);
    let copulas = re!(r"\b(is|was|are|were)\s+", I).finditer(clause);
    let restated = restated?;
    if copulas.len() != 1 {
        return None;
    }
    let copula = &copulas[0];
    let pronoun = py::split(&py::lower(restated.g(1)).replace('\u{2019}', "'")).join(" ");
    let verb = PRONOUN_VERBS
        .iter()
        .find(|(said, _)| *said == pronoun)
        .map(|(_, verb)| *verb);
    if verb != Some(py::lower(copula.g(1)).as_str()) {
        return None;
    }
    let (head, previous) = (&clause[..copula.end()], &clause[copula.end()..]);
    let value = restated.g(2);
    if previous.is_empty()
        || re!(r"(?:not|also)\b", I).match_(value).is_some()
        || re!(r"not\b", I).match_(previous).is_some()
    {
        return None;
    }
    if re!(r"\b(?:said|says|asked|wrote|told|there)\b", I).is_match(head) {
        return None;
    }
    Some(format!("{head}{value}"))
}

fn words(text: &str) -> Vec<String> {
    word_re()
        .finditer(text)
        .iter()
        .map(|found| py::lower(&found.text().replace('\u{2019}', "'")))
        .collect()
}

/// Replace a final clause when an explicit cue repeats its subject/predicate.
///
/// Accept a shared two-word anchor, or a pronoun restating a single local
/// copular value. Never search past an intervening sentence/clause. Quoted
/// speech and ambiguous cues are left to ordinary refinement.
pub fn apply_spoken_corrections(text: &str) -> Option<String> {
    if ['"', '\u{201c}', '\u{201d}', '\u{2018}']
        .iter()
        .any(|mark| text.contains(*mark))
        || re!(r"(?<!\w)['’](?=\w)").is_match(text)
    {
        return None;
    }
    let cue_re = re!(
        r"\b(?:no(?:[,\s]+no)+(?:[,\s]+(?:actually|wait))?|no\s*,?\s*(?:actually|wait)|wait\s*,?\s*no|(?:sorry\s*,?\s*)?I meant?|or rather)\b[\s,]*",
        I
    );
    // Split coordinated clauses, but not objects such as "mac and cheese".
    let coordinate = re!(
        r"\b(?:and|but|or)\s+(?=(?:I|we|you|he|she|they|it|the)\b)",
        I
    );
    let mut result = text.to_string();
    let mut changed = false;
    for _ in 0..20 {
        let Some(cue) = cue_re.search(&result) else {
            return changed.then_some(result);
        };
        let before = py::rstrip_chars(&result[..cue.start()], " \t\r\n.,;:!?\u{2014}-").to_string();
        let after = py::strip(&result[cue.end()..]).to_string();
        let boundaries = re!(r"[.!?;\n]\s*").finditer(&before);
        let mut start = boundaries.last().map_or(0, |b| b.end());
        let clauses = coordinate.finditer_from(&before, start);
        if let Some(clause) = clauses.last() {
            start = clause.end();
        }
        if let Some(intro) =
            re!(r"(?:alright|all right|okay|ok|well|so)\s*,\s*", I).match_at(&before, start)
        {
            start = intro.end();
        }
        let old_clause = &before[start..];
        // A remaining conjunction/subclause may contain another fact. Without
        // a reliable clause boundary, let normal refinement interpret it.
        if re!(
            r"\b(?:and|but|or|while|whereas|although|because|which|who)\b|,\s*(?:I|we|you|he|she|they|it|my|your|our|the)\b",
            I
        )
        .is_match(old_clause)
        {
            return None;
        }
        if let Some(replacement) = replace_pronoun_value(old_clause, &after) {
            // Multiple coordinated clauses can provide competing antecedents.
            if !clauses.is_empty() {
                return None;
            }
            result = format!("{}{replacement}", &before[..start]);
            changed = true;
            continue;
        }
        let (old_words, new_words) = (words(old_clause), words(&after));
        let shared = old_words
            .iter()
            .zip(&new_words)
            .take_while(|(old, new)| old == new)
            .count();
        if shared < 2 || shared >= old_words.len().min(new_words.len()) {
            return None;
        }
        let mut replacement = after.clone();
        if start == 0 || boundaries.last().is_some_and(|b| start == b.end()) {
            replacement = crate::spoken_punctuation::upper_first(&replacement);
        }
        result = format!("{}{replacement}", &before[..start]);
        changed = true;
    }
    None
}

/// A bare replacement value ("Wednesday", "it's the blue one") is this short.
const REPLACEMENT_WORDS: usize = 4;
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "i", "i'm", "we", "you", "he", "she", "they", "it", "it's", "my", "our",
    "your", "their", "to", "of", "in", "on", "at", "for", "with", "and", "or", "but", "so", "is",
    "are", "was", "were", "be", "do", "did", "want", "need", "think", "that", "this", "there",
    "then", "just",
];

/// Whether `phrase` opens by repeating `anchor` or more earlier words, then
/// says something else in their place.
fn restates(phrase: &[String], earlier: &[String], anchor: usize) -> bool {
    let run = &phrase[..anchor.min(phrase.len())];
    if run.len() < anchor
        || run
            .iter()
            .all(|word| FUNCTION_WORDS.contains(&word.as_str()))
    {
        return false;
    }
    for start in 0..earlier.len().saturating_sub(anchor) {
        if earlier[start..start + anchor] != *run {
            continue;
        }
        let mut shared = anchor;
        while shared < phrase.len()
            && start + shared < earlier.len()
            && earlier[start + shared] == phrase[shared]
        {
            shared += 1;
        }
        if shared < phrase.len() && start + shared < earlier.len() {
            return true;
        }
    }
    false
}

/// What `phrase`, said after `earlier`, may take back: `earlier` and the cue;
/// None if it corrects nothing.
///
/// A retraction cue always corrects. A cue that can also open an answer or a
/// new thought ("No, unless...") does only when a short replacement value
/// follows, or the rest restates earlier words with a change. Without a cue
/// the phrase must restate three earlier words, one of them a content word,
/// and then differ.
pub fn retracted_by(phrase: &str, earlier: &str) -> Option<String> {
    let phrase = py::strip(phrase);
    // Cues that only ever take words back.
    let retraction = re!(
        r"\b(?:no[,\s]+(?:no|wait|actually)|wait[,\s]+no|scratch that|make that|I meant|or rather|sorry[,\s]+I mean)\b",
        I
    );
    if let Some(found) = retraction.search(phrase) {
        return Some(format!("{earlier} {}", &phrase[..found.end()]));
    }
    let previous = words(earlier);
    // Cues that open a correction, and also an answer or a new thought.
    let Some(cue) = re!(
        r"^(?:(?:no|nope|wait|sorry|oops|actually|I mean)\b[\s,.!-]*)+",
        I
    )
    .match_(phrase) else {
        return restates(&words(phrase), &previous, 3).then(|| earlier.to_string());
    };
    let rest = &phrase[cue.end()..];
    let said = words(rest);
    // After a cue, these continue the thought instead of replacing words.
    let continues = re!(
        r"(?:unless|if|because|but|and|so|since|though|although|when|until|that['’]s)\b",
        I
    );
    if said.is_empty() || continues.match_(rest).is_some() {
        return None;
    }
    if said.len() <= REPLACEMENT_WORDS || restates(&said, &previous, 2) {
        return Some(format!("{earlier} {}", &phrase[..cue.end()]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_a_corrected_clause() {
        assert_eq!(
            apply_spoken_corrections("the meeting is at noon, no, no, it's at one").as_deref(),
            Some("the meeting is at one")
        );
        assert_eq!(
            retracted_by("scratch that", "buy milk").as_deref(),
            Some("buy milk scratch that")
        );
    }
}
