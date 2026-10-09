//! Direct text insertion through the Accessibility API.
//!
//! The clipboard path in `paste_final_text` costs a clipboard save/write, a
//! synthetic ⌘V and a 400 ms wait before the clipboard is restored. When the focused element of the target app lets us
//! set `AXSelectedText`, we can instead write the text straight into it: it
//! replaces the selection (or inserts at the caret), with no keystroke, no
//! clipboard and no sleep, so the text is on screen as soon as the call
//! returns.
//!
//! Rules this module enforces:
//!
//! - Never insert into a secure (password) field, and never into apps whose
//!   AX text is not the input (terminals): those keep the clipboard path.
//! - Only attempt when the insertion can be verified: the selection range and
//!   the character count must be readable before the attempt.
//! - After the attempt, re-read the element. Fall back to the clipboard only
//!   when the element is observably unchanged, so the text is never inserted
//!   twice. When the outcome cannot be determined, report an error instead of
//!   pasting (the text stays in Captures).
//!
//! The decisions ([`choose_strategy`], [`judge`], [`next_step`]) are pure and
//! unit-tested. The AX calls sit behind [`AxTextTarget`] so the orchestration
//! ([`insert_into`]) runs against fakes in tests.

use std::sync::Mutex;
use std::time::Duration;

use crate::insert_chain::{Attempt, Inserter, Method, Request};
use crate::join;
use crate::overlap;

/// A range in the element's text, in UTF-16 code units (what AX reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub location: i64,
    pub length: i64,
}

/// What the element looks like at one moment. `None` means the attribute
/// could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Observation {
    pub selection: Option<TextRange>,
    pub char_count: Option<i64>,
}

/// What the focused element supports, read before any change is made.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub selected_text_settable: bool,
    pub before: Observation,
}

/// Why the clipboard path is used instead of direct insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    EmptyText,
    NoFocusedElement,
    ClipboardOnlyApp,
    SecureField,
    NotATextRole,
    NotSettable,
    Unverifiable,
    /// Attempted, and the element is observably unchanged.
    NotInserted,
    /// The app writes Accessibility text somewhere other than the caret
    /// ([`writes_at_caret`]).
    WritesAwayFromCaret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Accessibility,
    Clipboard(FallbackReason),
}

/// Result of comparing the element before and after the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Caret and character count moved exactly as the insertion predicts.
    Inserted,
    /// The element is observably identical to before: nothing was inserted.
    Unchanged,
    /// The element changed, but not exactly as predicted (autocorrect, a
    /// length limit, reformatting). Text landed; falling back would double it.
    ChangedUnexpectedly,
    /// Not enough could be read to tell.
    Unknown,
}

/// What to do after a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Done,
    /// Re-read after a short wait (apps that apply the change asynchronously).
    Wait,
    Fallback,
    Abort,
}

/// Final result of an insertion attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Text is in the field. `exact` is false when it changed unexpectedly.
    Inserted { exact: bool },
    /// Nothing was inserted; use the clipboard path.
    UseClipboard(FallbackReason),
    /// Something may or may not have been inserted. Do not paste.
    Uncertain(String),
}

/// Roles whose `AXSelectedText` is the user's editable input.
const TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox"];

const SECURE_ROLE: &str = "AXSecureTextField";

/// Apps whose focused AX text is not where typed input goes. Terminals expose
/// their scrollback as an `AXTextArea`; input goes to the pty, not the view.
pub(crate) const CLIPBOARD_ONLY_BUNDLES: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "com.github.wez.wezterm",
    "org.alacritty",
    "io.alacritty",
    "com.mitchellh.ghostty",
    "com.mcclowes.saggar",
];

/// How many times to re-read an element that looks unchanged after a
/// successful set, and how long to wait between reads.
pub const VERIFY_POLLS: u32 = 3;
pub const VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(15);
/// Polls for words typed or pasted over the selection instead: a paste can
/// take a few hundred milliseconds to land (`clipboard::PASTE_CONSUME`).
const TYPED_VERIFY_POLLS: u32 = 40;
/// Reads of the selection, [`VERIFY_POLL_INTERVAL`] apart, before words are
/// typed over it: Electron moves it a frame or two after it is set.
const SELECTION_POLLS: u32 = 10;

/// UTF-16 length of `text`, the unit AX ranges and counts use.
pub fn utf16_len(text: &str) -> i64 {
    text.encode_utf16().count() as i64
}

/// Decide whether to insert through Accessibility or use the clipboard.
pub fn choose_strategy(bundle_id: Option<&str>, text: &str, caps: &Capabilities) -> Strategy {
    let is_secure = [&caps.role, &caps.subrole]
        .iter()
        .any(|r| r.as_deref() == Some(SECURE_ROLE));
    if is_secure {
        return Strategy::Clipboard(FallbackReason::SecureField);
    }
    if text.is_empty() {
        return Strategy::Clipboard(FallbackReason::EmptyText);
    }
    if bundle_id.is_some_and(|id| CLIPBOARD_ONLY_BUNDLES.contains(&id)) {
        return Strategy::Clipboard(FallbackReason::ClipboardOnlyApp);
    }
    if !caps
        .role
        .as_deref()
        .is_some_and(|r| TEXT_ROLES.contains(&r))
    {
        return Strategy::Clipboard(FallbackReason::NotATextRole);
    }
    if !caps.selected_text_settable {
        return Strategy::Clipboard(FallbackReason::NotSettable);
    }
    if caps.before.selection.is_none() || caps.before.char_count.is_none() {
        return Strategy::Clipboard(FallbackReason::Unverifiable);
    }
    Strategy::Accessibility
}

/// Compare the element before and after inserting `inserted_utf16` units.
/// `text_matches` is whether the text now at the insertion range equals the
/// inserted text (`None` when it could not be read).
pub fn judge(
    before: &Observation,
    after: &Observation,
    inserted_utf16: i64,
    text_matches: Option<bool>,
) -> Verdict {
    let (Some(sel0), Some(count0)) = (before.selection, before.char_count) else {
        return Verdict::Unknown;
    };
    if after.selection.is_none() && after.char_count.is_none() {
        return Verdict::Unknown;
    }

    let sel_same = after.selection == Some(sel0);
    let count_same = after.char_count == Some(count0);
    if after.selection.is_some() && after.char_count.is_some() && sel_same && count_same {
        return Verdict::Unchanged;
    }

    // At least one readable attribute differs from before.
    let moved =
        (after.selection.is_some() && !sel_same) || (after.char_count.is_some() && !count_same);
    if !moved {
        return Verdict::Unknown;
    }

    let expected_sel = TextRange {
        location: sel0.location + inserted_utf16,
        length: 0,
    };
    let expected_count = count0 - sel0.length + inserted_utf16;
    let sel_ok = after.selection.is_none_or(|s| s == expected_sel);
    let count_ok = after.char_count.is_none_or(|c| c == expected_count);
    if sel_ok && count_ok && text_matches != Some(false) {
        Verdict::Inserted
    } else {
        Verdict::ChangedUnexpectedly
    }
}

/// Turn a verdict into the next step. `set_ok` is whether the AX set call
/// reported success; `polls_left` is how many re-reads remain.
pub fn next_step(verdict: Verdict, set_ok: bool, polls_left: u32) -> Step {
    match verdict {
        Verdict::Inserted | Verdict::ChangedUnexpectedly => Step::Done,
        Verdict::Unchanged if set_ok && polls_left > 0 => Step::Wait,
        Verdict::Unchanged => Step::Fallback,
        Verdict::Unknown if polls_left > 0 => Step::Wait,
        Verdict::Unknown => Step::Abort,
    }
}

/// The focused text element of the target app, as the AX calls see it.
pub trait AxTextTarget {
    fn role(&self) -> Option<String>;
    fn subrole(&self) -> Option<String>;
    fn is_selected_text_settable(&self) -> bool;
    fn observe(&self) -> Observation;
    /// Set `AXSelectedText`. `Err` carries the AX error code.
    fn set_selected_text(&self, text: &str) -> Result<(), i32>;
    fn string_for_range(&self, range: TextRange) -> Option<String>;
    /// Set `AXSelectedTextRange`. `Err` carries the AX error code.
    fn set_selection(&self, range: TextRange) -> Result<(), i32>;
    /// The element's whole `AXValue`, when it is a string.
    fn value(&self) -> Option<String>;
}

/// Read what `target` supports without changing anything.
pub fn probe<T: AxTextTarget>(target: &T) -> Capabilities {
    Capabilities {
        role: target.role(),
        subrole: target.subrole(),
        selected_text_settable: target.is_selected_text_settable(),
        before: target.observe(),
    }
}

// ========================================================================
// Caret context: the text the dictation joins
// (docs/plans/MID_SENTENCE_DICTATION.md)
// ========================================================================

/// UTF-16 units read before the caret: enough to see the end of a word or
/// sentence.
const CONTEXT_BEFORE: i64 = 16;
/// UTF-16 units read after the caret or selection.
const CONTEXT_AFTER: i64 = 4;

/// The text on each side of `before.selection`, as far as it can be read.
/// `before` is an observation taken before anything was written.
pub fn context_at<T: AxTextTarget>(target: &T, before: Observation) -> join::Context {
    let Some(sel) = before.selection else {
        return join::Context::default();
    };
    let start = (sel.location - CONTEXT_BEFORE).max(0);
    let end = sel.location + sel.length;
    join::Context {
        before: side(target, start, sel.location, true),
        after: before
            .char_count
            .and_then(|count| side(target, end, (end + CONTEXT_AFTER).min(count), false)),
    }
}

/// The text in `from..to`. An edge away from the caret can split a
/// surrogate pair, which does not read as a string: then one unit less is
/// read on that side. `caret_at_end` says which side the caret is on.
fn side<T: AxTextTarget>(target: &T, from: i64, to: i64, caret_at_end: bool) -> Option<String> {
    if from >= to {
        return (from == to).then(String::new);
    }
    let range = |from: i64, to: i64| TextRange {
        location: from,
        length: to - from,
    };
    text_in(target, range(from, to)).or_else(|| {
        let shorter = if caret_at_end {
            range(from + 1, to)
        } else {
            range(from, to - 1)
        };
        (shorter.length > 0)
            .then(|| text_in(target, shorter))
            .flatten()
    })
}

/// Whether the text around the caret in `target` may be read: not in
/// secure fields, and not in apps whose AX text is not the input (terminals).
fn context_readable<T: AxTextTarget>(target: &T, bundle_id: Option<&str>) -> bool {
    let secure = [target.role(), target.subrole()]
        .iter()
        .any(|r| r.as_deref() == Some(SECURE_ROLE));
    !secure && !bundle_id.is_some_and(|id| CLIPBOARD_ONLY_BUNDLES.contains(&id))
}

/// The text around the caret in `target`, where dictated text may be
/// fitted to it ([`context_readable`]).
#[cfg(test)]
pub fn caret_context<T: AxTextTarget>(target: &T, bundle_id: Option<&str>) -> join::Context {
    if !context_readable(target, bundle_id) {
        return join::Context::default();
    }
    context_at(target, target.observe())
}

/// UTF-16 units read after the caret beyond the dictated text's own length,
/// for [`overlap`]: room for the field to write the same words longer
/// (`3 p.m.` for `3pm`).
const REPEAT_SLACK: i64 = 32;

/// The field's text from `from` on, as far as a repeat of `text` could
/// reach. Never read past the field's `count` units: there TextEdit and
/// Notes read nothing, and Safari and Gecko a shortened string. Cut short,
/// the last word may be partial and is dropped.
fn text_after<T: AxTextTarget>(target: &T, from: i64, count: i64, text: &str) -> Option<String> {
    let to = (from + utf16_len(text) + REPEAT_SLACK).min(count);
    let mut after = side(target, from, to, false)?;
    if to < count {
        after.truncate(after.rfind(char::is_whitespace).unwrap_or(0));
    }
    Some(after)
}

/// `text` without the words at its end that the field already has right
/// after the caret, fitted to `join`. The field's own text resumes at
/// `from`, and it is `count` units long. Only fitted unless `drop_repeat`
/// (the `voice_edits` beta).
fn fit_without_repeat<T: AxTextTarget>(
    target: &T,
    join: &join::Context,
    from: i64,
    count: Option<i64>,
    text: &str,
    drop_repeat: bool,
) -> String {
    if !drop_repeat {
        return join.fit(text);
    }
    let after = count.and_then(|count| text_after(target, from, count, text));
    join.fit(overlap::without_repeat(
        join.before.as_deref(),
        text,
        after.as_deref(),
    ))
}

/// `text` fitted to the text around the caret in `target`, without a
/// repeat of the words after it, where that text may be read
/// ([`context_readable`]).
pub fn fit_at_caret<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    text: &str,
    drop_repeat: bool,
) -> String {
    if !context_readable(target, bundle_id) {
        return text.to_string();
    }
    let now = target.observe();
    let Some(sel) = now.selection else {
        return text.to_string();
    };
    let join = context_at(target, now);
    fit_without_repeat(
        target,
        &join,
        sel.location + sel.length,
        now.char_count,
        text,
        drop_repeat,
    )
}

/// UTF-16 units read before the caret at key-down, for recognition: the
/// sentence the dictation may continue, and the names in it.
pub const SENTENCE_BEFORE: i64 = 600;

/// Up to `units` of the text before the caret (or selection) in `target`,
/// where it may be read ([`context_readable`]).
pub fn text_before_caret<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    units: i64,
) -> Option<String> {
    if !context_readable(target, bundle_id) {
        return None;
    }
    let sel = target.observe().selection?;
    side(target, (sel.location - units).max(0), sel.location, true)
}

/// [`text_before_caret`] in `pid`'s focused element, [`SENTENCE_BEFORE`]
/// units of it. Blocking.
pub fn sentence_before_focused(pid: i32, bundle_id: Option<&str>) -> Option<String> {
    let element = macos::FocusedElement::of_app(pid)?;
    text_before_caret(&element, bundle_id, SENTENCE_BEFORE)
}

/// [`fit_at_caret`] in `pid`'s focused element, or `text` unchanged where
/// that can't be read. Blocking.
pub fn fit_to_focused(pid: i32, bundle_id: Option<&str>, text: &str) -> String {
    match macos::FocusedElement::of_app(pid) {
        Some(element) => fit_at_caret(&element, bundle_id, text, true),
        None => text.to_string(),
    }
}

// ========================================================================
// Selection: the text a command rewrites (docs/plans/COMMAND_MODE.md)
// ========================================================================

/// What Accessibility says is selected in the focused element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionRead {
    Text(String),
    /// A caret with nothing selected.
    Empty,
    /// The focused text can't be rewritten in place: a secure field, or an
    /// app whose Accessibility text is not its input (a terminal).
    NotEditable,
    /// Accessibility can't tell (no selection range, or its text won't
    /// read). The caller may copy the selection instead.
    Unreadable,
}

/// Read the selection in `target` without changing anything.
pub fn read_selection<T: AxTextTarget>(target: &T, bundle_id: Option<&str>) -> SelectionRead {
    if !context_readable(target, bundle_id) {
        return SelectionRead::NotEditable;
    }
    match target.observe().selection {
        Some(sel) if sel.length == 0 => SelectionRead::Empty,
        Some(sel) => match target.string_for_range(sel) {
            Some(text) if !text.is_empty() => SelectionRead::Text(text),
            _ => SelectionRead::Unreadable,
        },
        None => SelectionRead::Unreadable,
    }
}

/// [`read_selection`] in `pid`'s focused element. Blocking.
pub fn selection_in_focused(pid: i32, bundle_id: Option<&str>) -> SelectionRead {
    match macos::FocusedElement::of_app(pid) {
        Some(element) => read_selection(&element, bundle_id),
        None => SelectionRead::Unreadable,
    }
}

/// Try to insert `text` into `target`, verifying the result. `sleep` is
/// called between re-reads (a real sleep in production, recorded in tests).
pub fn insert_into<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    text: &str,
    mut sleep: impl FnMut(Duration),
) -> Outcome {
    let caps = probe(target);
    if let Strategy::Clipboard(reason) = choose_strategy(bundle_id, text, &caps) {
        return Outcome::UseClipboard(reason);
    }
    let before = caps.before;
    let Some(sel0) = before.selection else {
        return Outcome::UseClipboard(FallbackReason::Unverifiable);
    };
    let inserted = utf16_len(text);
    let inserted_at = TextRange {
        location: sel0.location,
        length: inserted,
    };

    let set_result = target.set_selected_text(text);
    let set_ok = set_result.is_ok();

    let mut polls_left = VERIFY_POLLS;
    loop {
        let after = target.observe();
        let mut verdict = judge(&before, &after, inserted, None);
        if verdict == Verdict::Inserted {
            // Shape matches; check the characters too (only affects `exact`).
            let matches = target.string_for_range(inserted_at).map(|s| s == text);
            verdict = judge(&before, &after, inserted, matches);
        }
        match next_step(verdict, set_ok, polls_left) {
            Step::Done => {
                return Outcome::Inserted {
                    exact: verdict == Verdict::Inserted,
                }
            }
            Step::Fallback => return Outcome::UseClipboard(FallbackReason::NotInserted),
            Step::Abort => {
                return Outcome::Uncertain(format!(
                    "Could not confirm the dictated text was inserted (AX set: {set_result:?}). \
                     It was not pasted again; copy it from Captures if it is missing."
                ))
            }
            Step::Wait => {
                polls_left -= 1;
                sleep(VERIFY_POLL_INTERVAL);
            }
        }
    }
}

// ========================================================================
// Live insertion: cleaned text shown while it is still being generated
// (docs/plans/STREAMING_INSERTION.md)
// ========================================================================

/// Text this dictation has put into the field, and where it starts (UTF-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owned {
    pub start: i64,
    /// The text as written, already fitted to [`Owned::join`].
    pub text: String,
    /// The field around the caret when the first live text was written.
    pub join: join::Context,
}

impl Owned {
    /// Whether `text`, once fitted, grows the owned text.
    pub fn grows_to(&self, text: &str) -> bool {
        let text = self.join.lead(text);
        text.len() > self.text.len() && text.starts_with(self.text.as_str())
    }

    fn range(&self) -> TextRange {
        TextRange {
            location: self.start,
            length: utf16_len(&self.text),
        }
    }

    pub fn end(&self) -> i64 {
        self.start + utf16_len(&self.text)
    }
}

/// Result of the first live write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveStart {
    /// Inserted and read back exactly.
    Started(Owned),
    /// Nothing was inserted. The final text takes today's path.
    Declined(FallbackReason),
    /// Something may have been inserted but can't be tracked. Nothing more
    /// may be written, and the final text must not be pasted on top of it.
    Broken(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveError {
    /// The user changed the text or moved the caret. Nothing is written.
    Edited,
    /// The app did not apply the write; the field still holds the owned text.
    NotApplied,
    /// The field no longer matches what was written, for an unknown reason.
    Uncertain(String),
}

/// The text in `range`, from `AXStringForRange` or else the whole `AXValue`.
fn text_in<T: AxTextTarget>(target: &T, range: TextRange) -> Option<String> {
    if let Some(text) = target.string_for_range(range) {
        return Some(text);
    }
    let value: Vec<u16> = target.value()?.encode_utf16().collect();
    let start = usize::try_from(range.location).ok()?;
    let end = start.checked_add(usize::try_from(range.length).ok()?)?;
    String::from_utf16(value.get(start..end)?).ok()
}

/// The field's state when it still shows exactly `owned` with the caret
/// right after it, which is how this dictation left it.
fn intact<T: AxTextTarget>(target: &T, owned: &Owned) -> Option<Observation> {
    let now = target.observe();
    let caret = TextRange {
        location: owned.end(),
        length: 0,
    };
    (now.selection == Some(caret)
        && now.char_count.is_some()
        && text_in(target, owned.range()).as_deref() == Some(owned.text.as_str()))
    .then_some(now)
}

/// Byte length of the longest common prefix, on a char boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map(|((i, _), _)| i)
        .unwrap_or_else(|| a.len().min(b.len()))
}

/// Byte length of the longest common suffix, on a char boundary.
fn common_suffix(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

/// Whether byte `at` of `text` falls between two letters or digits.
fn mid_word(text: &str, at: usize) -> bool {
    let around = (text[..at].chars().next_back(), text[at..].chars().next());
    matches!(around, (Some(a), Some(b)) if a.is_alphanumeric() && b.is_alphanumeric())
}

/// Types or pastes text over the selection of the focused app, for a field
/// whose `AXSelectedText` can't be set (Messages). False when nothing was
/// sent. Blocking.
pub type TypeIn<'a> = &'a dyn Fn(&str) -> bool;

/// Make the owned text read `text`, rewriting only the part after what the
/// two share, and with `keep_tail` also leaving the end they share in place
/// (a voice edit changes one word), then putting the caret back after it.
/// `before` is the [`intact`] state of the field. With `typed`, the new part
/// is typed or pasted over the selection instead of set over Accessibility,
/// and read back the same way.
fn rewrite<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    before: Observation,
    text: &str,
    keep_tail: bool,
    typed: Option<TypeIn>,
    mut sleep: impl FnMut(Duration),
) -> Result<Owned, LiveError> {
    let mut keep = common_prefix(&owned.text, text);
    let mut tail = 0;
    if keep_tail {
        // Whole words, as the app's undo and autocorrect expect of an edit.
        if mid_word(&owned.text, keep) || mid_word(text, keep) {
            keep = owned.text[..keep]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric())
                .map_or(0, |(i, c)| i + c.len_utf8());
        }
        tail = common_suffix(&owned.text[keep..], &text[keep..]);
        if mid_word(&owned.text, owned.text.len() - tail) || mid_word(text, text.len() - tail) {
            let shared = &owned.text[owned.text.len() - tail..];
            tail -= shared
                .find(|c: char| !c.is_alphanumeric())
                .unwrap_or(shared.len());
        }
    }
    if typed.is_some() && keep + tail == text.len() {
        // Nothing to type for a removal: retype the character before it.
        if let Some((at, _)) = owned.text[..keep].char_indices().next_back() {
            keep = at;
        } else if let Some(c) = owned.text[owned.text.len() - tail..].chars().next() {
            tail -= c.len_utf8();
        }
    }
    let kept16 = utf16_len(&owned.text[..keep]);
    let tail16 = utf16_len(&owned.text[owned.text.len() - tail..]);
    let new_part = &text[keep..text.len() - tail];
    let replaced = TextRange {
        location: owned.start + kept16,
        length: utf16_len(&owned.text) - kept16 - tail16,
    };
    // A write at the caret needs no selection first.
    let selects = replaced.length > 0 || tail > 0;
    if selects && target.set_selection(replaced).is_err() {
        return match intact(target, owned) {
            Some(_) => Err(LiveError::NotApplied),
            None => Err(LiveError::Uncertain(
                "The selection changed while revising the dictated text.".into(),
            )),
        };
    }
    // Typed words land wherever the selection really is: an app that took
    // the selection without moving it would get them in the wrong place.
    // Electron moves it a frame or two later.
    let mut selection_polls_left = SELECTION_POLLS;
    while typed.is_some()
        && selects
        && target.observe().selection != Some(replaced)
        && selection_polls_left > 0
    {
        selection_polls_left -= 1;
        sleep(VERIFY_POLL_INTERVAL);
    }
    if typed.is_some() && selects && target.observe().selection != Some(replaced) {
        let _ = target.set_selection(TextRange {
            location: owned.end(),
            length: 0,
        });
        return Err(LiveError::NotApplied);
    }
    let set_ok = match typed {
        Some(type_in) => type_in(new_part),
        None => target.set_selected_text(new_part).is_ok(),
    };
    let count0 = before.char_count.unwrap_or_default();
    let written = Owned {
        start: owned.start,
        text: text.to_string(),
        join: owned.join.clone(),
    };
    let caret = |location| TextRange {
        location,
        length: 0,
    };
    let expected = Observation {
        selection: Some(caret(replaced.location + utf16_len(new_part))),
        char_count: Some(count0 - utf16_len(&owned.text) + utf16_len(text)),
    };
    let untouched = Observation {
        selection: Some(if selects {
            replaced
        } else {
            caret(owned.end())
        }),
        char_count: Some(count0),
    };
    let mut polls_left = if typed.is_some() {
        TYPED_VERIFY_POLLS
    } else {
        VERIFY_POLLS
    };
    loop {
        let after = target.observe();
        if after == expected && text_in(target, written.range()).as_deref() == Some(text) {
            if tail > 0 {
                // Where the take left it, so the take still reads as intact.
                let _ = target.set_selection(caret(written.end()));
            }
            return Ok(written);
        }
        // Typed words land a few at a time; only the last read counts.
        let settled = typed.is_none() || !set_ok || polls_left == 0;
        if after != untouched && settled {
            return Err(LiveError::Uncertain(
                "The dictated text did not read back as written.".into(),
            ));
        }
        if !set_ok || polls_left == 0 {
            if selects {
                let _ = target.set_selection(caret(owned.end()));
            }
            return Err(LiveError::NotApplied);
        }
        polls_left -= 1;
        sleep(VERIFY_POLL_INTERVAL);
    }
}

/// First live write: insert `text` at the caret (or over the selection),
/// only where the result can be read back and revised later.
pub fn begin_live<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    text: &str,
    sleep: impl FnMut(Duration),
) -> LiveStart {
    let caps = probe(target);
    if let Strategy::Clipboard(reason) = choose_strategy(bundle_id, text, &caps) {
        return LiveStart::Declined(reason);
    }
    let Some(selection) = caps.before.selection else {
        return LiveStart::Declined(FallbackReason::Unverifiable);
    };
    // A revision has to read back what was written: check before writing.
    if text_in(target, selection).is_none() {
        return LiveStart::Declined(FallbackReason::Unverifiable);
    }
    // Read once: after this, the text around the caret includes our own.
    let join = context_at(target, caps.before);
    let text = join.lead(text);
    match insert_into(target, bundle_id, &text, sleep) {
        Outcome::Inserted { .. } => {
            let owned = Owned {
                start: selection.location,
                text,
                join,
            };
            match intact(target, &owned) {
                Some(_) => LiveStart::Started(owned),
                None => LiveStart::Broken(
                    "The app changed the dictated text as it was inserted.".into(),
                ),
            }
        }
        Outcome::UseClipboard(reason) => LiveStart::Declined(reason),
        Outcome::Uncertain(message) => LiveStart::Broken(message),
    }
}

/// Grow the owned text to `text`, which must start with it once fitted.
pub fn extend_live<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    text: &str,
    sleep: impl FnMut(Duration),
) -> Result<Owned, LiveError> {
    let text = &owned.join.lead(text);
    if !text.starts_with(owned.text.as_str()) {
        return Err(LiveError::Uncertain("Live text can only grow.".into()));
    }
    let before = intact(target, owned).ok_or(LiveError::Edited)?;
    if text.len() == owned.text.len() {
        return Ok(owned.clone());
    }
    rewrite(target, owned, before, text, false, None, sleep)
}

/// Make the owned text exactly `final_text`, fitted to the text around it
/// and without a repeat of the words after it (empty removes it), unless
/// the user has touched the field since. Returns the text as it now stands
/// in the field.
pub fn finish_live<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    final_text: &str,
    drop_repeat: bool,
    sleep: impl FnMut(Duration),
) -> Result<Owned, LiveError> {
    let state = intact(target, owned);
    // The field's own text resumes where the owned text ends.
    let count = state.and_then(|s| s.char_count);
    let final_text = &fit_without_repeat(
        target,
        &owned.join,
        owned.end(),
        count,
        final_text,
        drop_repeat,
    );
    if *final_text == owned.text {
        return Ok(owned.clone());
    }
    let before = state.ok_or(LiveError::Edited)?;
    rewrite(target, owned, before, final_text, false, None, sleep)
}

/// [`begin_live`] on the focused element of the app with `pid`. Blocking.
pub fn begin_live_focused(pid: i32, bundle_id: Option<&str>, text: &str) -> LiveStart {
    if !writes_at_caret(pid) {
        return LiveStart::Declined(FallbackReason::WritesAwayFromCaret);
    }
    match macos::FocusedElement::of_app(pid) {
        Some(element) => begin_live(&element, bundle_id, text, std::thread::sleep),
        None => LiveStart::Declined(FallbackReason::NoFocusedElement),
    }
}

/// [`extend_live`] on the focused element of the app with `pid`. Blocking.
/// Focus having moved to another element reads as an edit.
pub fn extend_live_focused(pid: i32, owned: &Owned, text: &str) -> Result<Owned, LiveError> {
    match macos::FocusedElement::of_app(pid) {
        Some(element) => extend_live(&element, owned, text, std::thread::sleep),
        None => Err(LiveError::Edited),
    }
}

/// [`finish_live`] on the focused element of the app with `pid`. Blocking.
pub fn finish_live_focused(pid: i32, owned: &Owned, final_text: &str) -> Result<Owned, LiveError> {
    match macos::FocusedElement::of_app(pid) {
        Some(element) => finish_live(&element, owned, final_text, true, std::thread::sleep),
        None => Err(LiveError::Edited),
    }
}

// ========================================================================
// Voice edits: changing the last take after it went in
// (docs/plans/VOICE_EDITS.md)
// ========================================================================

/// `text` as owned text, when the field shows it right before a bare caret:
/// how a take that was just written leaves it. `None` where it can't be read
/// back (secure fields, terminals, fields that don't expose their text).
pub fn owned_before_caret<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    text: &str,
) -> Option<Owned> {
    if text.is_empty() || !context_readable(target, bundle_id) {
        return None;
    }
    let sel = target.observe().selection.filter(|s| s.length == 0)?;
    let owned = Owned {
        start: sel.location - utf16_len(text),
        text: text.to_string(),
        join: join::Context::default(),
    };
    (owned.start >= 0 && text_in(target, owned.range()).as_deref() == Some(text)).then_some(owned)
}

/// `pid`'s focused field for the log: its role, selection and length,
/// never its text. Blocking.
pub fn describe_focused(pid: i32) -> String {
    let Some(element) = macos::FocusedElement::of_app(pid) else {
        return "no focused element".into();
    };
    let now = element.observe();
    format!(
        "{:?} selection {:?} count {:?}",
        element.role(),
        now.selection,
        now.char_count
    )
}

/// [`owned_before_caret`] in `pid`'s focused element. Blocking.
pub fn owned_before_focused(pid: i32, bundle_id: Option<&str>, text: &str) -> Option<Owned> {
    let element = macos::FocusedElement::of_app(pid)?;
    owned_before_caret(&element, bundle_id, text)
}

/// UTF-16 units before the caret a voice edit may change. Within the
/// server's `LAST_TAKE_CHARS`.
pub const EDITABLE_BEFORE: i64 = 1000;

/// The text right before a bare caret, up to `units` of it from the start
/// of a word: what a voice edit may change, whoever wrote it. `None` where
/// it can't be read or there is none.
pub fn owned_near_caret<T: AxTextTarget>(
    target: &T,
    bundle_id: Option<&str>,
    units: i64,
) -> Option<Owned> {
    if !context_readable(target, bundle_id) {
        return None;
    }
    let caret = target.observe().selection.filter(|s| s.length == 0)?;
    let from = (caret.location - units).max(0);
    let read = text_in(
        target,
        TextRange {
            location: from,
            length: caret.location - from,
        },
    )?;
    // Cut short, the first word may be partial.
    let cut = match from {
        0 => 0,
        _ => read.find(char::is_whitespace)?,
    };
    let text = read[cut..].trim_start();
    if text.trim().is_empty() {
        return None;
    }
    Some(Owned {
        start: from + utf16_len(&read[..read.len() - text.len()]),
        text: text.to_string(),
        join: join::Context::default(),
    })
}

/// [`owned_near_caret`] in `pid`'s focused element, where an edit could be
/// written back. Blocking.
pub fn owned_near_focused(pid: i32, bundle_id: Option<&str>) -> Option<Owned> {
    if !writes_at_caret(pid) {
        return None;
    }
    let element = macos::FocusedElement::of_app(pid)?;
    owned_near_caret(&element, bundle_id, EDITABLE_BEFORE)
}

/// Why a voice edit changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The field no longer shows the take as it was left (the user typed,
    /// moved the caret, or focus moved), or the edit was for other text.
    Changed,
    /// The app did not apply the write; the field is as it was.
    NotApplied,
    /// The app writes Accessibility text away from the caret (Gecko).
    Unsupported,
    /// The field no longer matches what was written, for an unknown reason.
    Uncertain(String),
}

/// Make the owned take end in `after` instead of `before` (the part the
/// server saw), changing only what differs. Nothing is written unless the
/// field still shows the take with the caret after it, and a write the app
/// ignores leaves the field as it was. Where `AXSelectedText` can't be set,
/// the new words go in with `type_in`.
pub fn edit_owned<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    before: &str,
    after: &str,
    writes_at_caret: bool,
    type_in: TypeIn,
    sleep: impl FnMut(Duration),
) -> Result<Owned, EditError> {
    let typed = (!target.is_selected_text_settable()).then_some(type_in);
    edit_owned_with(target, owned, before, after, writes_at_caret, typed, sleep)
}

/// [`edit_owned`], with the new words typed by `typed` when it is given and
/// written over Accessibility when not.
fn edit_owned_with<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    before: &str,
    after: &str,
    writes_at_caret: bool,
    typed: Option<TypeIn>,
    sleep: impl FnMut(Duration),
) -> Result<Owned, EditError> {
    if !writes_at_caret {
        return Err(EditError::Unsupported);
    }
    let head = owned.text.strip_suffix(before).ok_or(EditError::Changed)?;
    let text = format!("{head}{after}");
    let now = intact(target, owned).ok_or(EditError::Changed)?;
    if text == owned.text {
        return Ok(owned.clone());
    }
    rewrite(target, owned, now, &text, true, typed, sleep).map_err(|error| match error {
        LiveError::Edited => EditError::Changed,
        LiveError::NotApplied => EditError::NotApplied,
        LiveError::Uncertain(message) => EditError::Uncertain(message),
    })
}

/// [`edit_owned`] in the focused element of the app with `pid`. Blocking.
/// Focus having moved to another element reads as a change.
pub fn edit_focused(
    pid: i32,
    owned: &Owned,
    before: &str,
    after: &str,
    type_in: TypeIn,
) -> Result<Owned, EditError> {
    let writes = writes_at_caret(pid);
    match macos::FocusedElement::of_app(pid) {
        Some(element) => edit_owned(
            &element,
            owned,
            before,
            after,
            writes,
            type_in,
            std::thread::sleep,
        ),
        None => Err(EditError::Changed),
    }
}

/// A correction saved in Kass, from `before` (the take as Kass last knew
/// it) to `after`, made in the owned take. Like [`edit_owned`], but the new
/// words are typed only with `type_in` (the app is in front and ignores
/// Accessibility writes), and otherwise written over `AXSelectedText` (the
/// app may be behind Kass, where nothing may be typed).
pub fn correct_owned<T: AxTextTarget>(
    target: &T,
    owned: &Owned,
    before: &str,
    after: &str,
    writes_at_caret: bool,
    type_in: Option<TypeIn>,
    sleep: impl FnMut(Duration),
) -> Result<Owned, EditError> {
    if type_in.is_none() && !target.is_selected_text_settable() {
        return Err(EditError::Unsupported);
    }
    // From where they differ: the take's start may have been fitted to the
    // field (a leading space, a capital), so only its end is matched.
    let from = common_prefix(before, after);
    let (before, after) = (&before[from..], &after[from..]);
    if before == after {
        return Err(EditError::Changed);
    }
    edit_owned_with(
        target,
        owned,
        before,
        after,
        writes_at_caret,
        type_in,
        sleep,
    )
}

/// [`correct_owned`] in the focused element of the app with `pid`, which
/// need not be in front unless `type_in` is given. Blocking.
pub fn correct_focused(
    pid: i32,
    owned: &Owned,
    before: &str,
    after: &str,
    type_in: Option<TypeIn>,
) -> Result<Owned, EditError> {
    let writes = writes_at_caret(pid);
    match macos::FocusedElement::of_app(pid) {
        Some(element) => correct_owned(
            &element,
            owned,
            before,
            after,
            writes,
            type_in,
            std::thread::sleep,
        ),
        None => Err(EditError::Changed),
    }
}

/// The whole text of `pid`'s focused element (benchmarks read it back).
#[cfg(test)]
pub fn focused_value(pid: i32) -> Option<String> {
    macos::FocusedElement::of_app(pid)?.value()
}

/// Empty `pid`'s focused element (benchmarks start each run from a blank
/// field).
#[cfg(test)]
pub fn clear_focused(pid: i32) -> bool {
    let Some(element) = macos::FocusedElement::of_app(pid) else {
        return false;
    };
    let length = element.observe().char_count.unwrap_or(0);
    let range = TextRange {
        location: 0,
        length,
    };
    length == 0 || (element.set_selection(range).is_ok() && element.set_selected_text("").is_ok())
}

/// Insert `text` into the focused element of the app with `pid`, verifying
/// the result. Blocking: every step is a synchronous AX message to the target.
pub fn insert_focused(pid: i32, bundle_id: Option<&str>, text: &str) -> Outcome {
    if !writes_at_caret(pid) {
        return Outcome::UseClipboard(FallbackReason::WritesAwayFromCaret);
    }
    match macos::FocusedElement::of_app(pid) {
        Some(element) => insert_into(&element, bundle_id, text, std::thread::sleep),
        None => Outcome::UseClipboard(FallbackReason::NoFocusedElement),
    }
}

/// Map an Accessibility [`Outcome`] onto the fallback chain's [`Attempt`].
pub fn as_attempt(outcome: Outcome) -> Attempt {
    match outcome {
        Outcome::Inserted { .. } => Attempt::Inserted { verified: true },
        Outcome::UseClipboard(reason) => Attempt::Declined(format!("{reason:?}")),
        Outcome::Uncertain(message) => Attempt::Uncertain(message),
    }
}

/// The Accessibility step of the fallback chain: a verified write into the
/// target's focused element. Works without bringing the target to the front.
pub struct Accessibility;

impl Inserter for Accessibility {
    fn method(&self) -> Method {
        Method::Accessibility
    }

    fn attempt(&self, req: &Request) -> Attempt {
        static IGNORED_BY: Mutex<IgnoredWrites> = Mutex::new(IgnoredWrites::new());
        let Ok(mut ignored_by) = IGNORED_BY.lock() else {
            return as_attempt(insert_focused(req.pid, req.bundle_id, req.text));
        };
        ignored_by.attempt(req.pid, || insert_focused(req.pid, req.bundle_id, req.text))
    }
}

/// Apps (by pid) that accepted an Accessibility write that never appeared.
/// Electron apps do this: the set succeeds, and the field stays unchanged
/// through every verify poll, costing ~55 ms on each dictation. After the
/// first time, the step declines at once for that running app and the chain
/// moves straight on. A relaunched app gets a new pid and a fresh try.
struct IgnoredWrites(Vec<i32>);

impl IgnoredWrites {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn attempt(&mut self, pid: i32, insert: impl FnOnce() -> Outcome) -> Attempt {
        if self.0.contains(&pid) {
            return Attempt::Declined("ignored Accessibility writes earlier".into());
        }
        let outcome = insert();
        if outcome == Outcome::UseClipboard(FallbackReason::NotInserted) {
            self.0.push(pid);
        }
        as_attempt(outcome)
    }
}

/// Apps built on Electron keep their accessibility tree off until an
/// assistive app asks for it, so their text fields look like plain groups
/// and the Accessibility step declines. Setting `AXManualAccessibility` on
/// the app turns the tree on (Electron's documented switch for non-VoiceOver
/// tools). The tree builds lazily, so this is called at dictation key-down,
/// seconds before the text is ready.
///
/// Chromium's own `AXEnhancedUserInterface` is never set: it breaks window
/// moving in window managers and can replay typed keys.
pub fn wake_electron(pid: i32) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static WOKEN: Mutex<Option<HashSet<i32>>> = Mutex::new(None);

    let Ok(mut woken) = WOKEN.lock() else {
        return;
    };
    let woken = woken.get_or_insert_with(HashSet::new);
    if woken.contains(&pid) {
        return;
    }
    let Some(path) = crate::focus_capture::app_bundle_path(pid) else {
        return;
    };
    if !is_electron_bundle(&path) {
        return;
    }
    let result = macos::set_manual_accessibility(pid);
    eprintln!("[kass] AXManualAccessibility on {path}: {result:?}");
    if result.is_ok() {
        woken.insert(pid);
    }
}

fn is_electron_bundle(path: &str) -> bool {
    std::path::Path::new(path)
        .join("Contents/Frameworks/Electron Framework.framework")
        .exists()
}

/// Whether an `AXSelectedText` write in the app with `pid` lands at the
/// caret. Gecko apps (Firefox, Zen, Thunderbird) accept the write and put
/// the text at the start of the field, in `<input>`, `<textarea>` and
/// contenteditable alike; in a multi-paragraph editor (X, Draft.js) it also
/// deletes the first paragraph. Setting `AXSelectedTextRange` first does not
/// help. Reading the caret and the text around it works there.
fn writes_at_caret(pid: i32) -> bool {
    !crate::focus_capture::app_bundle_path(pid).is_some_and(|path| is_gecko_bundle(&path))
}

/// Gecko ships its engine as `XUL` next to the app's executable.
fn is_gecko_bundle(path: &str) -> bool {
    std::path::Path::new(path)
        .join("Contents/MacOS/XUL")
        .exists()
}

/// The Accessibility-backed [`AxTextTarget`]: the target app's
/// `AXFocusedUIElement`.
mod macos {
    use super::{AxTextTarget, Observation, TextRange};
    use crate::focus_capture::{cf_string_const, cfstring_to_rust};
    use core_foundation_sys::base::{
        kCFAllocatorDefault, Boolean, CFGetTypeID, CFIndex, CFRange, CFRelease, CFTypeID, CFTypeRef,
    };
    use core_foundation_sys::number::{
        kCFNumberSInt64Type, CFNumberGetTypeID, CFNumberGetValue, CFNumberRef,
    };
    use core_foundation_sys::string::{
        kCFStringEncodingUTF8, CFStringCreateWithBytes, CFStringGetLength, CFStringGetTypeID,
        CFStringRef,
    };
    use std::ffi::c_void;
    use std::ptr;

    type AXUIElementRef = CFTypeRef;
    type AXError = i32;
    const AX_SUCCESS: AXError = 0;
    /// `kAXValueTypeCFRange`.
    const AX_VALUE_CF_RANGE: u32 = 4;
    /// Upper bound on one AX round trip to the target. The default is about
    /// 6 s; a hung target should fail over quickly instead.
    const AX_TIMEOUT_SECS: f32 = 0.25;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementGetTypeID() -> CFTypeID;
        fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout: f32) -> AXError;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementIsAttributeSettable(
            element: AXUIElementRef,
            attribute: CFStringRef,
            settable: *mut Boolean,
        ) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> AXError;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            parameter: CFTypeRef,
            result: *mut CFTypeRef,
        ) -> AXError;
        fn AXValueCreate(value_type: u32, value: *const c_void) -> CFTypeRef;
        fn AXValueGetTypeID() -> CFTypeID;
        fn AXValueGetValue(value: CFTypeRef, value_type: u32, out: *mut c_void) -> Boolean;
    }

    /// An owned (+1) Core Foundation reference, released on drop.
    struct Cf(CFTypeRef);

    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    fn key(name: &str) -> Option<Cf> {
        unsafe { cf_string_const(name).map(|s| Cf(s as CFTypeRef)) }
    }

    fn copy_attr(element: AXUIElementRef, name: &str) -> Option<Cf> {
        let key = key(name)?;
        let mut out: CFTypeRef = ptr::null();
        let err = unsafe { AXUIElementCopyAttributeValue(element, key.0 as CFStringRef, &mut out) };
        if err != AX_SUCCESS || out.is_null() {
            return None;
        }
        Some(Cf(out))
    }

    fn is_type(value: &Cf, type_id: CFTypeID) -> bool {
        unsafe { CFGetTypeID(value.0) == type_id }
    }

    fn as_string(value: &Cf) -> Option<String> {
        if !is_type(value, unsafe { CFStringGetTypeID() }) {
            return None;
        }
        unsafe { cfstring_to_rust(value.0 as CFStringRef) }
    }

    fn as_range(value: &Cf) -> Option<TextRange> {
        if !is_type(value, unsafe { AXValueGetTypeID() }) {
            return None;
        }
        let mut range = CFRange {
            location: 0,
            length: 0,
        };
        let ok = unsafe {
            AXValueGetValue(
                value.0,
                AX_VALUE_CF_RANGE,
                &mut range as *mut CFRange as *mut c_void,
            )
        };
        (ok != 0).then_some(TextRange {
            location: range.location as i64,
            length: range.length as i64,
        })
    }

    fn as_i64(value: &Cf) -> Option<i64> {
        if !is_type(value, unsafe { CFNumberGetTypeID() }) {
            return None;
        }
        let mut n: i64 = 0;
        let ok = unsafe {
            CFNumberGetValue(
                value.0 as CFNumberRef,
                kCFNumberSInt64Type,
                &mut n as *mut i64 as *mut c_void,
            )
        };
        ok.then_some(n)
    }

    /// UTF-16 length of a CFString value (the unit AX counts in).
    fn string_len(value: &Cf) -> Option<i64> {
        if !is_type(value, unsafe { CFStringGetTypeID() }) {
            return None;
        }
        Some(unsafe { CFStringGetLength(value.0 as CFStringRef) } as i64)
    }

    fn cf_string(text: &str) -> Option<Cf> {
        let s = unsafe {
            CFStringCreateWithBytes(
                kCFAllocatorDefault,
                text.as_ptr(),
                text.len() as CFIndex,
                kCFStringEncodingUTF8,
                0,
            )
        };
        (!s.is_null()).then(|| Cf(s as CFTypeRef))
    }

    /// Set `AXManualAccessibility` on the app with `pid`. `Err` carries the
    /// AX error code.
    pub fn set_manual_accessibility(pid: i32) -> Result<(), i32> {
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return Err(-1);
        }
        let app = Cf(app);
        unsafe { AXUIElementSetMessagingTimeout(app.0, AX_TIMEOUT_SECS) };
        let key = key("AXManualAccessibility").ok_or(-1)?;
        let err = unsafe {
            AXUIElementSetAttributeValue(
                app.0,
                key.0 as CFStringRef,
                core_foundation_sys::number::kCFBooleanTrue as CFTypeRef,
            )
        };
        if err == AX_SUCCESS {
            Ok(())
        } else {
            Err(err)
        }
    }

    pub struct FocusedElement {
        element: Cf,
    }

    impl FocusedElement {
        /// The focused element of the app with `pid`, if it exposes one.
        /// Asking the app (not the system-wide element) means the Kass
        /// pill holding key focus cannot redirect the insertion.
        pub fn of_app(pid: i32) -> Option<Self> {
            let app = unsafe { AXUIElementCreateApplication(pid) };
            if app.is_null() {
                return None;
            }
            let app = Cf(app);
            unsafe { AXUIElementSetMessagingTimeout(app.0, AX_TIMEOUT_SECS) };
            let element = copy_attr(app.0, "AXFocusedUIElement")?;
            if !is_type(&element, unsafe { AXUIElementGetTypeID() }) {
                return None;
            }
            unsafe { AXUIElementSetMessagingTimeout(element.0, AX_TIMEOUT_SECS) };
            Some(Self { element })
        }

        fn string_attr(&self, name: &str) -> Option<String> {
            copy_attr(self.element.0, name).as_ref().and_then(as_string)
        }
    }

    impl AxTextTarget for FocusedElement {
        fn role(&self) -> Option<String> {
            self.string_attr("AXRole")
        }

        fn subrole(&self) -> Option<String> {
            self.string_attr("AXSubrole")
        }

        fn is_selected_text_settable(&self) -> bool {
            let Some(key) = key("AXSelectedText") else {
                return false;
            };
            let mut settable: Boolean = 0;
            let err = unsafe {
                AXUIElementIsAttributeSettable(self.element.0, key.0 as CFStringRef, &mut settable)
            };
            err == AX_SUCCESS && settable != 0
        }

        fn observe(&self) -> Observation {
            let selection = copy_attr(self.element.0, "AXSelectedTextRange")
                .as_ref()
                .and_then(as_range);
            let char_count = copy_attr(self.element.0, "AXNumberOfCharacters")
                .as_ref()
                .and_then(as_i64)
                .or_else(|| {
                    copy_attr(self.element.0, "AXValue")
                        .as_ref()
                        .and_then(string_len)
                });
            Observation {
                selection,
                char_count,
            }
        }

        fn set_selected_text(&self, text: &str) -> Result<(), i32> {
            let key = key("AXSelectedText").ok_or(-1)?;
            let value = cf_string(text).ok_or(-1)?;
            let err = unsafe {
                AXUIElementSetAttributeValue(self.element.0, key.0 as CFStringRef, value.0)
            };
            if err == AX_SUCCESS {
                Ok(())
            } else {
                Err(err)
            }
        }

        fn set_selection(&self, range: TextRange) -> Result<(), i32> {
            let key = key("AXSelectedTextRange").ok_or(-1)?;
            let cf_range = CFRange {
                location: range.location as CFIndex,
                length: range.length as CFIndex,
            };
            let value = unsafe {
                AXValueCreate(
                    AX_VALUE_CF_RANGE,
                    &cf_range as *const CFRange as *const c_void,
                )
            };
            if value.is_null() {
                return Err(-1);
            }
            let value = Cf(value);
            let err = unsafe {
                AXUIElementSetAttributeValue(self.element.0, key.0 as CFStringRef, value.0)
            };
            if err == AX_SUCCESS {
                Ok(())
            } else {
                Err(err)
            }
        }

        fn value(&self) -> Option<String> {
            self.string_attr("AXValue")
        }

        fn string_for_range(&self, range: TextRange) -> Option<String> {
            let key = key("AXStringForRange")?;
            let cf_range = CFRange {
                location: range.location as CFIndex,
                length: range.length as CFIndex,
            };
            let param = unsafe {
                AXValueCreate(
                    AX_VALUE_CF_RANGE,
                    &cf_range as *const CFRange as *const c_void,
                )
            };
            if param.is_null() {
                return None;
            }
            let param = Cf(param);
            let mut out: CFTypeRef = ptr::null();
            let err = unsafe {
                AXUIElementCopyParameterizedAttributeValue(
                    self.element.0,
                    key.0 as CFStringRef,
                    param.0,
                    &mut out,
                )
            };
            if err != AX_SUCCESS || out.is_null() {
                return None;
            }
            as_string(&Cf(out))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn range(location: i64, length: i64) -> TextRange {
        TextRange { location, length }
    }

    fn obs(sel: Option<(i64, i64)>, count: Option<i64>) -> Observation {
        Observation {
            selection: sel.map(|(l, n)| range(l, n)),
            char_count: count,
        }
    }

    #[test]
    fn gecko_bundles_are_found_by_their_engine() {
        let root = std::env::temp_dir().join(format!("kass-gecko-{}", std::process::id()));
        let gecko = root.join("Zen.app");
        let other = root.join("TextEdit.app");
        std::fs::create_dir_all(gecko.join("Contents/MacOS")).unwrap();
        std::fs::write(gecko.join("Contents/MacOS/XUL"), b"").unwrap();
        std::fs::create_dir_all(other.join("Contents/MacOS")).unwrap();
        assert!(is_gecko_bundle(gecko.to_str().unwrap()));
        assert!(!is_gecko_bundle(other.to_str().unwrap()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn text_area(before: Observation) -> Capabilities {
        Capabilities {
            role: Some("AXTextArea".into()),
            subrole: None,
            selected_text_settable: true,
            before,
        }
    }

    // ---- choose_strategy ----

    #[test]
    fn settable_text_area_with_readable_state_uses_accessibility() {
        let caps = text_area(obs(Some((3, 0)), Some(10)));
        assert_eq!(
            choose_strategy(Some("com.apple.TextEdit"), "hi", &caps),
            Strategy::Accessibility
        );
    }

    #[test]
    fn text_field_and_combo_box_are_text_roles() {
        for role in ["AXTextField", "AXComboBox"] {
            let mut caps = text_area(obs(Some((0, 0)), Some(0)));
            caps.role = Some(role.into());
            assert_eq!(
                choose_strategy(None, "hi", &caps),
                Strategy::Accessibility,
                "{role}"
            );
        }
    }

    #[test]
    fn secure_subrole_never_uses_accessibility() {
        let mut caps = text_area(obs(Some((0, 0)), Some(0)));
        caps.role = Some("AXTextField".into());
        caps.subrole = Some("AXSecureTextField".into());
        assert_eq!(
            choose_strategy(None, "hunter2", &caps),
            Strategy::Clipboard(FallbackReason::SecureField)
        );
    }

    #[test]
    fn secure_role_never_uses_accessibility() {
        let mut caps = text_area(obs(Some((0, 0)), Some(0)));
        caps.role = Some("AXSecureTextField".into());
        assert_eq!(
            choose_strategy(None, "hunter2", &caps),
            Strategy::Clipboard(FallbackReason::SecureField)
        );
    }

    #[test]
    fn secure_check_wins_over_everything_else() {
        let caps = Capabilities {
            role: Some("AXSecureTextField".into()),
            subrole: None,
            selected_text_settable: false,
            before: Observation::default(),
        };
        assert_eq!(
            choose_strategy(Some("com.apple.Terminal"), "x", &caps),
            Strategy::Clipboard(FallbackReason::SecureField)
        );
    }

    #[test]
    fn terminals_use_clipboard() {
        let caps = text_area(obs(Some((0, 0)), Some(0)));
        for id in [
            "com.apple.Terminal",
            "com.googlecode.iterm2",
            "com.mitchellh.ghostty",
        ] {
            assert_eq!(
                choose_strategy(Some(id), "ls", &caps),
                Strategy::Clipboard(FallbackReason::ClipboardOnlyApp),
                "{id}"
            );
        }
    }

    #[test]
    fn non_text_role_uses_clipboard() {
        for role in [Some("AXWebArea"), Some("AXButton"), None] {
            let mut caps = text_area(obs(Some((0, 0)), Some(0)));
            caps.role = role.map(Into::into);
            assert_eq!(
                choose_strategy(None, "hi", &caps),
                Strategy::Clipboard(FallbackReason::NotATextRole),
                "{role:?}"
            );
        }
    }

    #[test]
    fn unsettable_selected_text_uses_clipboard() {
        let mut caps = text_area(obs(Some((0, 0)), Some(0)));
        caps.selected_text_settable = false;
        assert_eq!(
            choose_strategy(None, "hi", &caps),
            Strategy::Clipboard(FallbackReason::NotSettable)
        );
    }

    #[test]
    fn unreadable_selection_or_count_is_unverifiable() {
        for before in [obs(None, Some(5)), obs(Some((0, 0)), None), obs(None, None)] {
            let caps = text_area(before);
            assert_eq!(
                choose_strategy(None, "hi", &caps),
                Strategy::Clipboard(FallbackReason::Unverifiable),
                "{before:?}"
            );
        }
    }

    #[test]
    fn empty_text_uses_clipboard_path() {
        let caps = text_area(obs(Some((0, 0)), Some(0)));
        assert_eq!(
            choose_strategy(None, "", &caps),
            Strategy::Clipboard(FallbackReason::EmptyText)
        );
    }

    // ---- judge ----

    #[test]
    fn caret_and_count_as_predicted_is_inserted() {
        let before = obs(Some((3, 0)), Some(10));
        let after = obs(Some((5, 0)), Some(12));
        assert_eq!(judge(&before, &after, 2, Some(true)), Verdict::Inserted);
    }

    #[test]
    fn replacing_a_selection_is_inserted() {
        // "hello [world]" with "world" (5) selected, replaced by "there!" (6).
        let before = obs(Some((6, 5)), Some(11));
        let after = obs(Some((12, 0)), Some(12));
        assert_eq!(judge(&before, &after, 6, Some(true)), Verdict::Inserted);
    }

    #[test]
    fn replacing_selection_with_same_length_text_is_inserted_by_caret() {
        let before = obs(Some((0, 4)), Some(4));
        let after = obs(Some((4, 0)), Some(4));
        assert_eq!(judge(&before, &after, 4, None), Verdict::Inserted);
    }

    #[test]
    fn identical_state_is_unchanged() {
        let before = obs(Some((3, 0)), Some(10));
        assert_eq!(judge(&before, &before, 2, Some(false)), Verdict::Unchanged);
    }

    #[test]
    fn identical_state_is_unchanged_even_if_following_text_matches() {
        // Caret sits before an existing copy of the same words.
        let before = obs(Some((3, 0)), Some(10));
        assert_eq!(judge(&before, &before, 2, Some(true)), Verdict::Unchanged);
    }

    #[test]
    fn predicted_shape_but_different_text_is_changed_unexpectedly() {
        // Smart quotes: same length, different characters.
        let before = obs(Some((0, 0)), Some(0));
        let after = obs(Some((5, 0)), Some(5));
        assert_eq!(
            judge(&before, &after, 5, Some(false)),
            Verdict::ChangedUnexpectedly
        );
    }

    #[test]
    fn truncated_by_length_limit_is_changed_unexpectedly() {
        let before = obs(Some((0, 0)), Some(0));
        let after = obs(Some((3, 0)), Some(3));
        assert_eq!(
            judge(&before, &after, 10, None),
            Verdict::ChangedUnexpectedly
        );
    }

    #[test]
    fn caret_left_in_place_but_count_grew_is_changed_unexpectedly() {
        let before = obs(Some((0, 0)), Some(0));
        let after = obs(Some((0, 0)), Some(5));
        assert_eq!(
            judge(&before, &after, 5, None),
            Verdict::ChangedUnexpectedly
        );
    }

    #[test]
    fn unreadable_after_is_unknown() {
        let before = obs(Some((0, 0)), Some(0));
        assert_eq!(judge(&before, &obs(None, None), 5, None), Verdict::Unknown);
    }

    #[test]
    fn partially_readable_after_that_looks_unchanged_is_unknown() {
        // Count unchanged but selection unreadable: cannot prove nothing landed.
        let before = obs(Some((0, 0)), Some(0));
        assert_eq!(
            judge(&before, &obs(None, Some(0)), 5, None),
            Verdict::Unknown
        );
    }

    #[test]
    fn partially_readable_after_matching_prediction_is_inserted() {
        let before = obs(Some((0, 0)), Some(0));
        assert_eq!(
            judge(&before, &obs(None, Some(5)), 5, Some(true)),
            Verdict::Inserted
        );
        assert_eq!(
            judge(&before, &obs(Some((5, 0)), None), 5, Some(true)),
            Verdict::Inserted
        );
    }

    #[test]
    fn unreadable_before_is_unknown() {
        let after = obs(Some((5, 0)), Some(5));
        assert_eq!(judge(&obs(None, None), &after, 5, None), Verdict::Unknown);
    }

    // ---- next_step ----

    #[test]
    fn inserted_and_changed_are_done() {
        for v in [Verdict::Inserted, Verdict::ChangedUnexpectedly] {
            for set_ok in [true, false] {
                assert_eq!(next_step(v, set_ok, 3), Step::Done);
                assert_eq!(next_step(v, set_ok, 0), Step::Done);
            }
        }
    }

    #[test]
    fn unchanged_after_rejected_set_falls_back_immediately() {
        assert_eq!(next_step(Verdict::Unchanged, false, 3), Step::Fallback);
    }

    #[test]
    fn unchanged_after_accepted_set_waits_then_falls_back() {
        assert_eq!(next_step(Verdict::Unchanged, true, 2), Step::Wait);
        assert_eq!(next_step(Verdict::Unchanged, true, 0), Step::Fallback);
    }

    #[test]
    fn unknown_waits_then_aborts_never_falls_back() {
        for set_ok in [true, false] {
            assert_eq!(next_step(Verdict::Unknown, set_ok, 1), Step::Wait);
            assert_eq!(next_step(Verdict::Unknown, set_ok, 0), Step::Abort);
        }
    }

    // ---- insert_into with a fake element ----

    /// A fake text field holding UTF-16 text and a selection. Behaviour knobs
    /// model real-app quirks.
    struct FakeField {
        role: Option<String>,
        subrole: Option<String>,
        settable: bool,
        text: RefCell<Vec<u16>>,
        sel: Cell<TextRange>,
        /// What the set call returns.
        set_result: Result<(), i32>,
        /// Whether the set actually changes the text.
        applies: bool,
        /// Number of observations after the set before the change shows.
        apply_delay_reads: Cell<u32>,
        pending: RefCell<Option<String>>,
        /// Reads return nothing after the set (app went unresponsive).
        blind_after_set: bool,
        set_calls: Cell<u32>,
        /// AXStringForRange is unsupported.
        ranges_unreadable: bool,
        /// Every text written with `set_selected_text`.
        written: RefCell<Vec<String>>,
        /// Setting the selection succeeds and doesn't move it.
        ignores_selection: bool,
        /// Reads after setting the selection before it shows (Electron).
        selection_lag: u32,
        pending_sel: Cell<Option<(TextRange, u32)>>,
    }

    impl FakeField {
        fn new(text: &str, sel: TextRange) -> Self {
            Self {
                role: Some("AXTextArea".into()),
                subrole: None,
                settable: true,
                text: RefCell::new(text.encode_utf16().collect()),
                sel: Cell::new(sel),
                set_result: Ok(()),
                applies: true,
                apply_delay_reads: Cell::new(0),
                pending: RefCell::new(None),
                blind_after_set: false,
                set_calls: Cell::new(0),
                ranges_unreadable: false,
                written: RefCell::new(Vec::new()),
                ignores_selection: false,
                selection_lag: 0,
                pending_sel: Cell::new(None),
            }
        }

        /// The user types `text` at `at` and leaves the caret after it.
        fn user_types(&self, at: i64, text: &str) {
            self.sel.set(range(at, 0));
            self.apply(text);
        }

        fn contents(&self) -> String {
            String::from_utf16(&self.text.borrow()).unwrap()
        }

        fn apply(&self, text: &str) {
            let sel = self.sel.get();
            let new: Vec<u16> = text.encode_utf16().collect();
            let start = sel.location as usize;
            let end = start + sel.length as usize;
            self.text
                .borrow_mut()
                .splice(start..end, new.iter().copied());
            self.sel.set(range(sel.location + new.len() as i64, 0));
        }
    }

    impl AxTextTarget for FakeField {
        fn role(&self) -> Option<String> {
            self.role.clone()
        }
        fn subrole(&self) -> Option<String> {
            self.subrole.clone()
        }
        fn is_selected_text_settable(&self) -> bool {
            self.settable
        }
        fn observe(&self) -> Observation {
            if self.set_calls.get() > 0 && self.blind_after_set {
                return Observation::default();
            }
            let pending = self.pending.borrow().clone();
            if let Some(t) = pending {
                let left = self.apply_delay_reads.get();
                if left == 0 {
                    self.pending.borrow_mut().take();
                    self.apply(&t);
                } else {
                    self.apply_delay_reads.set(left - 1);
                }
            }
            if let Some((sel, left)) = self.pending_sel.get() {
                if left == 0 {
                    self.pending_sel.set(None);
                    self.sel.set(sel);
                } else {
                    self.pending_sel.set(Some((sel, left - 1)));
                }
            }
            Observation {
                selection: Some(self.sel.get()),
                char_count: Some(self.text.borrow().len() as i64),
            }
        }
        fn set_selected_text(&self, text: &str) -> Result<(), i32> {
            self.set_calls.set(self.set_calls.get() + 1);
            self.written.borrow_mut().push(text.to_string());
            if self.applies {
                if self.apply_delay_reads.get() == 0 {
                    self.apply(text);
                } else {
                    *self.pending.borrow_mut() = Some(text.to_string());
                }
            }
            self.set_result
        }
        fn value(&self) -> Option<String> {
            (!self.ranges_unreadable).then(|| self.contents())
        }
        fn set_selection(&self, r: TextRange) -> Result<(), i32> {
            if r.location < 0 || r.location + r.length > self.text.borrow().len() as i64 {
                return Err(-25201);
            }
            if self.selection_lag > 0 {
                self.pending_sel.set(Some((r, self.selection_lag)));
            } else if !self.ignores_selection {
                self.sel.set(r);
            }
            Ok(())
        }
        fn string_for_range(&self, r: TextRange) -> Option<String> {
            if self.ranges_unreadable {
                return None;
            }
            let t = self.text.borrow();
            let start = r.location as usize;
            let end = start + r.length as usize;
            // A split surrogate pair does not read, as with a real element.
            t.get(start..end).and_then(|s| String::from_utf16(s).ok())
        }
    }

    fn run(field: &FakeField, bundle: Option<&str>, text: &str) -> (Outcome, Vec<Duration>) {
        let mut sleeps = Vec::new();
        let out = insert_into(field, bundle, text, |d| sleeps.push(d));
        (out, sleeps)
    }

    #[test]
    fn inserts_at_caret_without_waiting() {
        let field = FakeField::new("Hello world", range(5, 0));
        let (out, sleeps) = run(&field, Some("com.apple.TextEdit"), ", dear");
        assert_eq!(out, Outcome::Inserted { exact: true });
        assert_eq!(field.contents(), "Hello, dear world");
        assert!(sleeps.is_empty());
        assert_eq!(field.set_calls.get(), 1);
    }

    #[test]
    fn replaces_selection() {
        let field = FakeField::new("Hello world", range(6, 5));
        let (out, _) = run(&field, None, "there");
        assert_eq!(out, Outcome::Inserted { exact: true });
        assert_eq!(field.contents(), "Hello there");
    }

    #[test]
    fn reads_the_selected_text_for_a_command() {
        let field = FakeField::new("Hello 👍 world", range(6, 8));
        assert_eq!(
            read_selection(&field, Some("com.apple.TextEdit")),
            SelectionRead::Text("👍 world".into())
        );
    }

    #[test]
    fn a_bare_caret_is_no_selection() {
        let field = FakeField::new("Hello world", range(5, 0));
        assert_eq!(read_selection(&field, None), SelectionRead::Empty);
    }

    #[test]
    fn a_selection_whose_text_will_not_read_is_left_to_the_copy() {
        let mut field = FakeField::new("Hello world", range(0, 5));
        field.ranges_unreadable = true;
        assert_eq!(read_selection(&field, None), SelectionRead::Unreadable);
    }

    #[test]
    fn terminals_and_password_fields_have_no_editable_selection() {
        let field = FakeField::new("ls -la", range(0, 2));
        assert_eq!(
            read_selection(&field, Some("com.apple.Terminal")),
            SelectionRead::NotEditable
        );
        let mut secret = FakeField::new("hunter2", range(0, 7));
        secret.role = Some(SECURE_ROLE.into());
        assert_eq!(read_selection(&secret, None), SelectionRead::NotEditable);
    }

    #[test]
    fn counts_emoji_in_utf16_units() {
        let field = FakeField::new("ab", range(1, 0));
        let (out, _) = run(&field, None, "👍 ok");
        assert_eq!(out, Outcome::Inserted { exact: true });
        assert_eq!(field.contents(), "a👍 okb");
    }

    #[test]
    fn secure_field_is_never_touched() {
        let mut field = FakeField::new("", range(0, 0));
        field.subrole = Some("AXSecureTextField".into());
        let (out, _) = run(&field, None, "secret");
        assert_eq!(out, Outcome::UseClipboard(FallbackReason::SecureField));
        assert_eq!(field.set_calls.get(), 0);
    }

    #[test]
    fn unsettable_field_is_never_touched() {
        let mut field = FakeField::new("", range(0, 0));
        field.settable = false;
        let (out, _) = run(&field, None, "x");
        assert_eq!(out, Outcome::UseClipboard(FallbackReason::NotSettable));
        assert_eq!(field.set_calls.get(), 0);
    }

    #[test]
    fn rejected_set_with_no_change_falls_back_without_waiting() {
        let mut field = FakeField::new("abc", range(3, 0));
        field.applies = false;
        field.set_result = Err(-25205);
        let (out, sleeps) = run(&field, None, "x");
        assert_eq!(out, Outcome::UseClipboard(FallbackReason::NotInserted));
        assert!(sleeps.is_empty());
    }

    #[test]
    fn accepted_set_that_never_lands_falls_back_after_polls() {
        // An app that says yes but ignores the write.
        let mut field = FakeField::new("abc", range(3, 0));
        field.applies = false;
        let (out, sleeps) = run(&field, None, "x");
        assert_eq!(out, Outcome::UseClipboard(FallbackReason::NotInserted));
        assert_eq!(sleeps, vec![VERIFY_POLL_INTERVAL; VERIFY_POLLS as usize]);
        assert_eq!(field.contents(), "abc");
    }

    #[test]
    fn late_applying_app_is_detected_and_not_pasted_twice() {
        let field = FakeField::new("abc", range(3, 0));
        field.apply_delay_reads.set(2);
        let (out, sleeps) = run(&field, None, "de");
        assert_eq!(out, Outcome::Inserted { exact: true });
        assert_eq!(field.contents(), "abcde");
        assert_eq!(sleeps.len(), 2);
    }

    #[test]
    fn rejected_set_that_still_landed_is_not_pasted_twice() {
        let mut field = FakeField::new("abc", range(3, 0));
        field.set_result = Err(-25204);
        let (out, _) = run(&field, None, "de");
        assert_eq!(out, Outcome::Inserted { exact: true });
    }

    #[test]
    fn unreadable_after_set_is_uncertain_not_fallback() {
        let mut field = FakeField::new("abc", range(3, 0));
        field.blind_after_set = true;
        let (out, sleeps) = run(&field, None, "de");
        assert!(matches!(out, Outcome::Uncertain(_)), "{out:?}");
        assert_eq!(sleeps.len(), VERIFY_POLLS as usize);
    }

    // ---- caret context ----

    #[test]
    fn context_is_read_around_the_caret() {
        let field = FakeField::new("I think we should move it now", range(17, 0));
        assert_eq!(
            caret_context(&field, None),
            join::Context {
                before: Some(" think we should".into()),
                after: Some(" mov".into()),
            }
        );
    }

    #[test]
    fn context_is_read_around_a_selection() {
        let field = FakeField::new("Hello world, friend", range(6, 5));
        let ctx = caret_context(&field, None);
        assert_eq!(ctx.before.as_deref(), Some("Hello "));
        assert_eq!(ctx.after.as_deref(), Some(", fr"));
    }

    #[test]
    fn context_is_limited_to_a_few_characters() {
        let field = FakeField::new("0123456789abcdefghijXYZ", range(20, 0));
        assert_eq!(
            caret_context(&field, None).before.as_deref(),
            Some("456789abcdefghij")
        );
    }

    #[test]
    fn field_edges_read_as_empty() {
        let field = FakeField::new("", range(0, 0));
        assert_eq!(
            caret_context(&field, None),
            join::Context {
                before: Some(String::new()),
                after: Some(String::new()),
            }
        );
    }

    #[test]
    fn context_skips_a_split_surrogate_pair() {
        // 👍 is two UTF-16 units; 16 units back lands between them.
        let field = FakeField::new("👍abcdefghijklmno pq", range(17, 0));
        assert_eq!(
            caret_context(&field, None).before.as_deref(),
            Some("abcdefghijklmno")
        );
        let field = FakeField::new("ab cd👍", range(2, 0));
        assert_eq!(caret_context(&field, None).after.as_deref(), Some(" cd"));
    }

    #[test]
    fn unreadable_text_gives_no_context() {
        let mut field = FakeField::new("abc", range(1, 0));
        field.ranges_unreadable = true;
        assert_eq!(caret_context(&field, None), join::Context::default());
    }

    #[test]
    fn text_before_the_caret_is_read_up_to_a_limit() {
        let field = FakeField::new("Hello there. I think we should", range(30, 0));
        assert_eq!(
            text_before_caret(&field, None, 600).as_deref(),
            Some("Hello there. I think we should")
        );
        assert_eq!(
            text_before_caret(&field, None, 9).as_deref(),
            Some("we should")
        );
        assert_eq!(
            text_before_caret(&field, Some("com.apple.Terminal"), 600),
            None
        );
    }

    #[test]
    fn terminals_and_secure_fields_give_no_context() {
        let field = FakeField::new("$ ", range(2, 0));
        assert_eq!(
            caret_context(&field, Some("com.apple.Terminal")),
            join::Context::default()
        );
        let mut field = FakeField::new("secret", range(6, 0));
        field.subrole = Some("AXSecureTextField".into());
        assert_eq!(caret_context(&field, None), join::Context::default());
    }

    // ---- re-dictation cleanup ----

    #[test]
    fn a_repeat_of_the_words_after_the_caret_is_dropped() {
        let field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        let text = fit_at_caret(&field, None, "on Friday at noon tomorrow.", true);
        assert_eq!(text, "on Friday ");
        field.apply(&text);
        assert_eq!(field.contents(), "Let's meet on Friday at noon tomorrow.");
    }

    #[test]
    fn a_repeat_is_kept_outside_the_beta() {
        let field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        assert_eq!(
            fit_at_caret(&field, None, "on Friday at noon tomorrow.", false),
            "on Friday at noon tomorrow "
        );
        let field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        let owned = started(live(&field, "on Friday"));
        finish_live(&field, &owned, "on Friday at noon tomorrow.", false, |_| {}).unwrap();
        assert_eq!(
            field.contents(),
            "Let's meet on Friday at noon tomorrow at noon tomorrow."
        );
    }

    #[test]
    fn a_repeat_over_a_selection_is_dropped() {
        let field = FakeField::new("Let's meet on Monday at noon.", range(11, 9));
        assert_eq!(
            fit_at_caret(&field, None, "on Friday at noon.", true),
            "on Friday"
        );
    }

    #[test]
    fn no_repeat_is_only_fitted() {
        let field = FakeField::new("Can you before lunch?", range(8, 0));
        assert_eq!(
            fit_at_caret(&field, None, "send the report.", true),
            "send the report "
        );
    }

    #[test]
    fn the_repeat_is_kept_where_the_field_cannot_be_read() {
        let field = FakeField::new("$ git commit -m", range(2, 0));
        assert_eq!(
            fit_at_caret(
                &field,
                Some("com.apple.Terminal"),
                "run git commit -m",
                true
            ),
            "run git commit -m"
        );
        let mut field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        field.ranges_unreadable = true;
        assert_eq!(
            fit_at_caret(&field, None, "on Friday at noon tomorrow.", true),
            "on Friday at noon tomorrow."
        );
    }

    #[test]
    fn text_after_the_caret_is_read_as_far_as_a_repeat_could_reach() {
        let rest = "at noon tomorrow with everyone on the team, then lunch at the usual place.";
        let field = FakeField::new(&format!("Let's meet {rest}"), range(11, 0));
        let count = field.observe().char_count.unwrap();
        // "on Friday" + 32 units ends inside "team": the partial word is dropped.
        assert_eq!(
            text_after(&field, 11, count, "on Friday").as_deref(),
            Some("at noon tomorrow with everyone on the")
        );
        // Never past the end of the field.
        assert_eq!(
            text_after(&field, 11, count, &"word ".repeat(40)).as_deref(),
            Some(rest)
        );
        assert_eq!(
            text_after(&field, count, count, "on Friday").as_deref(),
            Some("")
        );
    }

    #[test]
    fn live_text_drops_a_repeat_when_it_is_final() {
        let field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        let owned = started(live(&field, "on Friday"));
        let owned = extend_live(&field, &owned, "on Friday at noon", |_| {}).unwrap();
        // Drafts may show the repeat for a moment.
        assert_eq!(
            field.contents(),
            "Let's meet on Friday at noonat noon tomorrow."
        );
        let calls = field.set_calls.get();
        finish_live(&field, &owned, "on Friday at noon tomorrow.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "Let's meet on Friday at noon tomorrow.");
        assert_eq!(field.sel.get(), range(21, 0));
        // One verified write shrinks the owned text.
        assert_eq!(field.set_calls.get(), calls + 1);
    }

    #[test]
    fn live_text_that_stops_before_the_repeat_only_gets_its_space() {
        let field = FakeField::new("Let's meet at noon tomorrow.", range(11, 0));
        let owned = started(live(&field, "on Friday"));
        let calls = field.set_calls.get();
        finish_live(&field, &owned, "on Friday at noon.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "Let's meet on Friday at noon tomorrow.");
        assert_eq!(field.set_calls.get(), calls + 1);
    }

    #[test]
    fn terminal_is_never_touched() {
        let field = FakeField::new("$ ", range(2, 0));
        let (out, _) = run(&field, Some("com.apple.Terminal"), "ls");
        assert_eq!(out, Outcome::UseClipboard(FallbackReason::ClipboardOnlyApp));
        assert_eq!(field.set_calls.get(), 0);
    }

    // ---- live insertion ----

    fn live(field: &FakeField, text: &str) -> LiveStart {
        begin_live(field, Some("com.apple.TextEdit"), text, |_| {})
    }

    fn started(outcome: LiveStart) -> Owned {
        match outcome {
            LiveStart::Started(owned) => owned,
            other => panic!("not started: {other:?}"),
        }
    }

    #[test]
    fn live_text_grows_at_the_caret_and_is_owned() {
        let field = FakeField::new("Dear team, ", range(11, 0));
        let owned = started(live(&field, "The first"));
        assert_eq!(
            owned,
            Owned {
                start: 11,
                text: "The first".into(),
                join: join::Context {
                    before: Some("Dear team, ".into()),
                    after: Some(String::new()),
                },
            }
        );
        let owned = extend_live(&field, &owned, "The first part is", |_| {}).unwrap();
        assert_eq!(field.contents(), "Dear team, The first part is");
        assert_eq!(owned.text, "The first part is");
        assert_eq!(field.sel.get(), range(28, 0));
    }

    #[test]
    fn final_text_that_extends_the_live_text_is_appended() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "Hello there"));
        finish_live(&field, &owned, "Hello there, my friend.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "Hello there, my friend.");
        assert_eq!(field.sel.get(), range(23, 0));
    }

    #[test]
    fn a_revised_final_replaces_only_what_this_dictation_inserted() {
        let field = FakeField::new("Before. After", range(8, 0));
        let owned = started(live(&field, "Send the update"));
        assert_eq!(field.contents(), "Before. Send the updateAfter");
        finish_live(
            &field,
            &owned,
            "Do not send the update to the team.",
            true,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            field.contents(),
            "Before. Do not send the update to the team. After"
        );
        assert_eq!(field.sel.get(), range(8 + 36, 0));
    }

    #[test]
    fn a_revision_keeps_the_common_start_in_place() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "It might. But"));
        let calls = field.set_calls.get();
        finish_live(&field, &owned, "It might, but we'll see", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "It might, but we'll see");
        // One replacement of the changed tail.
        assert_eq!(field.set_calls.get(), calls + 1);
    }

    #[test]
    fn empty_final_removes_the_live_text() {
        let field = FakeField::new("keep ", range(5, 0));
        let owned = started(live(&field, "Um so"));
        finish_live(&field, &owned, "", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "keep ");
        assert_eq!(field.sel.get(), range(5, 0));
    }

    #[test]
    fn unicode_is_measured_in_utf16_units() {
        let field = FakeField::new("x", range(1, 0));
        let owned = started(live(&field, "Café 👍"));
        let owned = extend_live(&field, &owned, "Café 👍 and", |_| {}).unwrap();
        finish_live(&field, &owned, "Café 👍 and naïve.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "x Café 👍 and naïve.");
    }

    #[test]
    fn live_text_joins_the_sentence_around_the_caret() {
        let field = FakeField::new("Can you before lunch?", range(7, 0));
        let owned = started(live(&field, "send"));
        assert_eq!(field.contents(), "Can you send before lunch?");
        assert!(owned.grows_to("send the"));
        let owned = extend_live(&field, &owned, "send the", |_| {}).unwrap();
        finish_live(&field, &owned, "send the report.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "Can you send the report before lunch?");
    }

    #[test]
    fn live_text_after_a_word_gets_a_space() {
        let field = FakeField::new("I think we should", range(17, 0));
        let owned = started(live(&field, "move"));
        assert_eq!(owned.text, " move");
        assert!(owned.grows_to("move it"));
        assert!(!owned.grows_to("move"));
        finish_live(&field, &owned, "move it.", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "I think we should move it.");
    }

    #[test]
    fn user_typing_stops_live_insertion_and_is_never_overwritten() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "Hello there"));
        field.user_types(11, "!");
        assert!(matches!(
            extend_live(&field, &owned, "Hello there my", |_| {}),
            Err(LiveError::Edited)
        ));
        assert!(matches!(
            finish_live(&field, &owned, "Goodbye.", true, |_| {}),
            Err(LiveError::Edited)
        ));
        assert_eq!(field.contents(), "Hello there!");
    }

    #[test]
    fn user_editing_inside_the_live_text_is_never_overwritten() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "Hello there"));
        field.user_types(0, "Oh ");
        field.sel.set(range(14, 0));
        assert!(matches!(
            finish_live(&field, &owned, "Hello there.", true, |_| {}),
            Err(LiveError::Edited)
        ));
        assert_eq!(field.contents(), "Oh Hello there");
    }

    #[test]
    fn a_moved_caret_counts_as_an_edit() {
        let field = FakeField::new("abc ", range(4, 0));
        let owned = started(live(&field, "Hi"));
        field.sel.set(range(0, 0));
        assert!(matches!(
            finish_live(&field, &owned, "Hi there.", true, |_| {}),
            Err(LiveError::Edited)
        ));
        assert_eq!(field.contents(), "abc Hi");
    }

    #[test]
    fn live_insertion_is_declined_where_it_cannot_be_verified() {
        let mut field = FakeField::new("", range(0, 0));
        field.ranges_unreadable = true;
        assert_eq!(
            live(&field, "Hello"),
            LiveStart::Declined(FallbackReason::Unverifiable)
        );
        assert_eq!(field.set_calls.get(), 0);
    }

    #[test]
    fn terminals_and_secure_fields_never_get_live_text() {
        let field = FakeField::new("$ ", range(2, 0));
        assert_eq!(
            begin_live(&field, Some("com.googlecode.iterm2"), "ls", |_| {}),
            LiveStart::Declined(FallbackReason::ClipboardOnlyApp)
        );
        let mut field = FakeField::new("", range(0, 0));
        field.role = Some("AXSecureTextField".into());
        assert_eq!(
            live(&field, "pw"),
            LiveStart::Declined(FallbackReason::SecureField)
        );
    }

    #[test]
    fn an_app_that_ignores_the_first_write_is_declined_with_nothing_inserted() {
        let mut field = FakeField::new("abc", range(3, 0));
        field.applies = false;
        assert_eq!(
            live(&field, "Hello"),
            LiveStart::Declined(FallbackReason::NotInserted)
        );
        assert_eq!(field.contents(), "abc");
    }

    #[test]
    fn a_first_write_that_lands_differently_is_broken_not_declined() {
        // Declining would paste the whole text again on top of it.
        let field = FakeField::new("abc", range(3, 0));
        field.apply_delay_reads.set(0);
        let mut field = field;
        field.blind_after_set = true;
        assert!(matches!(live(&field, "Hello"), LiveStart::Broken(_)));
    }

    #[test]
    fn a_selection_is_replaced_by_the_first_live_text() {
        let field = FakeField::new("Hello world", range(6, 5));
        let owned = started(live(&field, "there"));
        assert_eq!(owned.start, 6);
        finish_live(&field, &owned, "there, friend", true, |_| {}).unwrap();
        assert_eq!(field.contents(), "Hello there, friend");
    }

    #[test]
    fn final_equal_to_the_live_text_changes_nothing() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "Done"));
        let calls = field.set_calls.get();
        finish_live(&field, &owned, "Done", true, |_| {}).unwrap();
        assert_eq!(field.set_calls.get(), calls);
    }

    #[test]
    fn an_app_that_ignored_a_write_is_skipped_after() {
        let mut ignored = IgnoredWrites::new();
        let first = ignored.attempt(7, || Outcome::UseClipboard(FallbackReason::NotInserted));
        assert!(matches!(first, Attempt::Declined(_)));
        let second = ignored.attempt(7, || panic!("should not write again"));
        assert!(matches!(second, Attempt::Declined(_)));
    }

    #[test]
    fn other_declines_and_other_apps_are_still_tried() {
        let mut ignored = IgnoredWrites::new();
        ignored.attempt(7, || {
            Outcome::UseClipboard(FallbackReason::NoFocusedElement)
        });
        ignored.attempt(8, || Outcome::UseClipboard(FallbackReason::NotInserted));
        let again = ignored.attempt(7, || Outcome::Inserted { exact: true });
        assert_eq!(again, Attempt::Inserted { verified: true });
        let other = ignored.attempt(9, || Outcome::Inserted { exact: true });
        assert_eq!(other, Attempt::Inserted { verified: true });
    }

    #[test]
    fn uncertain_writes_are_not_remembered() {
        let mut ignored = IgnoredWrites::new();
        ignored.attempt(7, || Outcome::Uncertain("?".into()));
        let again = ignored.attempt(7, || Outcome::Inserted { exact: true });
        assert_eq!(again, Attempt::Inserted { verified: true });
    }

    // ---- voice edits ----

    const TAKE: &str = "Hi Megan, see you Tuesday.";

    /// A field where a take was just written after "Note: ", caret after it.
    fn after_a_take() -> (FakeField, Owned) {
        let text = format!("Note: {TAKE}");
        let field = FakeField::new(&text, range(utf16_len(&text), 0));
        let owned = owned_before_caret(&field, Some("com.apple.TextEdit"), TAKE).unwrap();
        (field, owned)
    }

    fn edit(
        field: &FakeField,
        owned: &Owned,
        before: &str,
        after: &str,
    ) -> Result<Owned, EditError> {
        edit_owned(field, owned, before, after, true, &|_| false, |_| {})
    }

    #[test]
    fn a_take_just_written_is_owned_where_it_reads_back() {
        let (_, owned) = after_a_take();
        assert_eq!((owned.start, owned.text.as_str()), (6, TAKE));
        // A selection, other text, a terminal: not a take Kass can find.
        let field = FakeField::new("Hi Megan", range(0, 2));
        assert_eq!(owned_before_caret(&field, None, "Hi Megan"), None);
        let field = FakeField::new("Hi Meg", range(6, 0));
        assert_eq!(owned_before_caret(&field, None, "Hi Megan"), None);
        let field = FakeField::new("ls -la", range(6, 0));
        assert_eq!(
            owned_before_caret(&field, Some("com.apple.Terminal"), "ls -la"),
            None
        );
    }

    #[test]
    fn any_text_before_the_caret_can_be_edited() {
        let text = "I typed this. Hi Megan";
        let field = FakeField::new(text, range(utf16_len(text), 0));
        let owned = owned_near_caret(&field, Some("com.apple.MobileSMS"), 1000).unwrap();
        assert_eq!((owned.start, owned.text.as_str()), (0, text));
        let after = edit(&field, &owned, text, "I typed this. Hi Morgan").unwrap();
        assert_eq!(field.contents(), "I typed this. Hi Morgan");
        assert_eq!(after.end(), utf16_len("I typed this. Hi Morgan"));
    }

    #[test]
    fn text_before_the_caret_starts_at_a_whole_word() {
        let text = "one two three four";
        let field = FakeField::new(text, range(utf16_len(text), 0));
        let owned = owned_near_caret(&field, None, 12).unwrap();
        // 12 units start inside "two", so it is left out.
        assert_eq!((owned.start, owned.text.as_str()), (8, "three four"));
        assert_eq!(owned.end(), utf16_len(text));
    }

    #[test]
    fn nothing_to_edit_without_text_before_a_bare_caret() {
        let field = FakeField::new("Hi Megan", range(0, 2));
        assert_eq!(owned_near_caret(&field, None, 1000), None);
        let field = FakeField::new("  ", range(2, 0));
        assert_eq!(owned_near_caret(&field, None, 1000), None);
        let field = FakeField::new("ls -la", range(6, 0));
        assert_eq!(
            owned_near_caret(&field, Some("com.apple.Terminal"), 1000),
            None
        );
    }

    #[test]
    fn an_edit_changes_only_the_word_and_leaves_the_caret_after_the_take() {
        let (field, owned) = after_a_take();
        let fixed = TAKE.replace("Megan", "Morgan");
        let owned = edit(&field, &owned, TAKE, &fixed).unwrap();
        assert_eq!(field.contents(), format!("Note: {fixed}"));
        assert_eq!(*field.written.borrow(), vec!["Morgan".to_string()]);
        assert_eq!(field.sel.get(), range(utf16_len(&field.contents()), 0));
        // The take is still owned as it now reads: a second edit works on it.
        assert_eq!(owned.text, fixed);
        let again = fixed.replace("Tuesday", "Thursday");
        edit(&field, &owned, &fixed, &again).unwrap();
        assert_eq!(field.contents(), format!("Note: {again}"));
    }

    #[test]
    fn an_edit_may_see_only_the_end_of_a_long_take() {
        let (field, owned) = after_a_take();
        edit(&field, &owned, "see you Tuesday.", "see you Thursday.").unwrap();
        assert_eq!(field.contents(), "Note: Hi Megan, see you Thursday.");
    }

    #[test]
    fn a_deleted_word_goes_with_its_space() {
        let (field, owned) = after_a_take();
        edit(&field, &owned, TAKE, "Hi Megan, see Tuesday.").unwrap();
        assert_eq!(field.contents(), "Note: Hi Megan, see Tuesday.");
        assert_eq!(field.sel.get(), range(28, 0));
    }

    // ---- corrections saved in Kass ----

    fn correct(
        field: &FakeField,
        owned: &Owned,
        before: &str,
        after: &str,
    ) -> Result<Owned, EditError> {
        correct_owned(field, owned, before, after, true, None, |_| {})
    }

    #[test]
    fn a_saved_correction_changes_the_take_where_kass_left_it() {
        let (field, owned) = after_a_take();
        // The capture's text starts differently from the field's: only
        // where they differ is matched.
        let owned = correct(
            &field,
            &owned,
            "hi Megan, see you Tuesday.",
            "hi Morgan, see you Tuesday.",
        )
        .unwrap();
        assert_eq!(field.contents(), "Note: Hi Morgan, see you Tuesday.");
        assert_eq!(*field.written.borrow(), vec!["Morgan".to_string()]);
        assert_eq!(owned.text, "Hi Morgan, see you Tuesday.");
    }

    #[test]
    fn a_correction_leaves_a_field_the_user_changed() {
        let (field, owned) = after_a_take();
        field.user_types(utf16_len(&field.contents()), " Bye");
        let fixed = TAKE.replace("Megan", "Morgan");
        assert_eq!(
            correct(&field, &owned, TAKE, &fixed),
            Err(EditError::Changed)
        );
        assert_eq!(field.contents(), format!("Note: {TAKE} Bye"));
        assert!(field.written.borrow().is_empty());
    }

    #[test]
    fn a_correction_is_never_typed() {
        let (mut field, owned) = after_a_take();
        field.settable = false;
        let fixed = TAKE.replace("Megan", "Morgan");
        assert_eq!(
            correct(&field, &owned, TAKE, &fixed),
            Err(EditError::Unsupported)
        );
        assert_eq!(field.contents(), format!("Note: {TAKE}"));
    }

    #[test]
    fn a_correction_for_other_text_changes_nothing() {
        let (field, owned) = after_a_take();
        assert_eq!(
            correct(&field, &owned, "Bye Megan.", "Bye Morgan."),
            Err(EditError::Changed)
        );
        assert_eq!(correct(&field, &owned, TAKE, TAKE), Err(EditError::Changed));
        assert!(field.written.borrow().is_empty());
    }

    /// [`correct_owned`] typing the new words, as in an Electron app that
    /// ignores Accessibility writes. Returns what was typed.
    fn correct_typed(
        field: &FakeField,
        owned: &Owned,
        after: &str,
    ) -> (Result<Owned, EditError>, Vec<String>) {
        let typed = RefCell::new(Vec::new());
        let type_in = |text: &str| {
            typed.borrow_mut().push(text.to_string());
            field.apply(text);
            true
        };
        let result = correct_owned(field, owned, TAKE, after, true, Some(&type_in), |_| {});
        (result, typed.into_inner())
    }

    #[test]
    fn a_correction_in_front_is_typed_even_where_accessibility_says_it_writes() {
        let (mut field, owned) = after_a_take();
        // Electron: AXSelectedText is settable, and a write is ignored.
        field.applies = false;
        let (result, typed) = correct_typed(&field, &owned, &TAKE.replace("Megan", "Morgan"));
        assert_eq!(result.unwrap().text, TAKE.replace("Megan", "Morgan"));
        assert_eq!(typed, ["Morgan"]);
        assert_eq!(field.set_calls.get(), 0);
        assert_eq!(field.contents(), "Note: Hi Morgan, see you Tuesday.");
    }

    #[test]
    fn a_correction_waits_for_a_selection_that_moves_late() {
        let (mut field, owned) = after_a_take();
        field.applies = false;
        field.selection_lag = 3;
        let fixed = TAKE.replace("Hi Megan", "Hi, Megan");
        let (result, typed) = correct_typed(&field, &owned, &fixed);
        assert_eq!(result.unwrap().text, fixed);
        assert_eq!(typed, [","]);
        assert_eq!(field.contents(), format!("Note: {fixed}"));
    }

    #[test]
    fn nothing_is_typed_where_the_selection_did_not_move() {
        let (mut field, owned) = after_a_take();
        field.ignores_selection = true;
        let (result, typed) = correct_typed(&field, &owned, &TAKE.replace("Megan", "Morgan"));
        assert_eq!(result, Err(EditError::NotApplied));
        assert!(typed.is_empty());
        assert_eq!(field.contents(), format!("Note: {TAKE}"));
    }

    #[test]
    fn a_take_the_user_changed_since_is_never_edited() {
        let (field, owned) = after_a_take();
        field.user_types(32, " Bye.");
        assert_eq!(
            edit(&field, &owned, TAKE, &TAKE.replace("Megan", "Morgan")),
            Err(EditError::Changed)
        );
        assert_eq!(field.contents(), format!("Note: {TAKE} Bye."));
        // Or moved the caret, or the edit was planned on other text.
        let (field, owned) = after_a_take();
        field.sel.set(range(0, 0));
        assert_eq!(edit(&field, &owned, TAKE, "x"), Err(EditError::Changed));
        let (field, owned) = after_a_take();
        assert_eq!(
            edit(&field, &owned, "Hello.", "Bye."),
            Err(EditError::Changed)
        );
        assert_eq!(field.set_calls.get(), 0);
    }

    #[test]
    fn a_write_the_app_ignores_changes_nothing() {
        let (mut field, owned) = after_a_take();
        // Safari's web fields: the set succeeds, the text never changes.
        field.applies = false;
        assert_eq!(
            edit(&field, &owned, TAKE, &TAKE.replace("Megan", "Morgan")),
            Err(EditError::NotApplied)
        );
        assert_eq!(field.contents(), format!("Note: {TAKE}"));
        assert_eq!(field.sel.get(), range(utf16_len(&field.contents()), 0));
    }

    #[test]
    fn a_write_that_lands_differently_is_reported_not_trusted() {
        let (mut field, owned) = after_a_take();
        field.blind_after_set = true;
        assert!(matches!(
            edit(&field, &owned, TAKE, &TAKE.replace("Megan", "Morgan")),
            Err(EditError::Uncertain(_))
        ));
    }

    /// [`edit`] in a field like Messages', whose `AXSelectedText` can't be
    /// set: the new words are typed over the selection instead, landing
    /// unless `lands` is false. Returns what was typed.
    fn edit_typed(
        field: &FakeField,
        owned: &Owned,
        after: &str,
        lands: bool,
    ) -> (Result<Owned, EditError>, Vec<String>) {
        let typed = RefCell::new(Vec::new());
        let type_in = |text: &str| {
            typed.borrow_mut().push(text.to_string());
            if lands {
                field.apply(text);
            }
            true
        };
        let result = edit_owned(field, owned, TAKE, after, true, &type_in, |_| {});
        (result, typed.into_inner())
    }

    #[test]
    fn a_fix_is_typed_where_accessibility_cannot_write_it() {
        let (mut field, owned) = after_a_take();
        field.settable = false;
        let (result, typed) = edit_typed(&field, &owned, &TAKE.replace("Megan", "Morgan"), true);
        assert_eq!(result.unwrap().text, TAKE.replace("Megan", "Morgan"));
        assert_eq!(typed, ["Morgan"]);
        assert_eq!(field.set_calls.get(), 0);
        assert_eq!(field.contents(), "Note: Hi Morgan, see you Tuesday.");
        assert_eq!(field.sel.get(), range(utf16_len(&field.contents()), 0));
    }

    #[test]
    fn a_removal_retypes_the_character_before_it() {
        let (mut field, owned) = after_a_take();
        field.settable = false;
        let (result, typed) = edit_typed(&field, &owned, "Hi Megan, see you.", true);
        assert_eq!(result.unwrap().text, "Hi Megan, see you.");
        assert_eq!(typed, ["u"]);
        assert_eq!(field.contents(), "Note: Hi Megan, see you.");
    }

    #[test]
    fn typing_that_never_lands_changes_nothing() {
        let (mut field, owned) = after_a_take();
        field.settable = false;
        let (result, _) = edit_typed(&field, &owned, &TAKE.replace("Megan", "Morgan"), false);
        assert_eq!(result, Err(EditError::NotApplied));
        assert_eq!(field.contents(), format!("Note: {TAKE}"));
        assert_eq!(field.sel.get(), range(utf16_len(&field.contents()), 0));
    }

    #[test]
    fn apps_that_write_away_from_the_caret_are_never_edited() {
        let (field, owned) = after_a_take();
        assert_eq!(
            edit_owned(
                &field,
                &owned,
                TAKE,
                "Hi Morgan.",
                false,
                &|_| false,
                |_| {}
            ),
            Err(EditError::Unsupported)
        );
        assert_eq!(field.set_calls.get(), 0);
    }

    #[test]
    fn finished_live_text_is_owned_as_it_ends() {
        let field = FakeField::new("", range(0, 0));
        let owned = started(live(&field, "Hi Megan"));
        let owned = finish_live(&field, &owned, "Hi Megan, see you.", true, |_| {}).unwrap();
        assert_eq!(owned.text, "Hi Megan, see you.");
        edit(&field, &owned, &owned.text, "Hi Morgan, see you.").unwrap();
        assert_eq!(field.contents(), "Hi Morgan, see you.");
    }
}
