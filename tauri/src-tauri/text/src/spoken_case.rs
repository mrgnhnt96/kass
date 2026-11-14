//! Write a word in the case the speaker asked for.
//!
//! Said after the word, with Whisper's pause before it: "I love, in all caps"
//! or "I love, with a capital L-O-V-E" is "I LOVE"; "I love, with a capital L"
//! is "I Love"; "in lowercase" and "with a lowercase N-A-S-A" lower it. Said
//! just before the word: "I all caps love you" is "I LOVE you". Around words:
//! "I am all caps yelling end caps at you" is "I am YELLING at you". Otherwise
//! the case asked for applies to the one word it is said next to. Spelled
//! letters must spell that word ("with a capital L" must start it), so a
//! sentence about capitals is left alone; so is case that is talked about ("I
//! wrote it in all caps", "all caps is hard to read").

use fancy_regex::Captures;

use crate::py::{self, Groups, I};
use crate::re;

const LETTER: &str = r"[^\W\d_]";
const UPPER: &str = r"caps|capitals|capital letters|block capitals|block letters|upper[ -]?case(?: letters)?";
const LOWER: &str = r"lower[ -]?case(?: letters)?|small letters";
/// Whisper's pause between the word and the ask: a comma, a dash, an
/// ellipsis or the ask as its own sentence.
const PAUSE: &str = r"[ \t]*(?:[,;:]|\.{3}|…|[.!?]|[ \t][-–—])[ \t]*";
const END: &str = r"(?=[ \t]*(?:[.!?,;:\n]|$))";

fn word() -> String {
    format!(r"{LETTER}[\w'’-]*")
}

fn ask() -> String {
    // "please", "that's" and "make it" around the ask are part of it.
    format!(
        r"(?:(?:make (?:that|it)|put (?:that|it)|that's|that is|write (?:that|it))\s+)?(?:all\s+in\s+|in\s+all\s+|in\s+|all\s+|with\s+)?(?:(?P<upper>{UPPER})|(?P<lower>{LOWER}))(?:,?\s+please)?"
    )
}

/// Words that say the text was written in a case rather than ask for it:
/// "I typed it, in all caps", "don't write in all caps".
const TALKED_ABOUT: &str = "it that this them those these everything something anything all one name text message \
    title word words in with using use the a an of type typed typing write wrote written writing shout shouting is \
    was are were be been being looks look reads read lock key";

fn talked_about(word: &str) -> bool {
    py::split(TALKED_ABOUT).contains(&py::lower(word).as_str())
}

fn cased(word: &str, upper: bool) -> String {
    if upper {
        py::upper(word)
    } else {
        py::lower(word)
    }
}

/// The sentence end the ask carried, when it was said as its own sentence.
fn rest(found: &Captures, text: &str) -> String {
    let pause = py::strip(found.n("pause"));
    let following = py::lstrip_chars(&text[found.end()..], " \t");
    match py::final_char(pause) {
        Some(end) if ".!?".contains(end) && following.is_empty() => end.to_string(),
        _ => String::new(),
    }
}

fn spelled(found: &Captures, text: &str) -> String {
    let word = found.n("word");
    let letters = re!(r"[\W_]").replace(found.n("letters"), "");
    let upper = !found.n("upper").is_empty();
    let written = if py::lower(&letters) == py::lower(&re!(r"[\W_]").replace(word, "")) {
        cased(word, upper)
    } else if py::len(&letters) == 1 && py::lower(py::head(word, 1)) == py::lower(&letters) {
        cased(py::head(word, 1), upper) + py::tail_from(word, 1)
    } else {
        return found.text().to_string();
    };
    written + &rest(found, text)
}

fn after(found: &Captures, text: &str) -> String {
    let word = found.n("word");
    let pause = py::strip(found.n("pause"));
    if talked_about(word) {
        return found.text().to_string();
    }
    let following = py::head(py::lstrip_chars(&text[found.end()..], " \t"), 1);
    // "Don't shout. In all caps, it reads as yelling." starts a new sentence.
    if py::in_chars(py::final_char(pause), ".!?") && !["", ".", "!", "?", "\n"].contains(&following) {
        return found.text().to_string();
    }
    cased(word, !found.n("upper").is_empty()) + &rest(found, text)
}

fn before(found: &Captures, text: &str) -> String {
    let word = found.n("word");
    let said_before = re!(r"(\w+)[ \t,]*$").search(&text[..found.start()]);
    if talked_about(word) || said_before.is_some_and(|said| talked_about(said.g(1))) {
        return found.text().to_string();
    }
    py::upper(word)
}

/// Write words in the case said next to them ("I love, in all caps").
pub fn apply_spoken_case(text: &str) -> String {
    if !re!(r"cap|case|letters|big|small", I).is_match(text) {
        return text.to_string();
    }
    // "with a capital L-O-V-E", "spelled with a capital L", "in lowercase n a s a".
    let spelled_case = re!(
        format!(
            r"(?P<word>\b{word})(?P<pause>{PAUSE}|[ \t]+)(?:(?:spelled|spelt|written)\s+)?(?:with\s+|in\s+)?(?:an?\s+)?(?:all\s+)?(?:(?P<upper>capital|caps|upper[ -]?case|big)|(?P<lower>lower[ -]?case|small))(?:\s+letters?)?\s+(?P<letters>{LETTER}+(?:(?:[ \t]*[,.-][ \t]*|[ \t]+){LETTER}+)*){END}",
            word = word()
        ),
        I
    );
    let case_after = re!(format!(r"(?P<word>\b{})(?P<pause>{PAUSE}){}{END}", word(), ask()), I);
    // "all caps yelling end caps": everything between is written in capitals.
    // Whisper's commas around either command are pauses, not text.
    let case_span = re!(
        r"\b(?:(?:start|begin)[ \t]+)?(?:all[ \t]+caps|caps[ \t]+on),?[ \t]+(?P<words>[^\n]+?),?[ \t]+(?:(?:end|stop)[ \t]+(?:all[ \t]+)?caps|caps[ \t]+off)\b",
        I
    );
    // "all caps love": the next word is written in capitals.
    let case_before = re!(
        format!(r"\b(?P<ask>all[ \t]+caps|all[ \t]+capitals),?[ \t]+(?P<word>{})", word()),
        I
    );
    let text = text.to_string();
    let text = spelled_case.sub(&text, |found| spelled(found, &text));
    let text = case_after.sub(&text, |found| after(found, &text));
    let text = case_span.sub(&text, |found| py::upper(found.n("words")));
    case_before.sub(&text, |found| before(found, &text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_the_word_asked_for() {
        assert_eq!(apply_spoken_case("I love, in all caps"), "I LOVE");
        assert_eq!(apply_spoken_case("I love, with a capital L"), "I Love");
        assert_eq!(apply_spoken_case("I am all caps yelling end caps at you"), "I am YELLING at you");
        assert_eq!(apply_spoken_case("I wrote it in all caps"), "I wrote it in all caps");
    }
}
