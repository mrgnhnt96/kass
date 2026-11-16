//! Word-level diff between two transcripts, for showing what refinement (or
//! a correction) changed. Words compare by their letters and digits only, so
//! "thursday" and "Thursday." count as the same word and only real wording
//! changes are marked.

/// A run of text on one side of the diff; `changed` marks words the other side lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSegment {
    pub text: String,
    pub changed: bool,
}

/// One place where the texts differ: the words taken out and the words put in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffHunk {
    pub removed: String,
    pub added: String,
    /// How many times the same change was made, when more than once.
    pub count: Option<usize>,
}

impl DiffHunk {
    pub fn new(removed: impl Into<String>, added: impl Into<String>) -> Self {
        Self {
            removed: removed.into(),
            added: added.into(),
            count: None,
        }
    }

    fn times(&self) -> usize {
        self.count.unwrap_or(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergedKind {
    Same,
    Removed,
    Added,
}

/// A run of the merged diff: text both sides share, or only one side has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedSegment {
    pub text: String,
    pub kind: MergedKind,
}

/// Words both sides keep but write differently, which the segments don't mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Restyled {
    pub case: usize,
    pub punctuation: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordDiff {
    /// The first text, with the words missing from the second marked.
    pub before: Vec<DiffSegment>,
    /// The second text, with the words missing from the first marked.
    pub after: Vec<DiffSegment>,
    /// Both texts in one run, each change's removed words before its added ones.
    pub merged: Vec<MergedSegment>,
    pub hunks: Vec<DiffHunk>,
    pub restyled: Restyled,
}

/// Past this many LCS cells (about 2,000 × 2,000 words) the diff is skipped
/// and both texts are shown unmarked, so a long file import can't stall the UI.
const MAX_CELLS: usize = 4_000_000;

pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}

struct Token<'a> {
    /// The word plus the whitespace after it, so joining tokens rebuilds the text.
    text: String,
    word: &'a str,
    key: String,
}

fn tokenize(text: &str) -> Vec<Token<'_>> {
    let body = text.trim_start();
    let leading = &text[..text.len() - body.len()];
    let mut tokens = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let after = &rest[word_end..];
        let space_end = after.len() - after.trim_start().len();
        let key: String = word
            .to_lowercase()
            .chars()
            .filter(|&c| is_word_char(c))
            .collect();
        tokens.push(Token {
            text: rest[..word_end + space_end].to_string(),
            word,
            key: if key.is_empty() {
                word.to_string()
            } else {
                key
            },
        });
        rest = &after[space_end..];
    }
    if !leading.is_empty()
        && let Some(first) = tokens.first_mut()
    {
        first.text.insert_str(0, leading);
    }
    tokens
}

enum Op<'t, 'a> {
    Same(&'t Token<'a>, &'t Token<'a>),
    Removed(&'t Token<'a>),
    Added(&'t Token<'a>),
}

fn diff_tokens<'t, 'a>(a: &'t [Token<'a>], b: &'t [Token<'a>]) -> Option<Vec<Op<'t, 'a>>> {
    let mut start = 0;
    while start < a.len() && start < b.len() && a[start].key == b[start].key {
        start += 1;
    }
    let (mut end_a, mut end_b) = (a.len(), b.len());
    while end_a > start && end_b > start && a[end_a - 1].key == b[end_b - 1].key {
        end_a -= 1;
        end_b -= 1;
    }
    let mid_a = &a[start..end_a];
    let mid_b = &b[start..end_b];
    let (n, m) = (mid_a.len(), mid_b.len());
    if (n + 1).saturating_mul(m + 1) > MAX_CELLS {
        return None;
    }

    // lcs[i][j] = longest common subsequence of mid_a[i..] and mid_b[j..].
    let width = m + 1;
    let mut lcs = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * width + j] = if mid_a[i].key == mid_b[j].key {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }

    let mut ops: Vec<Op> = (0..start).map(|i| Op::Same(&a[i], &b[i])).collect();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if mid_a[i].key == mid_b[j].key {
            ops.push(Op::Same(&mid_a[i], &mid_b[j]));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            ops.push(Op::Removed(&mid_a[i]));
            i += 1;
        } else {
            ops.push(Op::Added(&mid_b[j]));
            j += 1;
        }
    }
    ops.extend(mid_a[i..].iter().map(Op::Removed));
    ops.extend(mid_b[j..].iter().map(Op::Added));
    ops.extend((end_a..a.len()).map(|i| Op::Same(&a[i], &b[end_b + i - end_a])));
    Some(ops)
}

fn push_segment(segments: &mut Vec<DiffSegment>, text: &str, changed: bool) {
    match segments.last_mut() {
        Some(last) if last.changed == changed => last.text.push_str(text),
        _ => segments.push(DiffSegment {
            text: text.to_string(),
            changed,
        }),
    }
}

fn push_merged(segments: &mut Vec<MergedSegment>, text: &str, kind: MergedKind) {
    match segments.last_mut() {
        Some(last) if last.kind == kind => last.text.push_str(text),
        _ => segments.push(MergedSegment {
            text: text.to_string(),
            kind,
        }),
    }
}

/// Keeps the space between a changed run and the next word outside the mark,
/// so a strike-through or highlight covers the words and not the gap.
fn split_trailing_space(segments: Vec<DiffSegment>) -> Vec<DiffSegment> {
    let mut out = Vec::new();
    for segment in segments {
        if !segment.changed {
            push_segment(&mut out, &segment.text, false);
            continue;
        }
        let body = segment.text.trim_end();
        let trailing = &segment.text[body.len()..];
        if !body.is_empty() {
            out.push(DiffSegment {
                text: body.to_string(),
                changed: true,
            });
        }
        if !trailing.is_empty() {
            push_segment(&mut out, trailing, false);
        }
    }
    out
}

fn split_merged_trailing_space(segments: Vec<MergedSegment>) -> Vec<MergedSegment> {
    let mut out = Vec::new();
    for segment in segments {
        if segment.kind == MergedKind::Same {
            push_merged(&mut out, &segment.text, MergedKind::Same);
            continue;
        }
        let body = segment.text.trim_end();
        let trailing = &segment.text[body.len()..];
        if !body.is_empty() {
            out.push(MergedSegment {
                text: body.to_string(),
                kind: segment.kind,
            });
        }
        if !trailing.is_empty() {
            push_merged(&mut out, trailing, MergedKind::Same);
        }
    }
    out
}

fn is_punctuation(c: char) -> bool {
    !is_word_char(c) && !c.is_whitespace()
}

/// "Sagar." → "Saggar." is a change to the word, not to the period: drop
/// punctuation both sides start or end with, unless nothing else is left.
fn trim_shared_punctuation(removed: &str, added: &str) -> DiffHunk {
    let r: Vec<char> = removed.chars().collect();
    let a: Vec<char> = added.chars().collect();
    let mut start = 0;
    while start < r.len() && start < a.len() && r[start] == a[start] && is_punctuation(r[start]) {
        start += 1;
    }
    let mut end = 0;
    while end < r.len() - start
        && end < a.len() - start
        && r[r.len() - 1 - end] == a[a.len() - 1 - end]
        && is_punctuation(r[r.len() - 1 - end])
    {
        end += 1;
    }
    let trimmed_removed: String = r[start..r.len() - end].iter().collect();
    let trimmed_added: String = a[start..a.len() - end].iter().collect();
    if trimmed_removed.is_empty() && trimmed_added.is_empty() {
        DiffHunk::new(removed, added)
    } else {
        DiffHunk::new(trimmed_removed, trimmed_added)
    }
}

fn strip(word: &str, keep_word_chars: bool) -> String {
    word.chars()
        .filter(|&c| is_word_char(c) == keep_word_chars)
        .collect()
}

pub fn diff_words(before: &str, after: &str) -> WordDiff {
    let a = tokenize(before);
    let b = tokenize(after);
    let Some(ops) = diff_tokens(&a, &b) else {
        let side = |text: &str| {
            if text.is_empty() {
                Vec::new()
            } else {
                vec![DiffSegment {
                    text: text.to_string(),
                    changed: false,
                }]
            }
        };
        return WordDiff {
            before: side(before),
            after: side(after),
            merged: if after.is_empty() {
                Vec::new()
            } else {
                vec![MergedSegment {
                    text: after.to_string(),
                    kind: MergedKind::Same,
                }]
            },
            hunks: Vec::new(),
            restyled: Restyled::default(),
        };
    };

    let mut before_segments = Vec::new();
    let mut after_segments = Vec::new();
    let mut merged = Vec::new();
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut restyled = Restyled::default();
    let mut removed: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    // A change's tokens, held so its removed words show before its added ones.
    let mut removed_text = String::new();
    let mut added_text = String::new();

    let mut close_hunk = |removed: &mut Vec<&str>,
                          added: &mut Vec<&str>,
                          removed_text: &mut String,
                          added_text: &mut String,
                          merged: &mut Vec<MergedSegment>| {
        if !removed.is_empty() || !added.is_empty() {
            let hunk = trim_shared_punctuation(&removed.join(" "), &added.join(" "));
            // The same change made again is counted, not listed again.
            match hunks
                .iter_mut()
                .find(|h| h.removed == hunk.removed && h.added == hunk.added)
            {
                Some(same) => same.count = Some(same.times() + 1),
                None => hunks.push(hunk),
            }
        }
        if !removed_text.is_empty() {
            push_merged(merged, removed_text, MergedKind::Removed);
        }
        if !added_text.is_empty() {
            push_merged(merged, added_text, MergedKind::Added);
        }
        removed.clear();
        added.clear();
        removed_text.clear();
        added_text.clear();
    };

    for op in ops {
        match op {
            Op::Same(x, y) => {
                close_hunk(
                    &mut removed,
                    &mut added,
                    &mut removed_text,
                    &mut added_text,
                    &mut merged,
                );
                push_segment(&mut before_segments, &x.text, false);
                push_segment(&mut after_segments, &y.text, false);
                push_merged(&mut merged, &y.text, MergedKind::Same);
                if x.word != y.word {
                    if strip(x.word, true) != strip(y.word, true) {
                        restyled.case += 1;
                    }
                    if strip(x.word, false) != strip(y.word, false) {
                        restyled.punctuation += 1;
                    }
                }
            }
            Op::Removed(x) => {
                removed.push(x.word);
                removed_text.push_str(&x.text);
                push_segment(&mut before_segments, &x.text, true);
            }
            Op::Added(y) => {
                added.push(y.word);
                added_text.push_str(&y.text);
                push_segment(&mut after_segments, &y.text, true);
            }
        }
    }
    close_hunk(
        &mut removed,
        &mut added,
        &mut removed_text,
        &mut added_text,
        &mut merged,
    );

    WordDiff {
        before: split_trailing_space(before_segments),
        after: split_trailing_space(after_segments),
        merged: split_merged_trailing_space(merged),
        hunks,
        restyled,
    }
}

/// How much a diff changed, by kind, for the change chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    /// Words taken out with nothing put in their place.
    pub removed: usize,
    /// Words put in where nothing was taken out.
    pub added: usize,
    /// Places where words were replaced by other words.
    pub reworded: usize,
    pub case: usize,
    pub punctuation: usize,
}

pub fn summarize_changes(diff: &WordDiff) -> ChangeSummary {
    let mut summary = ChangeSummary {
        case: diff.restyled.case,
        punctuation: diff.restyled.punctuation,
        ..Default::default()
    };
    for hunk in &diff.hunks {
        let times = hunk.times();
        if !hunk.removed.is_empty() && !hunk.added.is_empty() {
            summary.reworded += times;
        } else if !hunk.removed.is_empty() {
            summary.removed += count_words(&hunk.removed) * times;
        } else {
            summary.added += count_words(&hunk.added) * times;
        }
    }
    summary
}

/// Number of words in a transcript.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(segments: &[DiffSegment]) -> String {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn same_words_in_another_case_or_punctuation_are_not_marked() {
        let diff = diff_words("see you thursday", "See you Thursday.");
        assert!(diff.hunks.is_empty());
        assert_eq!(
            diff.restyled,
            Restyled {
                case: 2,
                punctuation: 1
            }
        );
        assert!(diff.after.iter().all(|s| !s.changed));
    }

    #[test]
    fn a_replaced_word_is_one_hunk() {
        let diff = diff_words("use post grass here", "use Postgres here");
        assert_eq!(diff.hunks, vec![DiffHunk::new("post grass", "Postgres")]);
        assert_eq!(
            diff.after,
            vec![
                DiffSegment {
                    text: "use ".into(),
                    changed: false
                },
                DiffSegment {
                    text: "Postgres".into(),
                    changed: true
                },
                DiffSegment {
                    text: " here".into(),
                    changed: false
                },
            ]
        );
        assert_eq!(joined(&diff.before), "use post grass here");
    }

    #[test]
    fn the_merged_run_puts_removed_words_before_added_ones() {
        let diff = diff_words("a b c", "a x c");
        let kinds: Vec<_> = diff
            .merged
            .iter()
            .map(|s| (s.text.as_str(), s.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("a ", MergedKind::Same),
                ("b", MergedKind::Removed),
                (" ", MergedKind::Same),
                ("x", MergedKind::Added),
                (" c", MergedKind::Same),
            ]
        );
    }

    #[test]
    fn shared_punctuation_is_trimmed_from_hunks() {
        let diff = diff_words("Hi Sagar.", "Hi Saggar.");
        assert_eq!(diff.hunks, vec![DiffHunk::new("Sagar", "Saggar")]);
    }

    #[test]
    fn a_repeated_change_is_counted() {
        let diff = diff_words("teh cat and teh dog", "the cat and the dog");
        assert_eq!(diff.hunks.len(), 1);
        assert_eq!(diff.hunks[0].count, Some(2));
        assert_eq!(summarize_changes(&diff).reworded, 2);
    }

    #[test]
    fn summary_counts_added_and_removed_words() {
        let diff = diff_words("um so we go", "so we go now please");
        let summary = summarize_changes(&diff);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.added, 2);
        assert_eq!(summary.reworded, 0);
    }

    #[test]
    fn leading_whitespace_is_kept() {
        let diff = diff_words("  hello world", "  hello there");
        assert_eq!(joined(&diff.after), "  hello there");
    }

    #[test]
    fn empty_texts_diff_to_nothing() {
        let diff = diff_words("", "");
        assert!(diff.hunks.is_empty() && diff.merged.is_empty());
        assert_eq!(count_words(" a  b\nc "), 3);
    }
}
