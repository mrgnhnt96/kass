//! Resolve the mechanical patterns of speech before refinement.
//!
//! Repeats, restarts, changed answers and refined answers each have a shape
//! that can be rewritten the same way every time. Each rule fires on a tight
//! shape or not at all, and only ever deletes words the speaker said twice or
//! replaced; anything ambiguous is left for the refinement model.

use fancy_regex::Captures;

use crate::py::{self, Groups, Re, I};
use crate::re;

/// Words speakers stutter on. Content words and genuine doubles ("very,
/// very", "no, no, no", "had had", "that that", "is is") are deliberately absent.
const STUTTER: &str =
    "a an the this these those my your our their his her its some any to of in on at \
    for with from by about into i you we they he she it me us them and but or \
    because if when was are were can will would should could do did have has i'm \
    it's we're they're you're there's i've we've i'll we'll";
/// Words a finished phrase cannot end on, so a fragment ending on one was
/// abandoned. Not "a": in "item A, item B" it is a label, not an article.
const DANGLING: &str = "the my your our their his her its and but or because";
/// Verbs that need what is said or asked after them: "I just want to say," was abandoned.
const NEEDS_OBJECT: &str = "say ask tell mention add explain";
const SUBORDINATORS: &str = "when if because while where since until once unless whether";
const PRONOUNS: &str = "i you we they he she it";
const AUX: &str =
    "am is are was were will would can could did does do has have had 's 're 'm 've 'll 'd";
const CLAUSE_OPENERS: &str = "because and but so that when if since while although though";
const HEDGES: &str = "probably maybe";
const DETERMINERS: &str = "the a an this these those my your our their his her its some any";
const PREPOSITIONS: &str = "for to of in on at with from by about into";

fn has(words: &str, word: &str) -> bool {
    words.split(' ').any(|w| w == word)
}

fn norm(token: &str) -> String {
    py::lower(&token.replace('\u{2019}', "'"))
}

#[derive(Clone, Copy)]
struct Token<'t> {
    start: usize,
    end: usize,
    text: &'t str,
}

fn tokens(text: &str) -> Vec<Token<'_>> {
    re!(r"[\w$%]+(?:['’.:][\w%]+)*")
        .finditer(text)
        .iter()
        .map(|found| Token {
            start: found.start(),
            end: found.end(),
            text: found.text(),
        })
        .collect()
}

/// What separates token `index` from the next: "" (space), "," or other punctuation.
fn gap<'t>(text: &'t str, tokens: &[Token], index: usize) -> &'t str {
    py::strip(&text[tokens[index].end..tokens[index + 1].start])
}

fn clause_start(text: &str, tokens: &[Token], index: usize) -> usize {
    let mut start = index;
    while start > 0 && gap(text, tokens, start - 1).is_empty() {
        start -= 1;
    }
    start
}

fn clause_end(text: &str, tokens: &[Token], index: usize) -> usize {
    let mut end = index;
    while end + 1 < tokens.len() && gap(text, tokens, end).is_empty() {
        end += 1;
    }
    end
}

/// Delete `text[start..end]`, keeping a capital the deleted words started with.
fn drop(text: &str, start: usize, end: usize) -> String {
    let mut kept = text[end..].to_string();
    if py::isupper(py::head(&text[start..], 1)) && py::islower(py::head(&kept, 1)) {
        kept = crate::spoken_punctuation::upper_first(&kept);
    }
    format!("{}{kept}", &text[..start])
}

fn words_of(tokens: &[Token]) -> Vec<String> {
    tokens.iter().map(|t| norm(t.text)).collect()
}

/// Drop a stuttered word: "look at the, the budget" -> "look at the budget".
fn immediate_repeat(text: &str, tokens: &[Token]) -> Option<String> {
    for i in 0..tokens.len().saturating_sub(1) {
        let word = norm(tokens[i].text);
        if tokens[i].text == "A" {
            continue; // a label or grade ("A, A, B"), not the article
        }
        if has(STUTTER, &word)
            && ["", ","].contains(&gap(text, tokens, i))
            && norm(tokens[i + 1].text) == word
        {
            return Some(drop(text, tokens[i].start, tokens[i + 1].start));
        }
    }
    None
}

/// `range(from, to_exclusive, -1)` over signed indices.
fn down(from: isize, to_exclusive: isize) -> impl Iterator<Item = usize> {
    (to_exclusive + 1..=from).rev().map(|j| j as usize)
}

/// Drop an abandoned start: "I can, I can probably" -> "I can probably".
fn repeated_restart(text: &str, tokens: &[Token]) -> Option<String> {
    for i in 0..tokens.len().saturating_sub(1) {
        if gap(text, tokens, i) != "," {
            continue;
        }
        let start = clause_start(text, tokens, i);
        let end = clause_end(text, tokens, i + 1);
        let before = words_of(&tokens[start..=i]);
        let after = words_of(&tokens[i + 1..=end]);
        let largest = (before.len() as isize).min(after.len() as isize - 1).min(6);
        for size in down(largest, 1) {
            if before[before.len() - size..] == after[..size] {
                return Some(drop(text, tokens[i + 1 - size].start, tokens[i + 1].start));
            }
        }
        if after.len() < 2 {
            continue;
        }
        let (si, ii) = (start as isize, i as isize);
        for j in down(ii - 1, si.max(ii - 3) - 1) {
            if norm(tokens[j].text) != after[0] {
                continue;
            }
            let fragment = &before[j - start..];
            let abandoned = has(DANGLING, fragment.last().unwrap())
                || (fragment.len() == 2
                    && has(SUBORDINATORS, &fragment[0])
                    && has(PRONOUNS, &fragment[1]));
            if abandoned && fragment != prefix(&after, fragment.len()) {
                return Some(drop(text, tokens[j].start, tokens[i + 1].start));
            }
            break;
        }
        // A longer start, abandoned and begun again with the same words:
        // "I just want to say, I just want a dialogue" -> "I just want a dialogue".
        // "say" can end a phrase ("I told you to ask"), so it needs more words in common.
        let last = before.last().unwrap();
        let shared = if has(DANGLING, last) {
            2
        } else if has(NEEDS_OBJECT, last) {
            3
        } else {
            0
        };
        if shared == 0 || after.len() <= shared {
            continue;
        }
        for j in down(ii - 2, si.max(ii - 6) - 1) {
            let fragment = &before[j - start..];
            if prefix(fragment, shared) == prefix(&after, shared)
                && fragment != prefix(&after, fragment.len())
            {
                return Some(drop(text, tokens[j].start, tokens[i + 1].start));
            }
        }
    }
    None
}

/// `items[:n]`.
fn prefix<T>(items: &[T], n: usize) -> &[T] {
    &items[..n.min(items.len())]
}

/// Drop a phrase said again with new middle words: "that we can do, that we
/// need to do it" -> "that we need to do it".
///
/// The second attempt opens with the same two words, ends the phrase on the
/// same word and carries on past it, so it replaces the first rather than
/// listing a second thing ("what we built, what we shipped").
fn reworded_restart(text: &str, tokens: &[Token]) -> Option<String> {
    let words = words_of(tokens);
    for i in 0..tokens.len().saturating_sub(1) {
        if gap(text, tokens, i) != "," {
            continue;
        }
        let start = clause_start(text, tokens, i);
        let end = clause_end(text, tokens, i + 1);
        let after = &words[i + 1..=end];
        for j in down(i as isize - 2, (start as isize).max(i as isize - 6) - 1) {
            let fragment = &words[j..=i];
            if prefix(fragment, 2) != prefix(after, 2) || fragment == prefix(after, fragment.len())
            {
                continue;
            }
            let upto = (after.len() as isize - 1).min(fragment.len() as isize + 2);
            let last = (2..upto.max(2) as usize).find(|&m| after[m] == *fragment.last().unwrap());
            if last.is_some() {
                return Some(drop(text, tokens[j].start, tokens[i + 1].start));
            }
            break;
        }
    }
    None
}

fn plural(word: &str) -> bool {
    py::len(word) > 3
        && word.ends_with('s')
        && !(word.ends_with("ss") || word.ends_with("us") || word.ends_with("is"))
}

/// Drop a restart with no pause: "the tasks for tasks that" -> "the tasks that".
///
/// "X for X" idioms ("word for word", "one on one", "end to end") repeat a
/// singular noun, so only a plural one is taken as a restart.
fn unpaused_restart(text: &str, tokens: &[Token]) -> Option<String> {
    let words = words_of(tokens);
    for i in 1..tokens.len().saturating_sub(2) {
        let noun = &words[i];
        if !has(DETERMINERS, &words[i - 1])
            || !has(PREPOSITIONS, &words[i + 1])
            || words[i + 2] != *noun
            || has(STUTTER, noun)
            || !plural(noun)
            || !gap(text, tokens, i - 1).is_empty()
            || !gap(text, tokens, i).is_empty()
            || !["", ","].contains(&gap(text, tokens, i + 1))
            || i + 3 >= tokens.len()
            || !gap(text, tokens, i + 2).is_empty()
        {
            continue;
        }
        return Some(drop(text, tokens[i].start, tokens[i + 2].start));
    }
    None
}

/// ("it", "'s") for "it's", ("it", "") for "it"; None when not a pronoun subject.
fn subject(token: &str) -> Option<(String, String)> {
    let (base, clitic) = token.split_once('\'').unwrap_or((token, ""));
    if !has(PRONOUNS, base) {
        return None;
    }
    let clitic = if clitic.is_empty() {
        String::new()
    } else {
        format!("'{clitic}")
    };
    Some((base.to_string(), clitic))
}

fn stem(word: &str) -> &str {
    let mut word = word;
    for suffix in ["ing", "ed", "es", "s"] {
        if word.ends_with(suffix) && py::len(word) >= 3 + suffix.len() {
            word = &word[..word.len() - suffix.len()];
            break;
        }
    }
    word.trim_end_matches('e')
}

/// The subject at `at` and the index of its main verb, skipping one auxiliary.
fn subject_verb(words: &[String], at: usize) -> Option<(String, usize)> {
    let subject = words.get(at).and_then(|word| subject(word))?;
    let mut verb = at + 1;
    if subject.1.is_empty() && verb < words.len() && has(AUX, &words[verb]) {
        verb += 1;
    }
    if verb >= words.len() || has(AUX, &words[verb]) {
        return None;
    }
    Some((subject.0, verb))
}

/// Words as spoken, so "it's loading" is three.
fn length(words: &[String]) -> usize {
    words.len() + words.iter().filter(|word| word.contains('\'')).count()
}

/// Keep the shorter clause: "it loads, it's loading everything" -> "it loads everything".
///
/// Contractions count as two words, and on a tie the restated clause wins.
fn stuttered_clause(text: &str, tokens: &[Token]) -> Option<String> {
    let words = words_of(tokens);
    for i in 0..tokens.len().saturating_sub(1) {
        if gap(text, tokens, i) != "," {
            continue;
        }
        let end = clause_end(text, tokens, i + 1);
        let Some(second) = subject_verb(&words, i + 1) else {
            continue;
        };
        if second.1 >= end {
            continue;
        }
        let start = clause_start(text, tokens, i);
        for at in [i as isize - 1, i as isize - 2] {
            if at < start as isize {
                break;
            }
            let at = at as usize;
            let Some(first) = subject_verb(&words, at) else {
                continue;
            };
            if first.1 != i || first.0 != second.0 {
                continue;
            }
            if at > start && !has(CLAUSE_OPENERS, &words[at - 1]) {
                break;
            }
            let (first_verb, second_verb) = (&words[i], &words[second.1]);
            let mut restated = words[i + 1..second.1].to_vec();
            restated.push(first_verb.clone());
            if stem(first_verb) != stem(second_verb) || words[at..i] == restated[..] {
                break;
            }
            if length(&words[at..=i]) < length(&words[i + 1..=second.1]) {
                return Some(format!(
                    "{}{}",
                    &text[..tokens[i].end],
                    &text[tokens[second.1].end..]
                ));
            }
            return Some(drop(text, tokens[at].start, tokens[i + 1].start));
        }
    }
    None
}

// --- Changed answers --------------------------------------------------------------

pub(crate) const WEEKDAYS: &str = "monday|tuesday|wednesday|thursday|friday|saturday|sunday";
pub(crate) const MONTHS: &str =
    "january|february|march|april|may|june|july|august|september|october|november|december";
const NUMBER_WORDS: &str = "zero|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|thirteen|fourteen|\
    fifteen|sixteen|seventeen|eighteen|nineteen|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred|\
    thousand|noon|midnight";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Weekday,
    Month,
    Number,
    Name,
}

fn value_kinds() -> [(Kind, &'static Re); 4] {
    [
        (
            Kind::Weekday,
            re!(
                format!(r"(?:{WEEKDAYS})(?:\s+(?:morning|afternoon|evening|night))?\b"),
                I
            ),
        ),
        (
            Kind::Month,
            re!(
                format!(r"(?:{MONTHS})(?:\s+\d{{1,2}}(?:st|nd|rd|th)?)?\b"),
                I
            ),
        ),
        (
            Kind::Number,
            re!(
                format!(
                    r"(?:\$?\d+(?:[.,:]\d+)*%?|(?:{NUMBER_WORDS})(?:[\s-](?:{NUMBER_WORDS}))*)(?:\s*(?:am|pm|a\.m\.|p\.m\.|o'clock))?(?!\w|\.\d)"
                ),
                I
            ),
        ),
        (Kind::Name, re!(r"[A-Z][a-z]+\b")),
    ]
}

fn not_name(word: &str) -> bool {
    word == "I"
        || format!("{WEEKDAYS}|{MONTHS}")
            .split('|')
            .any(|name| py::title(name) == word)
}

/// The cue may follow a sentence break too: speech-to-text often hears
/// "noon? No, actually, one" rather than "noon, no actually one". Where the
/// speaker paused there, the mark is gone (`continue_after_seam`) and only
/// the capital it gave the cue is left: "noon No, actually one".
const PAUSED_CUE: &str = r"\s+(?=(?-i:(?:Or|No|Sorry|Actually)\b))";
const AUX_AFTER: &str =
    r"\s+(?:is|are|was|were|has|have|had|will|would|can|could|should|does|did)\b";

fn value_at<'t>(text: &'t str, pos: usize) -> Option<(Kind, Captures<'t>)> {
    if let Some(previous) = py::final_char(&text[..pos]) {
        if py::is_alnum(previous) || "$.'\u{2019}".contains(previous) {
            return None;
        }
    }
    for (kind, pattern) in value_kinds() {
        if let Some(found) = pattern.match_at(text, pos) {
            if !(kind == Kind::Name && not_name(found.text())) {
                return Some((kind, found));
            }
        }
    }
    None
}

fn spaced_lower(text: &str) -> String {
    py::split(&py::lower(text)).join(" ")
}

/// Keep the final answer: "said Wednesday, or was it Tuesday, no Wednesday" -> "said Wednesday".
fn changed_answer(text: &str) -> Option<String> {
    let answer_cue = re!(
        format!(
            r"(?:[,.?!]\s*|{PAUSED_CUE})(?:or\s+(?:was|is)\s+it|(?:no|sorry)(?:\s*,?\s*(?:actually|wait|I\s+mean))?|actually|I\s+mean)\s*,?\s*"
        ),
        I
    );
    // A value followed by a verb starts a new clause ("5 is too many"), not an answer.
    let aux_after = re!(AUX_AFTER, I);
    // Names take any verb ("Sarah knows"); a number is followed by plural nouns ("4.5 seconds").
    let verb_after_name = re!(format!(r"{AUX_AFTER}|\s+\w+(?:ed|s)\b"), I);
    let tail_re = re!(format!(r"(?:\s+[a-z]+){{0,3}}?(?=[,.?!]|{PAUSED_CUE})"));
    for (start, _) in text.char_indices() {
        let Some((kind, value)) = value_at(text, start) else {
            continue;
        };
        if kind == Kind::Name && (start == 0 || re!(r"[.!?]\s*$").is_match(&text[..start])) {
            continue; // capitalized because it starts a sentence
        }
        let Some(tail) = tail_re.match_at(text, value.end()) else {
            continue;
        };
        let mut answers: Vec<Captures> = Vec::new();
        let mut pos = tail.end();
        while let Some(cue) = answer_cue.match_at(text, pos) {
            let Some((answer_kind, answer)) = value_at(text, cue.end()) else {
                break;
            };
            if answer_kind != kind {
                break;
            }
            if py::lower(py::strip_chars(cue.text(), " ,.?!")) == "no"
                && py::lower(answer.text()).starts_with("one")
            {
                break; // "no one"
            }
            pos = answer.end();
            answers.push(answer);
        }
        let Some(last) = answers.last() else { continue };
        if (kind == Kind::Name && verb_after_name.match_at(text, last.end()).is_some())
            || (kind == Kind::Number && aux_after.match_at(text, last.end()).is_some())
        {
            continue;
        }
        if spaced_lower(last.text()) == spaced_lower(value.text()) {
            return Some(format!("{}{}", &text[..tail.end()], &text[last.end()..]));
        }
        if tail.end() == value.end() {
            return Some(format!("{}{}", &text[..start], &text[last.start()..]));
        }
    }
    None
}

/// Keep the fuller answer: "ship Thursday, well Thursday morning probably" -> "ship Thursday morning".
fn refined_answer(text: &str, tokens: &[Token]) -> Option<String> {
    let words = words_of(tokens);
    for i in 0..tokens.len().saturating_sub(2) {
        if gap(text, tokens, i) != ","
            || words[i + 1] != "well"
            || !["", ","].contains(&gap(text, tokens, i + 1))
        {
            continue;
        }
        let mut after = i + 2;
        if has(HEDGES, &words[after])
            && after + 1 < tokens.len()
            && gap(text, tokens, after).is_empty()
        {
            after += 1;
        }
        let start = clause_start(text, tokens, i);
        let end = clause_end(text, tokens, after);
        for size in down((i + 1 - start).min(3) as isize, 0) {
            let said = &words[after.min(words.len())..(after + size).min(words.len())];
            if words[i + 1 - size..=i] != *said || after + size > end {
                continue;
            }
            let mut result = text.to_string();
            if has(HEDGES, &words[end]) && end > after + size {
                result = format!(
                    "{}{}",
                    &result[..tokens[end - 1].end],
                    &result[tokens[end].end..]
                );
            }
            return Some(drop(
                &result,
                tokens[i + 1 - size].start,
                tokens[after].start,
            ));
        }
    }
    None
}

/// Remove repeats, restarts and replaced answers; return `text` unchanged if none.
pub fn apply_spoken_cleanup(text: &str) -> String {
    let mut text = text.to_string();
    for _ in 0..50 {
        let tokens = tokens(&text);
        let rules: [&dyn Fn() -> Option<String>; 7] = [
            &|| immediate_repeat(&text, &tokens),
            &|| repeated_restart(&text, &tokens),
            &|| reworded_restart(&text, &tokens),
            &|| unpaused_restart(&text, &tokens),
            &|| stuttered_clause(&text, &tokens),
            &|| changed_answer(&text),
            &|| refined_answer(&text, &tokens),
        ];
        // Python's `a or b or ...`: the first non-empty result, else the last one.
        let mut cleaned = None;
        for rule in rules {
            cleaned = rule();
            if cleaned.as_deref().is_some_and(|c| !c.is_empty()) {
                break;
            }
        }
        let Some(cleaned) = cleaned else { break };
        text = cleaned;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_the_shapes_of_speech() {
        assert_eq!(
            apply_spoken_cleanup("look at the, the budget"),
            "look at the budget"
        );
        assert_eq!(
            apply_spoken_cleanup("I can, I can probably do it"),
            "I can probably do it"
        );
        assert_eq!(
            apply_spoken_cleanup("the tasks for tasks that matter"),
            "the tasks that matter"
        );
        assert_eq!(
            apply_spoken_cleanup("we said Wednesday, or was it Tuesday, no Wednesday"),
            "we said Wednesday"
        );
    }
}
