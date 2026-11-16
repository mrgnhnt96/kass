//! How dictation writes the user's dictionary words (docs/plans/DICTIONARIES.md).
//!
//! An entry is a term ("Kubernetes"), which is written the user's way after
//! cleanup, even where Whisper heard it a little wrong ("Kubernetis",
//! "cuber netes"), or a replacement, which writes `written` wherever
//! `spoken` was said. A phrase is a replacement whose `written` is text to
//! insert exactly: it is never a term, and nothing respells it. An entry
//! with `match_sound` off is only recased where it is spelled exactly.
//!
//! Which entries a dictation uses (its app's, its style's, the global ones)
//! is decided by kass-server, which reads them from `kass.db`; this module
//! takes the ones that apply, most specific first.
//!
//! Ported from the backend's `dictionary` module (its text half).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use crate::difflib::SequenceMatcher;
use crate::lexicon;
use crate::py::{self, Groups, Re};
use crate::re;
use crate::spelling::sound;

/// How close one uncommon word must be to a term's letters to be that term
/// misheard ("Kubernetis"): one letter in six may differ, so a five-letter
/// name ("Helga") is not another ("Kass"). Below the shortest term that
/// counts, only a word that sounds the same does ("Sagar" for "Saggar").
const HEARD_RATIO: f64 = 0.82;
const HEARD_MIN: usize = 5;
const MEMO: usize = 4096;

/// How alike a heard form and the term written for it must sound to be that
/// term misheard: "cloud" and "Claude" are 0.73, "midnight" and "Midna"
/// 0.62, a reword like "him" -> "Morgan" 0.22.
pub const SOUNDS_ALIKE: f64 = 0.5;

/// Whether a word is a common word rather than a name.
pub type Common = Arc<dyn Fn(&str) -> bool + Send + Sync>;

fn lexicon_common() -> Common {
    Arc::new(lexicon::common_word)
}

/// Words, as correction_rules matches them.
fn word() -> &'static Re {
    re!(r"\w+(?:['’-]\w+)*")
}

/// Between the words of a phrase: Whisper writes "voice box" and "voice-box".
const BETWEEN: &str = r"[\s-]+";

fn between() -> &'static Re {
    re!(BETWEEN)
}

fn words(text: &str) -> Vec<&str> {
    word().findall(text)
}

/// `text` with any run of spaces or hyphens between words as one space,
/// lowercased: "voice box" and "Voice-Box" are the same.
pub fn normal(text: &str) -> String {
    py::casefold(&between().split(py::strip(text)).join(" "))
}

/// `text` as a pattern that also matches it with other spaces or hyphens
/// between words.
fn phrase(text: &str) -> String {
    between()
        .split(py::strip(text))
        .iter()
        .map(|part| py::escape(part))
        .collect::<Vec<_>>()
        .join(BETWEEN)
}

/// One pattern for every alternative, as a whole word or phrase.
fn bounded(alternatives: impl IntoIterator<Item = String>) -> Option<Re> {
    // Longest first, so "voice box app" wins over "voice box" where both match.
    let mut ordered: Vec<String> = alternatives
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    ordered.sort_by_key(|pattern| std::cmp::Reverse(py::len(pattern)));
    if ordered.is_empty() {
        return None;
    }
    // Grouped by first letter, so a position is turned down after one letter
    // instead of after trying every term. Terms that start differently never
    // match at the same place, and each group keeps the longest-first order.
    // Only an ASCII letter or digit, which escaping leaves alone and case
    // folds predictably, starts a group.
    let mut groups: Vec<(char, Vec<&str>)> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    for pattern in &ordered {
        let first = pattern.chars().next().unwrap();
        if first.is_ascii_alphanumeric() {
            let first = first.to_ascii_lowercase();
            let tail = &pattern[1..];
            match groups.iter_mut().find(|(letter, _)| *letter == first) {
                Some((_, tails)) => tails.push(tail),
                None => groups.push((first, vec![tail])),
            }
        } else {
            rest.push(pattern);
        }
    }
    let grouped = groups
        .iter()
        .map(|(first, tails)| format!("{first}(?:{})", tails.join("|")));
    let all: Vec<String> = grouped.chain(rest.iter().map(|p| p.to_string())).collect();
    Some(Re::i(&format!(
        r"(?<![\w'’-])(?:{})(?![\w'’-])",
        all.join("|")
    )))
}

/// A bounded map that forgets everything once full.
struct Memo<V>(Mutex<HashMap<String, V>>);

impl<V: Clone> Memo<V> {
    fn new() -> Self {
        Memo(Mutex::new(HashMap::new()))
    }

    fn get_or(&self, key: &str, make: impl FnOnce() -> V) -> V {
        if let Some(found) = self.0.lock().unwrap().get(key) {
            return found.clone();
        }
        let made = make();
        let mut memo = self.0.lock().unwrap();
        if memo.len() >= MEMO {
            memo.clear();
        }
        memo.insert(key.to_string(), made.clone());
        made
    }
}

/// The terms a transcript may have heard wrong, keyed by how they sound.
struct Heard {
    by_sound: HashMap<String, String>,
    /// Every term as written, lowercased: a word that is one is already right.
    exact: HashSet<String>,
    /// Terms long enough to match approximately, by first sound.
    by_start: HashMap<char, Vec<(Vec<char>, String)>>,
    lengths: HashSet<usize>,
    words: usize,
    common: Common,
    sounds: Memo<String>,
    single: Memo<Option<String>>,
}

impl Heard {
    /// `exact`: more terms, only ever kept as they are, never matched to.
    fn of<'a>(
        terms: impl IntoIterator<Item = &'a str>,
        common: Common,
        exact: impl IntoIterator<Item = &'a str>,
    ) -> Option<Heard> {
        let terms: Vec<&str> = terms.into_iter().collect();
        let mut by_sound = HashMap::new();
        let mut by_start: HashMap<char, Vec<(Vec<char>, String)>> = HashMap::new();
        let mut most = 0;
        for term in &terms {
            let key = sound(term);
            if key.is_empty() || by_sound.contains_key(&key) {
                continue;
            }
            by_sound.insert(key.clone(), term.to_string());
            most = most.max(words(term).len());
            let chars: Vec<char> = key.chars().collect();
            if chars.len() >= HEARD_MIN {
                by_start
                    .entry(chars[0])
                    .or_default()
                    .push((chars, term.to_string()));
            }
        }
        if by_sound.is_empty() {
            return None;
        }
        let exact = terms.iter().copied().chain(exact).map(normal).collect();
        let lengths = by_sound.keys().map(|key| py::len(key)).collect();
        Some(Heard {
            by_sound,
            exact,
            by_start,
            lengths,
            // Whisper splits a word it doesn't know: "cuber netes".
            words: most + 1,
            common,
            sounds: Memo::new(),
            single: Memo::new(),
        })
    }

    fn sound(&self, word: &str) -> String {
        self.sounds.get_or(word, || sound(word))
    }

    /// The term one word is, heard wrong; never a common word, which the
    /// user likely said.
    fn one(&self, word: &str) -> Option<String> {
        self.single.get_or(word, || {
            let key = self.sound(word);
            if key.is_empty() || self.exact.contains(&py::casefold(word)) || (self.common)(word) {
                return None;
            }
            if let Some(found) = self.by_sound.get(&key) {
                return Some(found.clone());
            }
            let key: Vec<char> = key.chars().collect();
            if key.len() < HEARD_MIN - 1 {
                return None;
            }
            // The closest; the first, most specific, of equally close ones.
            let (mut found, mut best) = (None, HEARD_RATIO);
            for (term_key, term) in self.by_start.get(&key[0]).into_iter().flatten() {
                let quick = 2.0 * key.len().min(term_key.len()) as f64
                    / (key.len() + term_key.len()) as f64;
                if quick < best {
                    continue;
                }
                let ratio = SequenceMatcher::new(&key, term_key, false).ratio();
                if ratio >= best && (found.is_none() || ratio > best) {
                    (found, best) = (Some(term.clone()), ratio);
                }
            }
            found
        })
    }

    fn fix(&self, text: &str) -> String {
        let found = word().finditer(text);
        let mut out = String::with_capacity(text.len());
        let (mut last, mut i, mut changed) = (0, 0, false);
        while i < found.len() {
            match self.at(text, &found, i) {
                Some((term, width)) => {
                    out.push_str(&text[last..found[i].start()]);
                    out.push_str(&term);
                    last = found[i + width - 1].end();
                    i += width;
                    changed = true;
                }
                None => i += 1,
            }
        }
        if !changed {
            return text.to_string();
        }
        out.push_str(&text[last..]);
        out
    }

    /// The term the words from `found[i]` are, longest first, and how many words.
    fn at(&self, text: &str, found: &[fancy_regex::Captures], i: usize) -> Option<(String, usize)> {
        let mut key = String::new();
        let mut joined: Vec<(String, usize)> = Vec::new();
        for n in 0..self.words.min(found.len() - i) {
            let word = &found[i + n];
            // Words of one phrase: only spaces between, and no "tail's scale".
            if n > 0
                && !text[found[i + n - 1].end()..word.start()]
                    .trim_matches(' ')
                    .is_empty()
            {
                break;
            }
            if word.text().contains(['\'', '\u{2019}']) {
                break;
            }
            let part = self.sound(word.text());
            let first = part.chars().next();
            if key.chars().last().is_some_and(|last| Some(last) == first)
                && first.is_some_and(py::is_alpha)
            {
                key.push_str(&part[first.unwrap().len_utf8()..]);
            } else {
                key.push_str(&part);
            }
            joined.push((key.clone(), n + 1));
        }
        for (key, width) in joined.into_iter().rev() {
            if width == 1 {
                let word = found[i].text();
                if let Some(term) = self.one(word).filter(|term| term != word) {
                    return Some((term, 1));
                }
            } else if self.lengths.contains(&py::len(&key)) {
                if let Some(term) = self.by_sound.get(&key) {
                    let span = &text[found[i].start()..found[i + width - 1].end()];
                    if !self.exact.contains(&normal(span)) {
                        return Some((term.clone(), width));
                    }
                }
            }
        }
        None
    }
}

/// A dictionary entry that applies to a dictation, as matching needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Term {
    pub written: String,
    pub spoken: Option<String>,
    /// Off: never swapped in for a word that only sounds like it.
    pub match_sound: bool,
    /// Text inserted exactly where `spoken` is said; never a term.
    pub phrase: bool,
}

struct Compiled {
    replace: Option<Re>,
    recase: Option<Re>,
    recased: HashMap<String, String>,
    heard: Option<Heard>,
}

/// One app's merged dictionary, as dictation uses it.
pub struct Dictionary {
    /// Whisper prompt candidates, most specific first: terms, then what
    /// replacements write.
    pub terms: Vec<String>,
    /// Capitalized terms, which dictation keeps capitalized mid-sentence.
    pub names: BTreeSet<String>,
    /// What each replacement writes, by its lowercased spoken form.
    pub replacements: HashMap<String, String>,
    /// Terms whose case is fixed after cleanup, by lowercased form.
    pub spellings: HashMap<String, String>,
    /// The spellings, in the order they were added: earlier ones win.
    order: Vec<String>,
    /// Of those, the ones never matched by sound, by lowercased form.
    pub exact: HashSet<String>,
    /// The most words a match covers, which provisional text holds back.
    pub span: usize,
    common: Common,
    compiled: OnceLock<Compiled>,
}

impl Default for Dictionary {
    fn default() -> Self {
        Dictionary {
            terms: Vec::new(),
            names: BTreeSet::new(),
            replacements: HashMap::new(),
            spellings: HashMap::new(),
            order: Vec::new(),
            exact: HashSet::new(),
            span: 0,
            common: lexicon_common(),
            compiled: OnceLock::new(),
        }
    }
}

impl std::fmt::Debug for Dictionary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictionary")
            .field("terms", &self.terms)
            .field("replacements", &self.replacements)
            .field("spellings", &self.spellings)
            .field("exact", &self.exact)
            .field("span", &self.span)
            .finish()
    }
}

/// The dictionary for `active`: the entries that apply and aren't
/// overridden, most specific first. Without `exact_spelling`, every term is
/// matched by sound.
pub fn build(active: &[Term], exact_spelling: bool) -> Dictionary {
    build_with(active, exact_spelling, lexicon_common())
}

/// [`build`], deciding common words with `common`.
pub fn build_with(active: &[Term], exact_spelling: bool, common: Common) -> Dictionary {
    let mut dictionary = Dictionary {
        common,
        ..Dictionary::default()
    };
    if active.is_empty() {
        return dictionary;
    }
    let spoken = |entry: &Term| entry.spoken.as_deref().is_some_and(|s| !s.is_empty());
    for entry in active.iter().filter(|entry| spoken(entry)) {
        dictionary
            .replacements
            .entry(normal(entry.spoken.as_deref().unwrap()))
            .or_insert_with(|| entry.written.clone());
    }
    // Terms before what replacements write, which Whisper needs less: the
    // replacement fixes those whatever Whisper hears. What a phrase writes is
    // text to insert, not a word Whisper hears or a spelling to fix.
    let mut terms: Vec<&Term> = active.iter().filter(|entry| !entry.phrase).collect();
    terms.sort_by_key(|entry| spoken(entry));
    for entry in terms {
        let written = py::strip(&entry.written);
        let key = normal(written);
        if dictionary.spellings.contains_key(&key) {
            continue;
        }
        dictionary
            .spellings
            .insert(key.clone(), written.to_string());
        dictionary.order.push(key.clone());
        // The most specific entry that writes it decides.
        if exact_spelling && !entry.match_sound {
            dictionary.exact.insert(key);
        }
        dictionary.terms.push(written.to_string());
    }
    dictionary.names = dictionary
        .terms
        .iter()
        .flat_map(|term| words(term))
        .filter(|word| py::isupper(py::head(word, 1)))
        .map(str::to_string)
        .collect();
    // A term may be heard as one word more than it has ("cuber netes").
    let pieces = |text: &str| between().split(text).len();
    dictionary.span = dictionary
        .replacements
        .keys()
        .map(|text| pieces(text))
        .chain(dictionary.spellings.keys().map(|text| pieces(text) + 1))
        .max()
        .unwrap_or(0);
    dictionary
}

impl Dictionary {
    /// A dictionary of these replacements (by lowercased spoken form) and
    /// spellings (by lowercased form, earlier ones first), with `exact`
    /// never matched by sound.
    pub fn of(
        replacements: impl IntoIterator<Item = (String, String)>,
        spellings: impl IntoIterator<Item = (String, String)>,
        exact: impl IntoIterator<Item = String>,
        common: Common,
    ) -> Dictionary {
        let mut dictionary = Dictionary {
            replacements: replacements.into_iter().collect(),
            exact: exact.into_iter().collect(),
            common,
            ..Dictionary::default()
        };
        for (key, term) in spellings {
            if !dictionary.spellings.contains_key(&key) {
                dictionary.order.push(key.clone());
                dictionary.terms.push(term.clone());
            }
            dictionary.spellings.insert(key, term);
        }
        dictionary
    }

    /// Spellings in the order they were added, by lowercased form.
    pub fn spellings_in_order(&self) -> impl Iterator<Item = (&str, &str)> {
        self.order
            .iter()
            .map(|key| (key.as_str(), self.spellings[key].as_str()))
    }

    fn compiled(&self) -> &Compiled {
        self.compiled.get_or_init(|| {
            let recased: HashMap<String, String> = self
                .order
                .iter()
                .map(|key| (key, &self.spellings[key]))
                .filter(|(_, term)| !words(term).iter().all(|word| (self.common)(word)))
                .map(|(key, term)| (key.clone(), term.clone()))
                .collect();
            let spelled = |exact: bool| {
                self.order
                    .iter()
                    .filter(move |key| self.exact.contains(*key) == exact)
                    .map(|key| self.spellings[key].as_str())
            };
            Compiled {
                replace: bounded(self.replacements.keys().map(|spoken| phrase(spoken))),
                recase: bounded(
                    self.order
                        .iter()
                        .filter_map(|key| recased.get(key))
                        .map(|term| phrase(term)),
                ),
                // Every term: only the heard word must not be a common one.
                heard: Heard::of(spelled(false), self.common.clone(), spelled(true)),
                recased,
            }
        })
    }

    /// Replacements, then terms written the user's way.
    ///
    /// What a replacement wrote is left as written: no term respells it. A
    /// term is fixed where Whisper wrote it in other capitals, and where it
    /// heard it a little wrong: an uncommon word spelled or sounding close
    /// ("Kubernetis"), or the term split into words ("cuber netes"). A term
    /// that is a common word ("Mark", "Slack") keeps whatever case the text
    /// gave it: it may be the word ("mark this") and not the name. A common
    /// word is never taken for a term, and a term with `match_sound` off is
    /// only ever recased, never matched by sound.
    pub fn apply(&self, text: &str) -> String {
        if text.is_empty() || (self.replacements.is_empty() && self.spellings.is_empty()) {
            return text.to_string();
        }
        let compiled = self.compiled();
        let fix = |part: &str| -> String {
            let mut part = part.to_string();
            if let Some(recase) = &compiled.recase {
                part = recase.sub(&part, |found| {
                    compiled
                        .recased
                        .get(&normal(found.text()))
                        .cloned()
                        .unwrap_or_else(|| found.text().to_string())
                });
            }
            if let Some(heard) = &compiled.heard {
                part = heard.fix(&part);
            }
            part
        };
        let Some(replace) = &compiled.replace else {
            return fix(text);
        };
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for found in replace.finditer(text) {
            let Some(written) = self.replacements.get(&normal(found.text())) else {
                continue;
            };
            out.push_str(&fix(&text[last..found.start()]));
            out.push_str(written);
            last = found.end();
        }
        out.push_str(&fix(&text[last..]));
        out
    }
}

/// A one-term dictionary, the way a correction's term is tried.
fn alone(term: &str, common: Common) -> Dictionary {
    Dictionary::of([], [(normal(term), term.to_string())], [], common)
}

/// A word written drawn out ("wayyy") is how it was said, not vocabulary.
fn drawn() -> &'static Re {
    re!(r"([^\W\d_])\1\1", py::I)
}

/// Each (heard, written) pair where the user fixed a misheard term.
///
/// The written term is one or two words in place of up to three heard ones,
/// and isn't all common words, digits or a drawn-out spelling: vocabulary,
/// not a wording or formatting change.
pub fn misheard_terms(original: &str, corrected: &str) -> Vec<(String, String)> {
    misheard_terms_with(original, corrected, &lexicon::common_word)
}

pub fn misheard_terms_with(
    original: &str,
    corrected: &str,
    common: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let (heard, wrote) = (word().finditer(original), word().finditer(corrected));
    let heard_words: Vec<&str> = heard.iter().map(|m| m.text()).collect();
    let wrote_words: Vec<&str> = wrote.iter().map(|m| m.text()).collect();
    let matcher = SequenceMatcher::new(&heard_words, &wrote_words, false);
    let mut found = Vec::new();
    for code in matcher.get_opcodes() {
        let (a, b, c, d) = (code.i1, code.i2, code.j1, code.j2);
        if code.tag != crate::difflib::Tag::Replace
            || !(1..=3).contains(&(b - a))
            || !(1..=2).contains(&(d - c))
        {
            continue;
        }
        let term = &corrected[wrote[c].start()..wrote[d - 1].end()];
        let said = &original[heard[a].start()..heard[b - 1].end()];
        let term_words = words(term);
        if term.chars().any(py::is_digit)
            || drawn().is_match(term)
            || term_words.iter().all(|word| common(word))
        {
            continue;
        }
        // Words only dropped ("I. Don't" -> "don't") were heard right. A
        // recased name ("github" -> "GitHub") is still the user's term.
        let said_words = words(said);
        let folded =
            |list: &[&str]| -> HashSet<String> { list.iter().map(|w| py::casefold(w)).collect() };
        if term_words.len() < said_words.len()
            && folded(&term_words).is_subset(&folded(&said_words))
        {
            continue;
        }
        found.push((said.to_string(), term.to_string()));
    }
    found
}

/// The terms a correction fixed, as (written, misheard form to replace or None).
///
/// A term is one or two words the user wrote in place of up to three heard
/// ones ("Pikto" -> "Picto"), that aren't all common words, digits or a
/// drawn-out spelling. It is kept only when it fixes the mistake: matched
/// by sound like any term, or, for a heard form that isn't a common word
/// ("Clard"), as a replacement of that form. A common word heard ("cloud",
/// "clod") is never replaced, since the user likely says it too; the term
/// is still prompted, which helps Whisper hear it.
pub fn corrected_words(original: &str, corrected: &str) -> Vec<(String, Option<String>)> {
    corrected_words_with(original, corrected, lexicon_common())
}

pub fn corrected_words_with(
    original: &str,
    corrected: &str,
    common: Common,
) -> Vec<(String, Option<String>)> {
    misheard_terms_with(original, corrected, &*common)
        .into_iter()
        .map(|(said, term)| {
            if normal(&alone(&term, common.clone()).apply(&said)) == normal(&term) {
                (term, None)
            } else if !words(&said).iter().any(|word| common(word)) {
                (term, Some(said))
            } else {
                (term, None)
            }
        })
        .collect()
}

/// Whether `heard` sounds enough like `written` to be it misheard.
pub fn sounds_alike(heard: &str, written: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (
        sound(heard).chars().collect(),
        sound(written).chars().collect(),
    );
    SequenceMatcher::new(&a, &b, false).ratio() >= SOUNDS_ALIKE
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMON: &[&str] = &[
        "mark", "slack", "open", "source", "voice", "box", "cloud", "this", "now", "the",
    ];

    fn common() -> Common {
        Arc::new(|word: &str| COMMON.contains(&py::casefold(word).as_str()))
    }

    fn term(written: &str, spoken: Option<&str>) -> Term {
        Term {
            written: written.into(),
            spoken: spoken.map(Into::into),
            match_sound: true,
            phrase: false,
        }
    }

    fn merged(terms: &[Term]) -> Dictionary {
        build_with(terms, true, common())
    }

    #[test]
    fn writes_terms_heard_a_little_wrong() {
        let found = merged(&[term("Kubernetes", None), term("Tailscale", None)]);
        assert_eq!(
            found.apply("deploy to kubernetis now"),
            "deploy to Kubernetes now"
        );
        assert_eq!(
            found.apply("cuber netes and tail scale"),
            "Kubernetes and Tailscale"
        );
        assert_eq!(
            found.apply("the KUBERNETES cluster"),
            "the Kubernetes cluster"
        );
    }

    #[test]
    fn replacements_win_and_are_left_as_written() {
        let found = merged(&[term("Voicebox", Some("voice box")), term("Kass", None)]);
        assert_eq!(found.apply("open Voice-Box now"), "open Voicebox now");
        assert_eq!(found.span, 2);
        assert_eq!(found.terms, ["Kass", "Voicebox"]);
    }

    #[test]
    fn common_words_keep_their_case_and_exact_terms_never_match_by_sound() {
        let mut meghan = term("Meghan", None);
        meghan.match_sound = false;
        let found = merged(&[term("Slack", None), meghan]);
        assert_eq!(found.apply("mark this in slack"), "mark this in slack");
        assert_eq!(found.apply("ask megan and MEGHAN"), "ask megan and Meghan");
    }

    #[test]
    fn finds_the_terms_a_correction_fixed() {
        assert_eq!(
            misheard_terms_with("ask cloud about it", "ask Claude about it", &*common()),
            [("cloud".to_string(), "Claude".to_string())]
        );
        assert_eq!(
            corrected_words_with("ask cloud about it", "ask Claude about it", common()),
            [("Claude".to_string(), None)]
        );
        assert_eq!(
            corrected_words_with("open Pikto now", "open Picto now", common()),
            [("Picto".to_string(), None)]
        );
        assert_eq!(
            corrected_words_with("ping Clard today", "ping Morgan today", common()),
            [("Morgan".to_string(), Some("Clard".to_string()))]
        );
        assert!(misheard_terms_with("it was way", "it was wayyy", &*common()).is_empty());
        assert!(sounds_alike("cloud", "Claude") && !sounds_alike("him", "Morgan"));
    }
}
