//! Conservative spoken formatting for complete, explicit dictation commands.
//!
//! Return `None` for ambiguous or unsupported wording so normal refinement
//! still runs. Only standalone commands are recognized; quoted or reported
//! speech is preserved.

use std::collections::{BTreeMap, BTreeSet};

use fancy_regex::Captures;

use crate::py::{self, Groups, Re};
use crate::re;

/// A command must start the transcript or follow sentence punctuation or a line break.
const BOUNDARY: &str = r"(?:^|(?<=[.!?\n])\s+)";
const LINE: &str = r"(?:add (?:a )?|insert (?:a )?)?new (?:line|paragraph)";
const LIST: &str = r"(?:create|make|add) (?:a )?(?:(?:bullet|bulleted) )?list of";
/// A spoken break: "new line", "newline", "line break", "new paragraph",
/// "next line", optionally led by a verb ("add a new line", "go to the next line").
const BREAK: &str = r"(?:new ?line(?: break)?|line break|new paragraph|paragraph break|next (?:line|paragraph))";
const LEAD: &str =
    r"(?:add|insert|start|put|make|begin|type|give me|go to|move to|skip to|jump to|drop to)\s+(?:(?:a|an|the|another)\s+)?";

/// Before a phrase with no lead verb, these make it something talked about:
/// "the next line", "on a new line", "our new line".
const TALKED_ABOUT: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "each", "every", "another", "one", "any", "some", "no", "our",
    "my", "your", "his", "her", "its", "their", "of", "on", "in", "at", "to", "into", "onto", "with", "for", "by",
    "from", "per", "what", "which", "whose",
];
/// Before "open quote" or "end quote", only these do: "the end quote".
const NAMED: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "each", "every", "another", "any", "some", "no", "our", "my",
    "your", "his", "her", "its", "their",
];
/// After a bare symbol, these make it a verb: "slash the budget", "pipe it".
const OBJECTS: &[&str] = &["it", "them", "me", "us", "him", "her", "you"];
/// Each pair's kind and its characters, in Python's dict order.
const PAIRS: &[(&str, &str)] = &[("quote", "\"\""), ("paren", "()"), ("curly", "{}"), ("angle", "<>"), ("square", "[]")];

fn start_command() -> &'static Re {
    re!(
        format!(
            r"{BOUNDARY}(?P<line>{LINE})(?:\s+and\s+then)?\s+(?P<list>{LIST})\s+|{BOUNDARY}(?P<list_only>{LIST})\s+"
        ),
        py::I
    )
}

/// A comma Whisper put before the command goes with it; sentence marks
/// stay. A quoted phrase ("new line") is words, not a command. A noun after
/// it ("a new line of products", "newline character") makes it words too.
fn break_command() -> &'static Re {
    re!(
        format!(
            r#"(?:[ \t]*,)?[ \t]*(?<![\w'\u2019"\u201c\u201d-])(?P<lead>{LEAD})?(?P<phrase>{BREAK})(?![\w'\u2019"\u201c\u201d-])(?P<noun>\s+(?:of|for|characters?|chars?|feeds?|endings?|separators?|symbols?|breaks?)\b)?[ \t]*[.,;:!]?[ \t]*"#
        ),
        py::I
    )
}

/// A spoken mark: "open quote", "end quote", "unquote", "open paren", "close
/// bracket", "open curly brace", "open angle bracket" or "open caret", or a
/// bare "quote", "parentheses" or "brackets" that opens and, said again,
/// closes. Or one symbol: "slash", "backslash", "pipe", "vertical bar", "caret".
fn mark_command() -> &'static Re {
    re!(
        concat!(
            r"(?<![\w'’-])(?:",
            r"(?:(?P<open>open|start|begin)\s+|(?P<close>close|end)\s+)?(?:",
            r"(?P<quote>quot(?:es?|ation marks?))",
            r"|(?P<paren>paren(?:s|thesis|theses)?|round brackets?)",
            r"|(?P<curly>(?:curly )?braces?|curly brackets?)",
            // Whisper spells "caret" as it sounds: "carrot".
            r"|(?P<angle>angle brackets?|(?P<caret>car(?:et|rot)s?)(?P<caret_noun>\s+(?:symbol|sign|character))?)",
            r"|(?P<square>(?:square )?brackets?)",
            r"|(?P<unquote>unquote))",
            r"|(?P<slash>(?:forward )?slash)|(?P<backslash>back ?slash)",
            r"|(?P<pipe>vertical bar|pipe(?P<pipe_noun>\s+(?:symbol|character))?)",
            r")(?![\w'’-])",
        ),
        py::I
    )
}

fn previous_word(text: &str) -> Option<String> {
    re!(r"([\w'’]+)\W*$").search(text).map(|found| py::lower(found.g(1)))
}

fn capitalized(text: &str) -> String {
    format!("{}{}", py::upper(py::head(text, 1)), py::tail_from(text, 1))
}

/// Apply a standalone list command with a clear, local edit target.
///
/// Line and paragraph breaks alone are left to [`apply_line_breaks`], so the
/// text around them still gets cleaned up.
///
/// List items must be comma-separated. Only a final list, optionally followed
/// by an explicit cancellation or last-item removal, is parsed. A mention of
/// a list inside prose, quotes, or more complex edits is left to refinement.
pub fn apply_dictation_edits(text: &str, formatting: bool, corrections: bool) -> Option<String> {
    if !formatting {
        return None;
    }
    // Quotation marks could make a command literal. Apostrophes in
    // contractions are fine, but quoted commands should never delete or
    // restructure prose.
    if ['"', '\u{201c}', '\u{201d}', '\u{2018}'].iter().any(|mark| text.contains(*mark))
        || re!(r"(?<!\w)['’](?=\w)").is_match(text)
    {
        return None;
    }

    let matches = start_command().finditer(text);
    if matches.len() != 1 {
        return None;
    }
    let found = &matches[0];
    let prefix = py::rstrip(&text[..found.start()]);
    let tail = py::strip(&text[found.end()..]);
    // Keep parsing deliberately bounded: no sentence punctuation in items.
    let parts = re!(r"[.!?]\s+").splitn(tail, 1);
    let items_text = py::strip(py::rstrip_chars(parts[0], ".!?"));
    let revision = if parts.len() == 2 { py::strip(parts[1]) } else { "" };
    let cancel = !revision.is_empty()
        && re!(r"(?:and\s+)?(?:actually\s*,?\s*(?:no\s*,?\s*)?|no\s*,?\s*)?(?:remove|delete)\s+(?:that|the)\s+list[.!]?\s*$", py::I)
            .fullmatch(revision)
            .is_some();
    let remove_last = !revision.is_empty()
        && re!(r"(?:and\s+)?(?:actually\s*,?\s*(?:no\s*,?\s*)?)?(?:remove|delete)\s+the\s+last\s+item[.!]?\s*$", py::I)
            .fullmatch(revision)
            .is_some();
    if !revision.is_empty() && !((cancel || remove_last) && corrections) {
        return None;
    }
    if items_text.chars().any(|c| ".!?\n".contains(c)) {
        return None;
    }
    let mut items: Vec<String> = items_text.split(',').map(|item| py::strip(item).to_string()).collect();
    if items.len() < 2 || items.iter().any(String::is_empty) {
        return None;
    }
    // Oxford comma or a final "B and C" are the supported list separators.
    let last = re!(r"^and\s+", py::I).replace(&items.pop().unwrap(), "");
    items.extend(re!(r"\s+and\s+", py::I).splitn(&last, 1).into_iter().map(String::from));
    if items.iter().any(String::is_empty) {
        return None;
    }
    if cancel {
        // Empty refinement results are not supported by captures; don't
        // manufacture a replacement when the entire take was retracted.
        return (!prefix.is_empty()).then(|| prefix.to_string());
    }
    if remove_last {
        items.pop();
    }
    let lines: Vec<String> = items.iter().map(|item| format!("- {}", capitalized(item))).collect();
    let lines = lines.join("\n");
    let separator = if py::lower(found.n("line")).contains("paragraph") { "\n\n" } else { "\n" };
    Some(if prefix.is_empty() { lines } else { format!("{prefix}{separator}{lines}") })
}

fn is_command(text: &str, found: &Captures) -> bool {
    if !found.n("noun").is_empty() {
        return false;
    }
    if !found.n("lead").is_empty() {
        return true;
    }
    let before = py::rstrip_chars(&text[..found.start()], " \t");
    match py::final_char(before) {
        None => return true,
        Some(last) if ".!?:;\n".contains(last) => return true,
        _ => {}
    }
    // "next line" alone mid-sentence is usually read out, not a command.
    if py::lower(found.n("phrase")).starts_with("next") {
        return false;
    }
    !previous_word(before).is_some_and(|word| TALKED_ABOUT.contains(&word.as_str()))
}

/// Turn spoken line and paragraph breaks into `\n` and `\n\n`.
///
/// "new line", "newline", "line break", "new paragraph", "next line" and the
/// like, alone or led by a verb ("add a new line"), anywhere in the text. One
/// that is talked about ("a new line of products", "the next line", "the
/// newline character") stays as words. The line after a break starts with a
/// capital. A break at the very start or end is kept: the text then starts or
/// ends on a new line.
pub fn apply_line_breaks(text: &str) -> String {
    if !re!(r"line|paragraph", py::I).is_match(text) {
        return text.to_string();
    }
    let mut pieces: Vec<&str> = Vec::new();
    let mut last = 0;
    for found in break_command().finditer(text) {
        if !is_command(text, &found) {
            continue;
        }
        pieces.push(&text[last..found.start()]);
        pieces.push(if py::lower(found.n("phrase")).contains("paragraph") { "\n\n" } else { "\n" });
        last = found.end();
    }
    if pieces.is_empty() {
        return text.to_string();
    }
    pieces.push(&text[last..]);
    let joined = pieces.concat();
    // Two breaks in a row ("new line new line") make a paragraph, no more.
    let joined = re!(r"\n{3,}").replace(&joined, "\n\n");
    let joined = py::strip_chars(&joined, " \t");
    re!(r"(?<=\n)([^\S\n]*)(\w)").sub(joined, |found| format!("{}{}", found.g(1), py::upper(found.g(2))))
}

/// The pair a mark belongs to, or `None` for a single symbol.
fn kind(found: &Captures) -> Option<&'static str> {
    if !found.n("unquote").is_empty() {
        return Some("quote");
    }
    if !found.n("caret").is_empty() && found.n("open").is_empty() && found.n("close").is_empty() {
        return None;
    }
    PAIRS.iter().map(|(kind, _)| *kind).find(|kind| !found.n(kind).is_empty())
}

fn pair(kind: &str) -> &'static str {
    PAIRS.iter().find(|(name, _)| *name == kind).unwrap().1
}

fn talked_about(text: &str, found: &Captures, closes: bool) -> bool {
    let Some(previous) = previous_word(&text[..found.start()]) else { return false };
    // "sort of close paren" and "the new one parentheses" close what was
    // opened; "in parentheses" is words.
    let named = !found.n("open").is_empty() || !found.n("close").is_empty() || !found.n("unquote").is_empty() || closes;
    (if named { NAMED } else { TALKED_ABOUT }).contains(&previous.as_str())
}

/// The symbol a bare "slash", "pipe" or "caret" is, or `None` for a word.
///
/// "pipe symbol", "vertical bar" and "caret sign" are, unless talked about
/// ("a vertical bar"). A bare one needs a word after it that is not its
/// object ("ls pipe grep", not "pipe it"), and "carrot" alone is a vegetable.
fn symbol(text: &str, found: &Captures) -> Option<&'static str> {
    let (symbol, spelled, certain) = if !found.n("caret").is_empty() {
        ("^", py::lower(found.n("caret")) == "caret", !found.n("caret_noun").is_empty())
    } else if !found.n("pipe").is_empty() {
        ("|", true, !found.n("pipe_noun").is_empty() || !py::lower(found.n("pipe")).starts_with("pipe"))
    } else {
        (if found.n("slash").is_empty() { "\\" } else { "/" }, true, false)
    };
    if talked_about(text, found, false) {
        return None;
    }
    if certain {
        return Some(symbol);
    }
    if !spelled {
        return None;
    }
    let following = re!(r"\W*([\w'’]+)").match_at(text, found.end())?;
    let word = py::lower(following.g(1));
    if TALKED_ABOUT.contains(&word.as_str()) || OBJECTS.contains(&word.as_str()) {
        return None;
    }
    Some(symbol)
}

/// Whether the mark is the last word of a sentence with words before it.
///
/// Such a mark opens nothing ("can we infer quotes?"); one said as its own
/// sentence ("He said. Quote. I'm tired.") is Whisper's pause after it.
fn ends_sentence(text: &str, found: &Captures) -> bool {
    let before = py::rstrip_chars(&text[..found.start()], " \t,");
    py::final_char(before).is_some_and(|last| !".!?:;\n".contains(last))
        && re!(r"[ \t]*(?:[.!?]|$)").match_(&text[found.end()..]).is_some()
}

/// What each command match becomes, by index: "open", "close" or a symbol.
///
/// A bare "quote" or "parentheses" is a command only when it has a partner:
/// one alone ("get a quote", "quote me on that") is a word. "open quote" or
/// "end quote" is a command on its own. "quote unquote" is an idiom, not a
/// pair of marks around nothing.
fn mark_roles(text: &str, matches: &[Captures]) -> BTreeMap<usize, &'static str> {
    let mut roles = BTreeMap::new();
    let mut bare = BTreeSet::new();
    let mut opened: Vec<(&str, Vec<usize>)> = PAIRS.iter().map(|(kind, _)| (*kind, Vec::new())).collect();
    for (index, found) in matches.iter().enumerate() {
        let Some(kind) = kind(found) else {
            if let Some(symbol) = symbol(text, found) {
                roles.insert(index, symbol);
            }
            continue;
        };
        let open = &mut opened.iter_mut().find(|(name, _)| *name == kind).unwrap().1;
        if talked_about(text, found, !open.is_empty()) {
            continue;
        }
        let explicit_close = !found.n("close").is_empty() || !found.n("unquote").is_empty();
        let explicit_open = !found.n("open").is_empty();
        if explicit_open || !(explicit_close || !open.is_empty()) {
            if !explicit_open && ends_sentence(text, found) {
                continue;
            }
            roles.insert(index, "open");
            open.push(index);
            if !explicit_open {
                bare.insert(index);
            }
            continue;
        }
        roles.insert(index, "close");
        if let Some(partner) = open.pop() {
            if !re!(r"\w").is_match(&text[matches[partner].end()..found.start()]) {
                roles.remove(&partner);
                roles.remove(&index);
            }
        }
    }
    for (_, indexes) in opened {
        for index in indexes {
            if bare.contains(&index) {
                roles.remove(&index);
            }
        }
    }
    roles
}

/// Turn spoken quotes, brackets and symbols into the characters.
///
/// Pairs: "open quote" / "end quote" / "unquote", "open paren" / "close
/// paren", "open bracket", "open curly brace", "open angle bracket" or "open
/// caret", and the like; or a bare "quote", "parentheses", "brackets" said
/// once to open and again to close. An opening mark sits against the word
/// after it and a closing mark against the word before it. Symbols: "slash"
/// and "backslash" join the words around them ("and/or"), "caret" too
/// ("x^2"); "pipe" or "vertical bar" stands between them ("ls | grep"). A
/// mark that is talked about ("a quote", "in parentheses", "slash the
/// budget") or a bare pair word with no partner stays as words.
pub fn apply_spoken_marks(text: &str) -> String {
    if !re!(r"quot|paren|brac|slash|pipe|bar|car", py::I).is_match(text) {
        return text.to_string();
    }
    let matches = mark_command().finditer(text);
    let roles = mark_roles(text, &matches);
    if roles.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut last = 0;
    for (index, found) in matches.iter().enumerate() {
        let Some(&role) = roles.get(&index) else { continue };
        out.push_str(&text[last..found.start()]);
        last = found.end();
        let rest = &text[last..];
        if role != "open" && role != "close" {
            // Whisper's commas around a symbol are pauses, not text.
            out = py::rstrip_chars(&out, " \t,").to_string();
            if role == "|" {
                out.push_str(if py::final_char(&out).is_some_and(|c| c != '\n') { " | " } else { "| " });
            } else {
                out.push_str(role);
            }
            last += rest.len() - py::lstrip_chars(rest, " \t,").len();
            continue;
        }
        let pair = pair(kind(found).unwrap());
        let (opening, closing) = (&pair[..1], &pair[1..]);
        if role == "open" {
            out = py::rstrip_chars(&out, " \t").to_string();
            if pair != "\"\"" {
                // "the plan, open paren": the comma was Whisper's pause.
                out = py::rstrip_chars(py::rstrip_chars(&out, ","), " \t").to_string();
            }
            if py::final_char(&out).is_some_and(|c| !"\n\"([{<".contains(c)) {
                out.push(' ');
            }
            out.push_str(opening);
            // Whisper's pause after the command ("quote, I'm") goes with it.
            last += rest.len() - py::lstrip_chars(rest, " \t,.;:").len();
            continue;
        }
        out = py::rstrip_chars(py::rstrip_chars(py::rstrip_chars(&out, " \t"), ","), " \t").to_string() + closing;
        let mut spaces = rest.len() - py::lstrip_chars(rest, " \t").len();
        let rest = &rest[spaces..];
        if out.chars().rev().nth(1).is_some_and(|c| ".!?".contains(c)) && rest.starts_with('.') {
            // "tired. End quote." has its period inside the mark already.
            spaces += 1;
        } else if py::first(rest).is_some_and(py::is_alnum) {
            out.push(' ');
        }
        last += spaces;
    }
    out + &text[last..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_list() {
        let text = "Groceries. Make a list of eggs, milk, and bread.";
        assert_eq!(apply_dictation_edits(text, true, false).unwrap(), "Groceries.\n- Eggs\n- Milk\n- Bread");
        assert_eq!(apply_dictation_edits(text, false, false), None);
    }

    #[test]
    fn breaks_lines() {
        assert_eq!(apply_line_breaks("Hi Sam, new line, thanks"), "Hi Sam\nThanks");
        assert_eq!(apply_line_breaks("a new line of products"), "a new line of products");
    }

    #[test]
    fn writes_marks() {
        assert_eq!(apply_spoken_marks("he said open quote hi end quote"), "he said \"hi\"");
        assert_eq!(apply_spoken_marks("ls pipe grep"), "ls | grep");
        assert_eq!(apply_spoken_marks("get a quote"), "get a quote");
    }
}
