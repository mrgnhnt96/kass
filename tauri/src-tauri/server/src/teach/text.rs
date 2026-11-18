//! What a reply shows about how the user writes, and the other side's next
//! turn: the prompt that asks for it and how its answer is read.

use kass_text::py;
use kass_text::re;
use serde::Serialize;

use super::catalog;

pub const MAX_CHIPS_PER_REPLY: usize = 5;
pub const MAX_MESSAGE_CHARS: usize = 1200;
/// Questions and answers a note lists at most.
const MAX_ANSWERS: usize = 4;

/// Something a reply showed about how the user writes; the app words `code`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Chip {
    pub code: &'static str,
    /// The text the chip points at, if any.
    pub value: Option<String>,
}

/// The facts for the user's next reply, so they only choose the words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Note {
    pub facts: Option<String>,
    pub answers: Vec<Answer>,
    /// A teaching trick to try, with an example when the opener has one.
    pub trick: Option<&'static str>,
    pub example: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub ask: String,
    pub answer: String,
}

impl Note {
    pub fn of(seed: &catalog::Seed) -> Note {
        Note {
            facts: seed.facts.map(str::to_string),
            answers: seed
                .answers
                .iter()
                .map(|(ask, answer)| Answer {
                    ask: ask.to_string(),
                    answer: answer.to_string(),
                })
                .collect(),
            trick: catalog::noted_trick(seed.trick),
            example: seed.example.map(str::to_string),
        }
    }
}

const ABBREVIATIONS: [&str; 14] = [
    "lmk", "tbh", "np", "btw", "fyi", "imo", "idk", "thx", "omw", "rn", "asap", "tho", "pls", "ty",
];

fn retraction(text: &str) -> bool {
    re!(
        r"\b(no wait|actually|scratch that|i mean|or rather|no no)\b",
        py::I
    )
    .is_match(text)
}

/// The first `n` characters, as Python's `text[:n]`.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// What a reply shows about how the user writes, as codes the app words.
pub fn reply_chips(written: &str, said: Option<&str>, kind: &str, terms: &[String]) -> Vec<Chip> {
    let mut chips: Vec<Chip> = Vec::new();
    let mut add = |code: &'static str, value: Option<String>| {
        if chips.len() < MAX_CHIPS_PER_REPLY && chips.iter().all(|chip| chip.code != code) {
            chips.push(Chip { code, value });
        }
    };
    let text = py::strip(written);
    let lines: Vec<&str> = py::splitlines(text)
        .into_iter()
        .map(py::strip)
        .filter(|line| !line.is_empty())
        .collect();
    let words = py::split(text);
    if lines.is_empty() {
        return chips;
    }
    if said.is_some_and(|said| !said.is_empty() && retraction(said)) && !retraction(text) {
        add("correction_resolved", None);
    }
    if let Some(term) = terms.iter().find(|term| text.contains(term.as_str())) {
        add("term", Some(term.clone()));
    }
    if let Some(token) = re!(r"\b\w+(?:[_./]\w+)+\b|\b[a-z]+[A-Z]\w*\b").search(text) {
        add("kept_exact", Some(token[0].to_string()));
    }
    let mut body: &[&str] = &lines;
    let greeting = re!(
        r"^(hi|hey|hello|dear|morning|good (morning|afternoon|evening)|yo)\b",
        py::I
    );
    if greeting.match_(lines[0]).is_some() {
        add("greeting", Some(head(lines[0], 40)));
        body = &body[1..];
    } else if ["team_chat", "text_message", "email", "issue_comment"].contains(&kind) {
        add("no_greeting", None);
    }
    let sign_off = re!(
        r"^(thanks|thank you|cheers|best|regards|thx|ty|talk soon|see you)\b[\w\s,!.]{0,30}$",
        py::I
    );
    let last = lines[lines.len() - 1];
    if lines.len() > 1 && sign_off.match_(last).is_some() {
        add("sign_off", Some(head(last, 40)));
        body = &body[..body.len().saturating_sub(1)];
    }
    let list_line = re!(r"^\s*(?:[-*•]|\d+[.)])\s+");
    if body.iter().any(|line| list_line.match_(line).is_some()) {
        add("list", None);
    } else if body.len() > 1 {
        add("line_per_thought", None);
    }
    if text
        .chars()
        .find(|c| py::is_alpha(*c))
        .is_some_and(|c| py::islower(&c.to_string()))
    {
        add("lowercase_start", None);
    }
    if words.len() >= 3 && py::final_char(text).is_some_and(py::is_alnum) {
        add("no_final_period", None);
    }
    let lowered: Vec<String> = words
        .iter()
        .map(|word| py::casefold(py::strip_chars(word, ".,!?")))
        .collect();
    if let Some(found) = ABBREVIATIONS
        .iter()
        .find(|abbreviation| lowered.iter().any(|word| word == *abbreviation))
    {
        add("abbreviation", Some(found.to_string()));
    }
    if let Some(emoji) = re!("[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]").search(text) {
        add("emoji", Some(emoji[0].to_string()));
    }
    if words.len() <= 12 && lines.len() == 1 {
        add("short", None);
    } else if re!(r"[.!?]\s")
        .split(text)
        .iter()
        .any(|sentence| py::split(sentence).len() >= 30)
    {
        add("run_on", None);
    }
    chips
}

/// The system prompt for the other side's next message.
pub fn system(
    persona: &str,
    kind: &catalog::Kind,
    trick: &str,
    terms: &[String],
    choose: impl FnOnce(usize) -> usize,
) -> String {
    let medium = kind.medium;
    let mut lines = vec![
        format!(
            "You play {persona}, {}, in a conversation with the user. The user is practicing {medium}.",
            kind.other_side
        ),
        format!(
            "Write {persona}'s next message: answer what the user just wrote, then keep the \
             conversation going with a question or request the user needs to answer. Stay in character. \
             Never mention practice, Kass or AI. Match how people really write \
             {medium}: keep it short. Never repeat an earlier message: if the user left something \
             unanswered, ask about just that part in new words."
        ),
    ];
    if trick == "terms" && !terms.is_empty() {
        lines.push(format!("Mention {} naturally.", terms[choose(terms.len())]));
    } else if let Some(instruction) = catalog::trick_instruction(trick) {
        lines.push(instruction.to_string());
    }
    lines.push(format!(
        "Then give the facts the user needs for their answer, so they only choose the words.\n\
         Answer with exactly two parts:\n\
         MESSAGE: <{persona}'s next message>\n\
         TELL: <for each thing you asked, `question -> short answer`, separated by ` | `; \
         or one short fact to pass on>"
    ));
    lines.join("\n\n")
}

/// The conversation so far, as the prompt the model answers.
pub fn transcript(persona: &str, messages: &[(bool, String)]) -> String {
    messages
        .iter()
        .map(|(from_you, text)| format!("{}: {text}", if *from_you { "User" } else { persona }))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn clean_part(text: &str) -> String {
    py::strip(py::strip_chars(py::strip(text), "`\"'“”<>")).to_string()
}

/// The message and note in the model's answer, or None when it isn't usable.
pub fn parse_turn(output: &str) -> Option<(String, Note)> {
    let output = re!(r"<think>.*?</think>", py::S).replace(output, "");
    let output = py::strip(&output);
    let parts = re!(
        r"MESSAGE:\s*(?P<message>.*?)\s*\n\s*TELL:\s*(?P<tell>.*)",
        py::S | py::I
    );
    let found = parts.search(output)?;
    let message = clean_part(found.name("message").map_or("", |m| m.as_str()));
    let tell = py::strip(found.name("tell").map_or("", |m| m.as_str()));
    let tell = clean_part(py::splitlines(tell).first().copied().unwrap_or(""));
    if message.is_empty()
        || py::len(&message) > MAX_MESSAGE_CHARS
        || re!(r"\b(MESSAGE|TELL):").is_match(&message)
    {
        return None;
    }
    let pair = re!(r"(?P<ask>\S.*?)\s*(?:->|→)\s*(?P<answer>\S.*)");
    let mut answers = Vec::new();
    let mut facts: Option<String> = None;
    for part in tell
        .split('|')
        .map(py::strip)
        .filter(|part| !part.is_empty())
    {
        if let Some(found) = pair.match_(part) {
            answers.push(Answer {
                ask: clean_part(&found["ask"]),
                answer: clean_part(&found["answer"]),
            });
        } else if facts.is_none() {
            facts = Some(part.to_string());
        }
    }
    if answers.is_empty() && facts.is_none() {
        return None;
    }
    answers.truncate(MAX_ANSWERS);
    let facts = if answers.is_empty() { facts } else { None };
    Some((
        message,
        Note {
            facts,
            answers,
            trick: None,
            example: None,
        },
    ))
}

fn normalized(text: &str) -> String {
    let kept = re!(r"[^\w\s]").replace(&py::casefold(text), "");
    py::split(&kept).join(" ")
}

/// Whether `message` is one the other side already sent (small models copy
/// their last one).
pub fn repeats_earlier(messages: &[(bool, String)], message: &str) -> bool {
    let said = normalized(message);
    messages
        .iter()
        .any(|(from_you, text)| !from_you && normalized(text) == said)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_TURN: &str = "MESSAGE: nice. can you post the release notes too?\nTELL: Post notes? -> Yes, Thursday | Who else? → Tag Jess";

    fn codes(chips: &[Chip]) -> Vec<(&str, Option<&str>)> {
        chips
            .iter()
            .map(|chip| (chip.code, chip.value.as_deref()))
            .collect()
    }

    #[test]
    fn a_turn_is_parsed_into_the_message_and_what_to_answer() {
        let (message, note) = parse_turn(GOOD_TURN).unwrap();
        assert_eq!(message, "nice. can you post the release notes too?");
        assert_eq!(
            note.answers,
            [
                Answer {
                    ask: "Post notes?".into(),
                    answer: "Yes, Thursday".into()
                },
                Answer {
                    ask: "Who else?".into(),
                    answer: "Tag Jess".into()
                },
            ]
        );
        assert_eq!(note.facts, None);
        let (message, note) =
            parse_turn("<think>hmm</think>MESSAGE: Hi,\n\nSure thing.\nTELL: Tuesday works.")
                .unwrap();
        assert_eq!(message, "Hi,\n\nSure thing.");
        assert_eq!(note.facts.as_deref(), Some("Tuesday works."));
        assert!(note.answers.is_empty());
        assert_eq!(parse_turn("Sure! Here's a message."), None);
        assert_eq!(parse_turn("MESSAGE: \nTELL: x"), None);
        assert_eq!(parse_turn("MESSAGE: a\nTELL: "), None);
        assert_eq!(parse_turn("MESSAGE: says MESSAGE: twice\nTELL: x"), None);
    }

    #[test]
    fn chips_describe_what_a_reply_shows() {
        let chat = reply_chips(
            "yep will do, tag jess so support's in the loop",
            Some("yep no wait will do"),
            "team_chat",
            &[],
        );
        assert_eq!(
            codes(&chat)[..4],
            [
                ("correction_resolved", None),
                ("no_greeting", None),
                ("lowercase_start", None),
                ("no_final_period", None),
            ]
        );
        let email = reply_chips(
            "Hi Priya,\n\nTuesday morning works.\n\nThanks!",
            None,
            "email",
            &[],
        );
        assert_eq!(
            codes(&email),
            [
                ("greeting", Some("Hi Priya,")),
                ("sign_off", Some("Thanks!"))
            ]
        );
        let agent = reply_chips(
            "keep them\nclear capitalize_first too",
            None,
            "coding_agent",
            &["Kass".into()],
        );
        assert!(codes(&agent).contains(&("kept_exact", Some("capitalize_first"))));
        assert!(codes(&agent).contains(&("line_per_thought", None)));
        let text = reply_chips("omw, see you soon 🙂", None, "text_message", &[]);
        assert_eq!(
            codes(&text),
            [
                ("no_greeting", None),
                ("lowercase_start", None),
                ("abbreviation", Some("omw")),
                ("emoji", Some("🙂")),
                ("short", None),
            ]
        );
        let list = reply_chips("Three things:\n- one\n- two", None, "notes", &[]);
        assert!(codes(&list).contains(&("list", None)));
        assert!(reply_chips("   ", None, "email", &[]).is_empty());
    }

    #[test]
    fn a_repeat_ignores_case_and_punctuation() {
        let messages = vec![
            (false, "Want to grab lunch?".to_string()),
            (true, "sure".to_string()),
        ];
        assert!(repeats_earlier(&messages, "WANT TO GRAB LUNCH"));
        assert!(!repeats_earlier(&messages, "sure"));
    }

    #[test]
    fn the_prompt_names_the_trick_and_the_transcript_who_said_what() {
        let kind = catalog::kind("team_chat").unwrap();
        let system = system("Jess", kind, "terms", &["Kass".into()], |_| 0);
        assert!(
            system.starts_with("You play Jess, a coworker in a team chat app, in a conversation")
        );
        assert!(system.contains("\n\nMention Kass naturally.\n\n"));
        assert!(system.ends_with("or one short fact to pass on>"));
        let tone = super::system("Jess", kind, "tone", &[], |_| 0);
        assert!(tone.contains("Something has gone wrong or is urgent now"));
        let messages = vec![(false, "hi".to_string()), (true, "hey".to_string())];
        assert_eq!(transcript("Jess", &messages), "Jess: hi\n\nUser: hey");
    }
}
