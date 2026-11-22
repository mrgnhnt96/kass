//! What a voice edit (docs/plans/VOICE_EDITS.md) may change: the text
//! before the caret when the take starts, whoever wrote it ("fix that,
//! Morgan not Megan"), and Kass's own last take, which says how much of
//! that text Kass wrote, so only a fix there is learned from.
//!
//! A take is remembered only where it read back: where Accessibility wrote
//! it (the live path, or the insertion chain's Accessibility step), or, for
//! a correction saved in Captures (docs/plans/CORRECTIONS_IN_PLACE.md),
//! where keys or ⌘V put it in an app that ignores Accessibility writes. The next
//! take that pastes anything replaces it; an edit updates it. Whether the
//! text is still as it was read (the user hasn't typed, moved the caret, or
//! left the field) is checked when an edit is applied.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use crate::text_insert::{utf16_len, EditError, Owned};

pub const NOTHING_TO_FIX: &str = "Nothing to fix: Kass can't read the text before the cursor here";
const CHANGED: &str = "The text changed while you spoke, so nothing was fixed";
const NOT_APPLIED: &str = "This app didn't take the fix, so nothing changed";
const UNSUPPORTED: &str = "Voice edits don't work in this app yet";

#[derive(Debug, Clone, PartialEq)]
pub struct LastTake {
    /// The app it went to.
    pub pid: i32,
    /// The capture that wrote it.
    pub capture_id: Option<String>,
    pub owned: Owned,
    /// Put in by keys or ⌘V: the app ignores Accessibility writes, so a
    /// correction is typed over it, once the app is in front again.
    pub typed: bool,
}

/// The text before the caret in `pid`'s field when a take started.
#[derive(Debug, Clone, PartialEq)]
pub struct Editable {
    pub pid: i32,
    pub owned: Owned,
    /// Kass's last take, where the text still ends with it as Kass left it.
    pub last: Option<LastTake>,
}

impl Editable {
    pub fn new(pid: i32, owned: Owned, last: Option<LastTake>) -> Self {
        let last = last.filter(|last| {
            last.pid == pid
                && last.owned.end() == owned.end()
                && (owned.text.ends_with(&last.owned.text)
                    || last.owned.text.ends_with(&owned.text))
        });
        Self { pid, owned, last }
    }

    /// How many of the text's last chars Kass's last take wrote.
    pub fn own_chars(&self) -> usize {
        self.last.as_ref().map_or(0, |last| {
            last.owned
                .text
                .chars()
                .count()
                .min(self.owned.text.chars().count())
        })
    }

    pub fn capture_id(&self) -> Option<String> {
        self.last.as_ref().and_then(|last| last.capture_id.clone())
    }
}

static LAST: Mutex<Option<LastTake>> = Mutex::new(None);
/// Counts every [`remember`] and [`forget`], so a take read back later is
/// kept only while no other take has replaced it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn current() -> Option<LastTake> {
    LAST.lock().ok()?.clone()
}

pub fn remember(take: LastTake) {
    if let Ok(mut last) = LAST.lock() {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        *last = Some(take);
    }
}

pub fn forget() {
    if let Ok(mut last) = LAST.lock() {
        GENERATION.fetch_add(1, Ordering::SeqCst);
        *last = None;
    }
}

/// Where the last take is now, for [`remember_if`].
pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// [`remember`], unless a take was remembered or forgotten since
/// `generation`. Returns whether it was kept.
pub fn remember_if(generation: u64, take: LastTake) -> bool {
    let Ok(mut last) = LAST.lock() else {
        return false;
    };
    if GENERATION.load(Ordering::SeqCst) != generation {
        return false;
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
    *last = Some(take);
    true
}

/// After an edit of `edited`: Kass's last take, where it ended the text,
/// now reads as `result` left it. Only while no other take has replaced it.
pub fn edited(edited: &Editable, result: &Result<Owned, EditError>) {
    let Some(take) = &edited.last else {
        return;
    };
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if last.as_ref() != Some(take) {
        return;
    }
    *last = match result {
        Ok(owned) => Some(LastTake {
            owned: after_edit(&edited.owned, &take.owned, owned),
            ..take.clone()
        }),
        // Not applied: the field is as it was, and a retry may work.
        Err(EditError::NotApplied) => Some(take.clone()),
        Err(_) => None,
    };
}

/// A correction saved in Captures, from `before` to `after`, for the field
/// `take` went to.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
    pub take: LastTake,
    pub before: String,
    pub after: String,
}

/// What became of a correction saved in Captures.
#[derive(Debug, Clone, PartialEq)]
pub enum Corrected {
    /// The field reads corrected, in the app with this pid.
    Applied(i32),
    /// Held for when its app is in front again ([`take_held`]).
    Held,
    /// Not Kass's last take, or its field changed: nothing was done.
    Nothing,
}

/// A typed take's correction, until its app is in front.
static HELD: Mutex<Option<Correction>> = Mutex::new(None);

/// A correction saved in Captures for `capture_id`, from `before` to
/// `after`, made in the field Kass's last take went to, through `correct`.
/// Only for the last take, and only while the field shows it as Kass left
/// it. A typed take's correction is held instead, for [`take_held`].
pub fn apply_correction(
    capture_id: &str,
    before: &str,
    after: &str,
    correct: impl FnOnce(i32, &Owned, &str, &str) -> Result<Owned, EditError>,
) -> Corrected {
    let Some(take) = current().filter(|take| take.capture_id.as_deref() == Some(capture_id)) else {
        return Corrected::Nothing;
    };
    if take.typed {
        return match hold(take, before, after) {
            true => Corrected::Held,
            false => Corrected::Nothing,
        };
    }
    let correction = Correction {
        take,
        before: before.into(),
        after: after.into(),
    };
    match apply_held(&correction, correct) {
        Some(pid) => Corrected::Applied(pid),
        None => Corrected::Nothing,
    }
}

/// Hold a correction of the typed `take`. A later correction of the same
/// text joins the held one, so the field goes from what it shows to the
/// newest fix in one edit. Returns whether one is held.
fn hold(take: LastTake, before: &str, after: &str) -> bool {
    let Ok(mut held) = HELD.lock() else {
        return false;
    };
    let before = match held.take() {
        Some(earlier) if earlier.take == take && earlier.after == before => earlier.before,
        _ => before.to_string(),
    };
    *held = (before != after).then(|| Correction {
        take,
        before,
        after: after.into(),
    });
    held.is_some()
}

/// The held correction, while its take is still Kass's last take; one whose
/// take was replaced is dropped. It stays held.
pub fn held() -> Option<Correction> {
    let mut held = HELD.lock().ok()?;
    if held
        .as_ref()
        .is_some_and(|c| current().as_ref() != Some(&c.take))
    {
        *held = None;
    }
    held.clone()
}

/// [`held`], no longer held.
pub fn take_held() -> Option<Correction> {
    let correction = held()?;
    drop_held();
    Some(correction)
}

pub fn drop_held() {
    if let Ok(mut held) = HELD.lock() {
        *held = None;
    }
}

/// Make `correction` through `correct`, where its take is still Kass's last
/// take. Returns the app's pid when the field now reads corrected.
pub fn apply_held(
    correction: &Correction,
    correct: impl FnOnce(i32, &Owned, &str, &str) -> Result<Owned, EditError>,
) -> Option<i32> {
    let take = &correction.take;
    if current().as_ref() != Some(take) {
        return None;
    }
    let editable = Editable::new(take.pid, take.owned.clone(), Some(take.clone()));
    let result = correct(take.pid, &take.owned, &correction.before, &correction.after);
    edited(&editable, &result);
    result.ok().map(|_| take.pid)
}

/// Kass's part `own` of the text `before`, once the edit made it `after`:
/// the same words, moved, when the edit was before them; else the text from
/// where they started.
fn after_edit(before: &Owned, own: &Owned, after: &Owned) -> Owned {
    let text = if after.text.ends_with(&own.text) {
        own.text.clone()
    } else {
        let from = (own.start - before.start).max(0);
        from_unit(&after.text, from).to_string()
    };
    Owned {
        start: after.end() - utf16_len(&text),
        text,
        join: own.join.clone(),
    }
}

/// `text` from its UTF-16 unit `unit` on.
fn from_unit(text: &str, unit: i64) -> &str {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        if units >= unit {
            return &text[at..];
        }
        units += c.len_utf16() as i64;
    }
    ""
}

/// What the pill says when an edit changed nothing.
pub fn message(error: &EditError) -> String {
    match error {
        EditError::Changed => CHANGED.into(),
        EditError::NotApplied => NOT_APPLIED.into(),
        EditError::Unsupported => UNSUPPORTED.into(),
        EditError::Uncertain(message) => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(start: i64, text: &str) -> Owned {
        Owned {
            start,
            text: text.into(),
            join: Default::default(),
        }
    }

    /// Tests that use the one last take run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn take(start: i64, text: &str) -> LastTake {
        LastTake {
            pid: 7,
            capture_id: Some("c1".into()),
            owned: owned(start, text),
            typed: false,
        }
    }

    #[test]
    fn kass_wrote_the_end_of_the_text_while_its_last_take_ends_it() {
        let text = owned(0, "I typed this. Hi Megan.");
        let editable = Editable::new(7, text.clone(), Some(take(14, "Hi Megan.")));
        assert_eq!(editable.own_chars(), 9);
        assert_eq!(editable.capture_id().as_deref(), Some("c1"));
    }

    #[test]
    fn text_kass_did_not_just_write_is_still_editable_but_not_its_own() {
        let text = owned(0, "I typed this. Hi Megan. More");
        // The user typed after the take, or it went to another app.
        for last in [
            take(14, "Hi Megan."),
            LastTake {
                pid: 8,
                ..take(23, " More")
            },
        ] {
            let editable = Editable::new(7, text.clone(), Some(last));
            assert_eq!(editable.own_chars(), 0);
            assert_eq!(editable.capture_id(), None);
        }
        assert_eq!(Editable::new(7, text, None).own_chars(), 0);
    }

    #[test]
    fn a_take_longer_than_the_text_read_owns_all_of_it() {
        let text = owned(10, "Hi Megan.");
        let editable = Editable::new(7, text, Some(take(0, "0123456789Hi Megan.")));
        assert_eq!(editable.own_chars(), 9);
    }

    #[test]
    fn a_fix_in_kass_s_part_keeps_it_kass_s() {
        let before = owned(0, "I typed this. Hi Megan.");
        let own = owned(14, "Hi Megan.");
        let after = owned(0, "I typed this. Hi Morgan.");
        assert_eq!(after_edit(&before, &own, &after), owned(14, "Hi Morgan."));
    }

    #[test]
    fn a_fix_before_kass_s_part_moves_it() {
        let before = owned(0, "I typed thsi. Hi Megan.");
        let own = owned(14, "Hi Megan.");
        let after = owned(0, "I typed this one. Hi Megan.");
        assert_eq!(after_edit(&before, &own, &after), owned(18, "Hi Megan."));
    }

    #[test]
    fn a_fix_remembers_the_take_as_it_now_reads() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget();
        let last = take(14, "Hi Megan.");
        remember(last.clone());
        let editable = Editable::new(7, owned(0, "I typed this. Hi Megan."), Some(last));
        edited(&editable, &Ok(owned(0, "I typed this. Hi Morgan.")));
        assert_eq!(current().map(|l| l.owned), Some(owned(14, "Hi Morgan.")));
        forget();
    }

    #[test]
    fn a_saved_correction_applies_only_to_the_last_take() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget();
        remember(take(14, "Hi Megan."));
        let fixed = |_: i32, own: &Owned, _: &str, _: &str| Ok(owned(own.start, "Hi Morgan."));
        let corrected = |id| apply_correction(id, "Hi Megan.", "Hi Morgan.", fixed);
        assert_eq!(corrected("c0"), Corrected::Nothing);
        assert_eq!(corrected("c1"), Corrected::Applied(7));
        // Remembered as it now reads, so the next correction finds it.
        assert_eq!(current().map(|l| l.owned), Some(owned(14, "Hi Morgan.")));
        let changed = |_: i32, _: &Owned, _: &str, _: &str| Err(EditError::Changed);
        assert_eq!(
            apply_correction("c1", "Hi Morgan.", "Hi Morgana.", changed),
            Corrected::Nothing
        );
        // A field that changed is no longer Kass's to correct.
        assert_eq!(current(), None);
        forget();
    }

    #[test]
    fn a_typed_take_s_corrections_wait_and_join() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        forget();
        drop_held();
        let typed = LastTake {
            typed: true,
            ..take(14, "Hi Megan, Tuesday.")
        };
        remember(typed.clone());
        let unused = |_: i32, _: &Owned, _: &str, _: &str| -> Result<Owned, EditError> {
            panic!("a typed take is corrected only once its app is in front")
        };
        let first = apply_correction("c1", "Hi Megan, Tuesday.", "Hi Morgan, Tuesday.", unused);
        assert_eq!(first, Corrected::Held);
        let second = apply_correction("c1", "Hi Morgan, Tuesday.", "Hi Morgan, Thursday.", unused);
        assert_eq!(second, Corrected::Held);
        // One edit, from what the field shows to the newest fix.
        let joined = held().unwrap();
        assert_eq!(
            (joined.before.as_str(), joined.after.as_str()),
            ("Hi Megan, Tuesday.", "Hi Morgan, Thursday.")
        );
        // A newer take drops it.
        let generation = generation();
        remember(take(0, "Something else."));
        assert_eq!(held(), None);
        // And a take read back late doesn't replace the newer one.
        assert!(!remember_if(generation, typed));
        forget();
    }
}
