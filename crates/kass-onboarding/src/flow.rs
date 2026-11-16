//! The onboarding flow's order and rules, kept apart from the UI so they
//! can be tested on their own (docs/plans/ONBOARDING.md).

use gpui_kit::{Hsla, Rgba, rgb};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Step {
    Welcome,
    Download,
    InputMonitoring,
    Accessibility,
    Microphone,
    Keys,
    Name,
    Messy,
    Style,
    Rewrite,
    Done,
}

pub const STEPS: [Step; 11] = [
    Step::Welcome,
    Step::Download,
    Step::InputMonitoring,
    Step::Accessibility,
    Step::Microphone,
    Step::Keys,
    Step::Name,
    Step::Messy,
    Step::Style,
    Step::Rewrite,
    Step::Done,
];

/// Sunflower: the one step whose text is dark.
const SUNFLOWER: u32 = 0xF6C343;
pub const INK: u32 = 0x1D1B19;

impl Step {
    pub fn index(self) -> usize {
        STEPS.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Step {
        STEPS[(self.index() + 1).min(STEPS.len() - 1)]
    }

    pub fn previous(self) -> Step {
        STEPS[self.index().saturating_sub(1)]
    }

    /// Steps that record speech wait for the models.
    pub fn is_spoken(self) -> bool {
        matches!(self, Step::Name | Step::Messy | Step::Style | Step::Rewrite)
    }

    /// A spoken step can't be used until dictation can transcribe and clean up.
    pub fn is_locked(self, models_ready: bool) -> bool {
        self.is_spoken() && !models_ready
    }

    /// Each step's color: a new one on every page.
    pub fn color_hex(self) -> u32 {
        match self {
            Step::Welcome | Step::Messy => 0xF2542D,
            Step::Download | Step::Rewrite => 0x2742D9,
            Step::InputMonitoring => INK,
            Step::Accessibility => 0x136F7A,
            Step::Microphone | Step::Done => 0x0F7B5A,
            Step::Keys => SUNFLOWER,
            Step::Name => 0x6B3FA0,
            Step::Style => 0x4A3A33,
        }
    }

    pub fn color(self) -> Hsla {
        rgb(self.color_hex()).into()
    }

    pub fn is_light(self) -> bool {
        self.color_hex() == SUNFLOWER
    }

    /// The text color on this step's color.
    pub fn fg(self) -> Hsla {
        let c: Rgba = if self.is_light() {
            rgb(INK)
        } else {
            rgb(0xFFFFFF)
        };
        c.into()
    }
}

/// What survives a quit: the open step, and whether downloads were started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub step: Step,
    pub downloads_started: bool,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            step: Step::Welcome,
            downloads_started: false,
        }
    }
}

impl Progress {
    /// Saved progress, or the start when there's none or it can't be read.
    pub fn parse(raw: Option<&str>) -> Self {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            step: Option<serde_json::Value>,
            downloads_started: Option<serde_json::Value>,
        }
        let Some(raw) = raw.and_then(|r| serde_json::from_str::<Raw>(r).ok()) else {
            return Self::default();
        };
        let step = raw
            .step
            .and_then(|s| serde_json::from_value::<Step>(s).ok())
            .unwrap_or(Step::Welcome);
        let downloads_started = raw.downloads_started == Some(serde_json::Value::Bool(true));
        Self {
            step,
            downloads_started,
        }
    }

    pub fn load(path: &Path) -> Self {
        Self::parse(std::fs::read_to_string(path).ok().as_deref())
    }

    /// Saving can fail; onboarding then starts over next time.
    pub fn save(&self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(self) {
            let _ = std::fs::write(path, text);
        }
    }

    /// Back to the first step, keeping whether downloads were started.
    pub fn restarted(self) -> Self {
        Self {
            step: Step::Welcome,
            ..self
        }
    }

    pub fn clear(path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Automatic retries before a failed download asks the user what to do.
pub const AUTO_RETRIES: u32 = 3;
/// Pause before each automatic retry.
pub const RETRY_DELAY_MS: u64 = 4000;

pub fn should_retry_automatically(failures: u32) -> bool {
    failures > 0 && failures <= AUTO_RETRIES
}

/// Room to spare on top of the downloads, for unpacking and the system.
const DISK_HEADROOM_MB: f64 = 500.0;

/// MB still to free before the downloads fit, or 0 when they do.
pub fn missing_disk_mb(free_mb: f64, needed_mb: f64) -> f64 {
    (needed_mb + DISK_HEADROOM_MB - free_mb).max(0.0)
}

/// The speech model offered when the default one keeps failing to download.
pub const FALLBACK_STT_MODEL: kass_api::types::WhisperModelSize =
    kass_api::types::WhisperModelSize::Small;

/// "1.2 GB" or "640 MB"; empty for nothing.
pub fn size_label(mb: f64) -> String {
    if mb <= 0.0 {
        String::new()
    } else if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{} MB", mb.round() as u64)
    }
}

/// One piece of a heard sentence, and whether cleanup dropped it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeardPart {
    pub text: String,
    pub dropped: bool,
}

fn norm(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Split what was heard into kept and dropped parts, by the words of what
/// was sent: a heard word that the sent text doesn't use at that point was
/// dropped. Case and punctuation are ignored when matching.
pub fn heard_parts(heard: &str, sent: &str) -> Vec<HeardPart> {
    let heard_words: Vec<&str> = heard.split_whitespace().collect();
    let heard_norm: Vec<String> = heard_words.iter().map(|w| norm(w)).collect();
    let sent_words: Vec<String> = sent
        .split_whitespace()
        .map(norm)
        .filter(|w| !w.is_empty())
        .collect();
    let (n, m) = (heard_words.len(), sent_words.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if heard_norm[i] == sent_words[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut parts: Vec<HeardPart> = Vec::new();
    let mut push = |text: &str, dropped: bool| match parts.last_mut() {
        Some(last) if last.dropped == dropped => {
            last.text.push(' ');
            last.text.push_str(text);
        }
        _ => parts.push(HeardPart {
            text: text.to_string(),
            dropped,
        }),
    };
    let (mut i, mut j) = (0, 0);
    while i < n {
        if j < m && heard_norm[i] == sent_words[j] {
            push(heard_words[i], false);
            i += 1;
            j += 1;
        } else if j < m && lcs[i][j + 1] > lcs[i + 1][j] {
            j += 1;
        } else {
            push(heard_words[i], !heard_norm[i].is_empty());
            i += 1;
        }
    }
    parts
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `text` the way standard cleanup would write it: every sentence starts
/// with a capital, "i" is "I", and it ends with a period. Shown beside the
/// user's own way, so the difference is theirs.
pub fn standard_cleanup(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let mut out = String::with_capacity(trimmed.len() + 1);
    // A sentence starts at the beginning, or after [.!?] and some space.
    let mut at_start = true;
    let mut after_end = false;
    for (k, &c) in chars.iter().enumerate() {
        let prev = k.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(k + 1).copied();
        let starts_sentence = at_start || (after_end && prev.is_some_and(char::is_whitespace));
        if starts_sentence && c.is_lowercase() {
            out.extend(c.to_uppercase());
        } else if c == 'i' && !prev.is_some_and(is_word_char) && !next.is_some_and(is_word_char) {
            out.push('I');
        } else {
            out.push(c);
        }
        at_start = false;
        if matches!(c, '.' | '!' | '?') {
            after_end = true;
        } else if !c.is_whitespace() {
            after_end = false;
        }
    }
    if !matches!(out.chars().last(), Some('.' | '!' | '?' | '\u{2026}')) {
        out.push('.');
    }
    out
}

/// What was said, without its closing punctuation: a name read back.
pub fn without_end_punctuation(text: &str) -> String {
    text.trim()
        .trim_end_matches(['.', '!', '?', ',', ';', ':', '\u{2026}'])
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_walk_forward_and_back_within_bounds() {
        assert_eq!(Step::Welcome.previous(), Step::Welcome);
        assert_eq!(Step::Welcome.next(), Step::Download);
        assert_eq!(Step::Done.next(), Step::Done);
        assert_eq!(Step::Keys.next(), Step::Name);
    }

    #[test]
    fn spoken_steps_lock_until_models_are_ready() {
        assert!(Step::Name.is_locked(false));
        assert!(!Step::Name.is_locked(true));
        assert!(!Step::Keys.is_locked(false));
        assert!(Step::Rewrite.is_locked(false));
    }

    #[test]
    fn only_sunflower_has_dark_text() {
        assert!(Step::Keys.is_light());
        assert!(STEPS.iter().filter(|s| s.is_light()).count() == 1);
    }

    #[test]
    fn progress_reads_what_it_writes() {
        let p = Progress {
            step: Step::InputMonitoring,
            downloads_started: true,
        };
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(
            text,
            r#"{"step":"inputMonitoring","downloadsStarted":true}"#
        );
        assert_eq!(Progress::parse(Some(&text)), p);
    }

    #[test]
    fn bad_progress_starts_over() {
        assert_eq!(Progress::parse(None), Progress::default());
        assert_eq!(Progress::parse(Some("not json")), Progress::default());
        let odd = Progress::parse(Some(r#"{"step":"nowhere","downloadsStarted":"yes"}"#));
        assert_eq!(odd, Progress::default());
        let kept = Progress::parse(Some(r#"{"step":"nowhere","downloadsStarted":true}"#));
        assert!(kept.downloads_started);
        assert_eq!(kept.step, Step::Welcome);
    }

    #[test]
    fn restarting_keeps_downloads() {
        let p = Progress {
            step: Step::Style,
            downloads_started: true,
        };
        assert_eq!(
            p.restarted(),
            Progress {
                step: Step::Welcome,
                downloads_started: true
            }
        );
    }

    #[test]
    fn progress_survives_a_round_trip_on_disk() {
        let dir = std::env::temp_dir().join(format!("kass-onboarding-{}", std::process::id()));
        let path = dir.join("onboarding.json");
        let p = Progress {
            step: Step::Microphone,
            downloads_started: true,
        };
        p.save(&path);
        assert_eq!(Progress::load(&path), p);
        Progress::clear(&path);
        assert_eq!(Progress::load(&path), Progress::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retries_by_itself_a_few_times() {
        assert!(!should_retry_automatically(0));
        assert!(should_retry_automatically(1));
        assert!(should_retry_automatically(AUTO_RETRIES));
        assert!(!should_retry_automatically(AUTO_RETRIES + 1));
    }

    #[test]
    fn disk_keeps_headroom() {
        assert_eq!(missing_disk_mb(10_000.0, 1_000.0), 0.0);
        assert_eq!(missing_disk_mb(1_200.0, 1_000.0), 300.0);
    }

    #[test]
    fn sizes_read_as_mb_or_gb() {
        assert_eq!(size_label(0.0), "");
        assert_eq!(size_label(640.0), "640 MB");
        assert_eq!(size_label(1_550.0), "1.6 GB");
    }

    fn parts(list: &[(&str, bool)]) -> Vec<HeardPart> {
        list.iter()
            .map(|(t, d)| HeardPart {
                text: t.to_string(),
                dropped: *d,
            })
            .collect()
    }

    #[test]
    fn heard_parts_marks_what_cleanup_dropped() {
        assert_eq!(
            heard_parts("um so I think, like, we should go", "I think we should go."),
            parts(&[
                ("um so", true),
                ("I think,", false),
                ("like,", true),
                ("we should go", false)
            ])
        );
    }

    #[test]
    fn heard_parts_keeps_everything_when_nothing_changed() {
        assert_eq!(
            heard_parts("Hello there", "hello, there!"),
            parts(&[("Hello there", false)])
        );
        assert!(heard_parts("", "anything").is_empty());
    }

    #[test]
    fn punctuation_alone_is_never_dropped() {
        assert_eq!(
            heard_parts("yes - please", "Yes please"),
            parts(&[("yes - please", false)])
        );
    }

    #[test]
    fn standard_cleanup_capitalizes_and_ends_sentences() {
        assert_eq!(
            standard_cleanup("i think so. sure i'm in"),
            "I think so. Sure I'm in."
        );
        assert_eq!(standard_cleanup("  done!  "), "Done!");
        assert_eq!(standard_cleanup("wait\u{2026}"), "Wait\u{2026}");
        assert_eq!(standard_cleanup("hi"), "Hi.");
        assert_eq!(standard_cleanup("   "), "");
        assert_eq!(standard_cleanup("pi is it"), "Pi is it.");
    }

    #[test]
    fn names_lose_their_closing_punctuation() {
        assert_eq!(without_end_punctuation(" Morgan. "), "Morgan");
        assert_eq!(without_end_punctuation("Dr. Who?!"), "Dr. Who");
    }
}
