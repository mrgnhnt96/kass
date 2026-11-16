//! Words as prosody.py reads them: keys, syllables, drawn-out spellings
//! and sentence ends. Python's `\w` is taken as a letter, a digit or `_`.

/// Hesitations ("the...", "a...") are drawn out too, and never written
/// "theee". Only a speaker's own rule may draw one out.
pub const FUNCTION_WORDS: [&str; 96] = [
    "a", "an", "the", "and", "or", "but", "nor", "so", "yet", "for", "of", "to", "in", "on", "at",
    "by", "with", "from", "into", "onto", "upon", "about", "as", "than", "then", "that", "this",
    "these", "those", "there", "here", "i", "me", "my", "mine", "you", "your", "yours", "he",
    "him", "his", "she", "her", "hers", "it", "its", "we", "us", "our", "ours", "they", "them",
    "their", "theirs", "who", "whom", "whose", "which", "what", "when", "where", "why", "how",
    "is", "am", "are", "was", "were", "be", "been", "being", "do", "does", "did", "have", "has",
    "had", "will", "would", "shall", "should", "can", "could", "may", "might", "must", "not", "no",
    "if", "um", "uh", "er", "ah", "oh", "like", "just",
];

pub fn is_function_word(key: &str) -> bool {
    FUNCTION_WORDS.contains(&key)
}

const VOWELS: &str = "aeiouy";
/// What may close a sentence after its mark: quotes and brackets.
const CLOSERS: &str = "\"')]\u{201d}\u{2019}";
/// A period after one of these ends the word, not the sentence.
const ABBREVIATIONS: [&str; 14] = [
    "mr", "mrs", "ms", "dr", "st", "vs", "etc", "jr", "sr", "prof", "inc", "ltd", "eg", "ie",
];

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `re.sub(r"[\W_]", "", word.lower())`: the letters and digits, lowercase.
pub fn key(word: &str) -> String {
    word.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Vowel groups in a word, at least one; a silent final e doesn't count.
pub fn syllables(word: &str) -> usize {
    let mut letters: Vec<char> = word
        .chars()
        .flat_map(char::to_lowercase)
        .filter(char::is_ascii_lowercase)
        .collect();
    let n = letters.len();
    if n > 2 && letters[n - 1] == 'e' && !"aeiouyl".contains(letters[n - 2]) {
        letters.pop();
    }
    let mut groups = 0;
    let mut in_group = false;
    for letter in letters {
        let vowel = VOWELS.contains(letter);
        if vowel && !in_group {
            groups += 1;
        }
        in_group = vowel;
    }
    groups.max(1)
}

/// `word` with its last vowel written `letters` times ("way" → "wayyy").
/// A silent final e is skipped ("five" → "fiiive"). Punctuation around the
/// word is kept.
pub fn stretch_word(word: &str, letters: usize) -> String {
    let chars: Vec<char> = word.chars().collect();
    let lead = chars.iter().take_while(|c| !is_word_char(**c)).count();
    let trail = chars[lead..]
        .iter()
        .rev()
        .take_while(|c| !is_word_char(**c))
        .count();
    let core = &chars[lead..chars.len() - trail];
    let lower: Vec<char> = core.iter().map(|c| lowercase(*c)).collect();
    let mut end = core.len();
    if end > 3 && lower[end - 1] == 'e' && !VOWELS.contains(lower[end - 2]) {
        end -= 1;
    }
    let Some(index) = (0..end).rev().find(|i| VOWELS.contains(lower[*i])) else {
        return word.to_string();
    };
    let mut written: String = chars[..lead + index].iter().collect();
    written.extend(std::iter::repeat_n(core[index], letters));
    written.extend(&chars[lead + index + 1..]);
    written
}

fn lowercase(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// How many times a drawn-out word's vowel is written.
pub fn letters(stretch: f64) -> usize {
    if stretch >= 6.0 {
        5
    } else if stretch >= 4.5 {
        4
    } else {
        3
    }
}

/// `text` ends with `.`, `!` or `?` and then only `closers`.
fn ends_with_mark(text: &str, closers: &str) -> bool {
    let trimmed = text.trim_end_matches(|c| closers.contains(c));
    trimmed.ends_with(['.', '!', '?'])
}

/// A word Whisper wrote ends its sentence: `[.!?]["')\]]*$`.
pub fn ends_heard_sentence(word: &str) -> bool {
    ends_with_mark(word, "\"')]")
}

/// A written word ends its sentence: a mark, closers after it, and not a
/// period after an abbreviation ("Dr.").
pub fn ends_written_sentence(word: &str) -> bool {
    ends_with_mark(word, CLOSERS) && !(word.ends_with('.') && ABBREVIATIONS.contains(&&*key(word)))
}

/// Where the period that ends `sentence` is, when it follows a letter:
/// not an ellipsis, a number ("3.") or a file name. Python's
/// `(?<=[^\W\d_])\.(?=[closers]*$)`.
pub fn final_period(sentence: &str) -> Option<usize> {
    let trimmed = sentence.trim_end_matches(|c| CLOSERS.contains(c));
    let before = trimmed.strip_suffix('.')?;
    let letter = before.chars().next_back()?;
    (letter.is_alphanumeric() && !letter.is_numeric() && letter != '_').then_some(before.len())
}

/// Each `\S+` run of `text` with its byte range.
pub fn tokens(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(from)) => {
                found.push((from, at));
                start = None;
            }
            (false, None) => start = Some(at),
            _ => {}
        }
    }
    if let Some(from) = start {
        found.push((from, text.len()));
    }
    found
}

/// `re.sub(r"[^\w']", "", word)`.
pub fn core(word: &str) -> String {
    word.chars()
        .filter(|c| is_word_char(*c) || *c == '\'')
        .collect()
}

/// Python's `str.isalpha()`.
pub fn is_alpha(word: &str) -> bool {
    !word.is_empty() && word.chars().all(char::is_alphabetic)
}

/// Python's `str.isupper()`: a cased letter, and none in lowercase.
pub fn is_upper(word: &str) -> bool {
    word.chars().any(char::is_uppercase) && !word.chars().any(char::is_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_syllables() {
        for (word, count) in [
            ("way", 1),
            ("five", 1),
            ("little", 2),
            ("amazing", 3),
            ("the", 1),
            ("rhythm", 1),
            ("I", 1),
            ("Thursday,", 2),
            ("be", 1),
        ] {
            assert_eq!(syllables(word), count, "{word}");
        }
    }

    #[test]
    fn draws_out_the_last_vowel() {
        for (word, letters, written) in [
            ("way", 3, "wayyy"),
            ("five", 3, "fiiive"),
            ("so", 4, "soooo"),
            ("good!", 3, "gooood!"),
            ("\"Nice,\"", 3, "\"Niiice,\""),
            ("hmm", 3, "hmm"),
            ("time", 3, "tiiime"),
            ("free", 3, "freeee"),
        ] {
            assert_eq!(stretch_word(word, letters), written, "{word}");
        }
    }

    #[test]
    fn finds_sentence_ends() {
        assert!(ends_heard_sentence("now."));
        assert!(ends_heard_sentence("now?\")"));
        assert!(!ends_heard_sentence("now.\u{201d}"));
        assert!(ends_written_sentence("now.\u{201d}"));
        assert!(!ends_written_sentence("Dr."));
        assert!(ends_written_sentence("Dr!"));
        assert_eq!(final_period("We did it."), Some(9));
        assert_eq!(final_period("He said \"go.\""), Some(11));
        assert_eq!(final_period("It was 3."), None);
        assert_eq!(final_period("Wait..."), None);
        assert_eq!(final_period("Done!"), None);
    }

    #[test]
    fn keys_and_tokens() {
        assert_eq!(key("Don't!"), "dont");
        assert_eq!(key("snake_case"), "snakecase");
        assert_eq!(tokens("  a bc\td "), vec![(2, 3), (4, 6), (7, 8)]);
        assert_eq!(core("don't,"), "don't");
        assert!(is_upper("UI") && !is_upper("Ui") && !is_upper("42"));
    }
}
