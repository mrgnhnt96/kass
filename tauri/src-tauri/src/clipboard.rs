//! Snapshot / write / restore helpers around the system clipboard.
//!
//! Used by the auto-paste flow: before synthesising the paste accelerator
//! into a foreign app we need to (1) remember what the user had on the
//! clipboard, (2) stage our transcribed text, (3) paste, (4) put the
//! original contents back. Missing step 4 turns every dictation into a
//! silent clipboard-stomp.
//!
//! The snapshot walks `NSPasteboard.pasteboardItems` and
//! copies every `(UTI, data)` pair into an owned `Vec<u8>`, so restore
//! rebuilds the full multi-type payload — not just the plain-text
//! fallback. Images, styled text, file-reference lists all survive the
//! round-trip.
//!
//! Every entry point manages its own `NSAutoreleasePool` because the
//! Tauri command runtime threads don't have one by default — without it,
//! every autoreleased `NSString` / `NSData` we touch would leak for the
//! life of the process.
//!
//! [`copy_selection`] runs the same save/restore around a synthetic ⌘C, to
//! read a selection that Accessibility can't (Command Mode).

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::insert_chain::{Attempt, Inserter, Method, Request};

/// One full-fidelity snapshot of the general pasteboard. Hold on to the value
/// until the paste has landed, then pass it to [`restore_clipboard`].
#[derive(Debug, Clone)]
pub struct ClipboardSnapshot {
    /// Outer vec: pasteboard items. Inner: `(uti, raw bytes)` per type. We
    /// store the raw UTI string and the raw `NSData` payload so we can rebuild
    /// the item with `setData:forType:` without interpreting the contents.
    items: Vec<Vec<(String, Vec<u8>)>>,
    /// `NSPasteboard.changeCount` at the moment of capture. Incremented by AppKit
    /// on every mutation from any process, so a caller can decide whether a
    /// restore is still safe (change_count == expected) or whether someone
    /// else wrote to the clipboard in the interim and we should back off.
    change_count: i64,
}

type Id = *mut Object;

/// RAII wrapper so the pool drains even on early return / `?` propagation.
struct AutoreleasePool {
    pool: Id,
}

impl AutoreleasePool {
    unsafe fn new() -> Self {
        let pool: Id = msg_send![class!(NSAutoreleasePool), alloc];
        let pool: Id = msg_send![pool, init];
        Self { pool }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.pool, drain];
        }
    }
}

/// Build an autoreleased `NSString` from a Rust `&str` without scanning for
/// interior nulls (which is what `initWithUTF8String:` would require).
unsafe fn ns_string(s: &str) -> Id {
    // NSUTF8StringEncoding = 4.
    let obj: Id = msg_send![class!(NSString), alloc];
    let obj: Id = msg_send![
        obj,
        initWithBytes: s.as_ptr()
        length: s.len()
        encoding: 4u64
    ];
    let _: () = msg_send![obj, autorelease];
    obj
}

unsafe fn ns_string_to_rust(s: Id) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let bytes: *const i8 = msg_send![s, UTF8String];
    if bytes.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(bytes)
        .to_str()
        .ok()
        .map(|x| x.to_owned())
}

unsafe fn general_pasteboard() -> Result<Id, String> {
    let pb: Id = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return Err("NSPasteboard generalPasteboard returned nil".into());
    }
    Ok(pb)
}

/// Read the pasteboard's current change count without snapshotting contents.
///
/// AppKit increments this every time any process writes to the general
/// pasteboard, so it's a cheap way to detect "did someone clobber my staged
/// text before the paste landed?".
/// A snapshot taken earlier (at dictation key-down) that still matches the
/// clipboard. Reading the clipboard makes the app that copied it render every
/// format, which can take seconds; taking it while the user speaks keeps that
/// off the wait after release. Anything copied since makes it stale.
pub fn reusable(
    prepared: Option<ClipboardSnapshot>,
    current: Option<i64>,
) -> Option<ClipboardSnapshot> {
    prepared.filter(|snapshot| Some(snapshot.change_count) == current)
}

pub fn current_change_count() -> Result<i64, String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let pb = general_pasteboard()?;
        let c: i64 = msg_send![pb, changeCount];
        Ok(c)
    }
}

/// The clipboard's text, when text is what was copied: `None` for images and
/// files, whose plain-text form is only a name. Reads just the text, so an
/// app that renders its clipboard lazily isn't asked for every format.
pub fn read_text() -> Result<Option<String>, String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let pb = general_pasteboard()?;
        let not_text: Id = msg_send![class!(NSMutableArray), array];
        for uti in ["public.image", "public.file-url"] {
            let _: () = msg_send![not_text, addObject: ns_string(uti)];
        }
        let has_other: bool = msg_send![pb, canReadItemWithDataConformingToTypes: not_text];
        if has_other {
            return Ok(None);
        }
        let text: Id = msg_send![pb, stringForType: ns_string("public.utf8-plain-text")];
        Ok(ns_string_to_rust(text).filter(|t| !t.trim().is_empty()))
    }
}

/// Capture every item on the general pasteboard into an owned snapshot.
pub fn save_clipboard() -> Result<ClipboardSnapshot, String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let pb = general_pasteboard()?;
        let change_count: i64 = msg_send![pb, changeCount];

        let items: Id = msg_send![pb, pasteboardItems];
        if items.is_null() {
            return Ok(ClipboardSnapshot {
                items: Vec::new(),
                change_count,
            });
        }

        let count: usize = msg_send![items, count];
        let mut saved: Vec<Vec<(String, Vec<u8>)>> = Vec::with_capacity(count);

        for i in 0..count {
            let item: Id = msg_send![items, objectAtIndex: i];
            if item.is_null() {
                continue;
            }
            let types: Id = msg_send![item, types];
            if types.is_null() {
                continue;
            }
            let type_count: usize = msg_send![types, count];
            let mut pairs: Vec<(String, Vec<u8>)> = Vec::with_capacity(type_count);
            for j in 0..type_count {
                let t: Id = msg_send![types, objectAtIndex: j];
                let Some(type_str) = ns_string_to_rust(t) else {
                    continue;
                };
                let data: Id = msg_send![item, dataForType: t];
                if data.is_null() {
                    // Type advertised but no concrete data (lazy provider).
                    // Skipping is safer than trying to force it to materialise.
                    continue;
                }
                let length: usize = msg_send![data, length];
                let bytes_ptr: *const u8 = msg_send![data, bytes];
                let bytes = if bytes_ptr.is_null() || length == 0 {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(bytes_ptr, length).to_vec()
                };
                pairs.push((type_str, bytes));
            }
            saved.push(pairs);
        }

        Ok(ClipboardSnapshot {
            items: saved,
            change_count,
        })
    }
}

const TRANSIENT_MARKERS: [&str; 2] = [
    "org.nspasteboard.TransientType",
    "org.nspasteboard.AutoGeneratedType",
];

/// Replace the pasteboard contents with a single plain-text string. Returns
/// the post-write change count so a later restore can verify nothing else
/// touched the clipboard in between.
pub fn write_text(text: &str) -> Result<i64, String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let pb = general_pasteboard()?;
        let _new_count: i64 = msg_send![pb, clearContents];

        let ns_text = ns_string(text);
        // `public.utf8-plain-text` is the raw UTI behind `NSPasteboardTypeString`
        // and works for every text-aware paste target we care about.
        let ns_type = ns_string("public.utf8-plain-text");
        let ok: bool = msg_send![pb, setString: ns_text forType: ns_type];
        if !ok {
            return Err("NSPasteboard setString:forType: returned NO".into());
        }
        // Markers from nspasteboard.org: clipboard-history apps (Raycast,
        // Paste, Maccy) skip entries carrying them, so dictations staged for
        // a paste don't end up in the user's history.
        for marker in TRANSIENT_MARKERS {
            let _: bool = msg_send![pb, setString: ns_string("") forType: ns_string(marker)];
        }

        let after: i64 = msg_send![pb, changeCount];
        Ok(after)
    }
}

/// Rebuild the pasteboard from a snapshot, replacing whatever is on it now.
///
/// Does not consult the change count — callers that want safe restore should
/// compare [`current_change_count`] against the value returned by
/// [`write_text`] first.
pub fn restore_clipboard(snapshot: &ClipboardSnapshot) -> Result<(), String> {
    unsafe {
        let _pool = AutoreleasePool::new();
        let pb = general_pasteboard()?;
        let _: i64 = msg_send![pb, clearContents];

        if snapshot.items.is_empty() {
            return Ok(());
        }

        let array: Id = msg_send![class!(NSMutableArray), array];

        for pairs in &snapshot.items {
            let item: Id = msg_send![class!(NSPasteboardItem), alloc];
            let item: Id = msg_send![item, init];
            let _: () = msg_send![item, autorelease];

            for (uti, bytes) in pairs {
                let ns_type = ns_string(uti);
                let data: Id = msg_send![
                    class!(NSData),
                    dataWithBytes: bytes.as_ptr()
                    length: bytes.len()
                ];
                let _ok: bool = msg_send![item, setData: data forType: ns_type];
            }

            let _: () = msg_send![array, addObject: item];
        }

        let ok: bool = msg_send![pb, writeObjects: array];
        if !ok {
            return Err("NSPasteboard writeObjects: returned NO".into());
        }
        Ok(())
    }
}

// ========================================================================
// Clipboard + ⌘V: the last step of the insertion chain
// ========================================================================

/// How long the staged text stays on the clipboard after ⌘V before the
/// user's clipboard is put back. Too short and slow apps haven't read the
/// paste yet. The wait runs in the background: the paste is already sent,
/// so nothing about delivering the text waits on it.
pub const PASTE_CONSUME: Duration = Duration::from_millis(400);

/// A restore still waiting to run: the user's own clipboard, and the change
/// count right after our text was staged over it.
struct PendingRestore {
    original: ClipboardSnapshot,
    staged_count: i64,
    /// When the ⌘V was sent, for [`restore_pending`] to wait out [`PASTE_CONSUME`].
    pasted_at: Instant,
}

static PENDING: Mutex<Option<PendingRestore>> = Mutex::new(None);

/// Which of the user's clipboards a new paste should put back afterwards.
/// A restore still pending with our text on the clipboard wins: the
/// clipboard holds our previous dictation, not the user's content. `None`
/// means the clipboard must be read now.
fn original_to_restore(
    pending: Option<PendingRestore>,
    prepared: Option<ClipboardSnapshot>,
    current: Option<i64>,
) -> Option<ClipboardSnapshot> {
    match pending {
        Some(p) if Some(p.staged_count) == current => Some(p.original),
        _ => reusable(prepared, current),
    }
}

/// A background restore runs only if it is still the latest one and nobody
/// has written to the clipboard since our text was staged.
fn should_restore(latest_staged: Option<i64>, ours: i64, current: Option<i64>) -> bool {
    latest_staged == Some(ours) && current == Some(ours)
}

/// Put the user's clipboard back as soon as the last paste has been read, if
/// its restore is still waiting, so a ⌘V sent next pastes their content, not
/// dictated text. Blocking.
pub fn restore_pending() {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = pending.take() else {
        return;
    };
    std::thread::sleep(PASTE_CONSUME.saturating_sub(p.pasted_at.elapsed()));
    if current_change_count().ok() == Some(p.staged_count) {
        if let Err(e) = restore_clipboard(&p.original) {
            eprintln!("[kass] clipboard restore failed: {e}");
        }
    }
}

/// Stage `text`, send ⌘V to the frontmost app, and restore the user's
/// clipboard in the background. Needs the target in front (wrap it in
/// [`crate::insert_chain::InFront`]).
pub struct Paste {
    /// Snapshot taken at dictation key-down, reused if still current.
    prepared: Mutex<Option<ClipboardSnapshot>>,
}

impl Paste {
    pub fn new(prepared: Option<ClipboardSnapshot>) -> Self {
        Self {
            prepared: Mutex::new(prepared),
        }
    }
}

impl Inserter for Paste {
    fn method(&self) -> Method {
        Method::Clipboard
    }

    fn attempt(&self, req: &Request) -> Attempt {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let prepared = self.prepared.lock().ok().and_then(|mut p| p.take());
        let original =
            match original_to_restore(pending.take(), prepared, current_change_count().ok()) {
                Some(snapshot) => snapshot,
                None => match save_clipboard() {
                    Ok(snapshot) => snapshot,
                    Err(e) => return Attempt::Declined(format!("clipboard unreadable: {e}")),
                },
            };
        let staged_count = match write_text(req.text) {
            Ok(count) => count,
            Err(e) => {
                let _ = restore_clipboard(&original);
                return Attempt::Declined(format!("clipboard write failed: {e}"));
            }
        };
        if let Err(e) = crate::synthetic_keys::send_paste() {
            // No event was posted: nothing can have been pasted.
            let _ = restore_clipboard(&original);
            return Attempt::Declined(format!("⌘V not sent: {e}"));
        }
        *pending = Some(PendingRestore {
            original,
            staged_count,
            pasted_at: Instant::now(),
        });
        drop(pending);

        std::thread::spawn(move || {
            std::thread::sleep(PASTE_CONSUME);
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            let latest = pending.as_ref().map(|p| p.staged_count);
            if should_restore(latest, staged_count, current_change_count().ok()) {
                if let Some(p) = pending.take() {
                    if let Err(e) = restore_clipboard(&p.original) {
                        eprintln!("[kass] clipboard restore failed: {e}");
                    }
                }
            } else if latest == Some(staged_count) {
                // Someone copied during the window: keep their content.
                pending.take();
                eprintln!("[kass] clipboard changed during paste — not restoring");
            }
        });
        Attempt::Inserted { verified: false }
    }
}

// ========================================================================
// ⌘C: a selection Accessibility can't read (docs/plans/COMMAND_MODE.md)
// ========================================================================

/// How long the frontmost app gets to put its selection on the clipboard
/// after ⌘C. Apps answer in a few milliseconds; with nothing selected most
/// leave the clipboard alone, so the whole wait means "no selection".
pub const COPY_WAIT: Duration = Duration::from_millis(250);
const COPY_POLL: Duration = Duration::from_millis(10);

/// Copy the frontmost app's selection with ⌘C and put the user's clipboard
/// back. `Ok(None)` when nothing was copied. Blocking, for up to
/// [`COPY_WAIT`].
pub fn copy_selection() -> Result<Option<String>, String> {
    // Serialized with pastes: a paste's restore still pending means the
    // clipboard holds our text, and the user's own is the one it would restore.
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let before = current_change_count()?;
    let from_pending = pending.as_ref().is_some_and(|p| p.staged_count == before);
    let original = match original_to_restore(pending.take(), None, Some(before)) {
        Some(snapshot) => snapshot,
        None => save_clipboard()?,
    };
    crate::synthetic_keys::send_copy()?;
    let started = Instant::now();
    let mut copied = None;
    while started.elapsed() < COPY_WAIT {
        std::thread::sleep(COPY_POLL);
        // An app may clear the clipboard before writing to it: wait for text.
        if current_change_count()? != before {
            copied = read_text().ok().flatten();
            if copied.is_some() {
                break;
            }
        }
    }
    if copied.is_some() || from_pending || current_change_count()? != before {
        restore_clipboard(&original)?;
    }
    Ok(copied.filter(|text| !text.is_empty()))
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    fn snap(tag: &[u8], change_count: i64) -> ClipboardSnapshot {
        ClipboardSnapshot {
            items: vec![vec![("public.utf8-plain-text".to_string(), tag.to_vec())]],
            change_count,
        }
    }

    fn tag(s: &ClipboardSnapshot) -> &[u8] {
        &s.items[0][0].1
    }

    #[test]
    fn back_to_back_paste_restores_the_users_clipboard_not_the_last_dictation() {
        // First paste staged at count 11 over the user's clipboard; its
        // restore hasn't run. The second paste must restore the user's.
        let pending = PendingRestore {
            original: snap(b"user", 10),
            staged_count: 11,
            pasted_at: Instant::now(),
        };
        let key_down = snap(b"first dictation", 11);
        let got = original_to_restore(Some(pending), Some(key_down), Some(11)).unwrap();
        assert_eq!(tag(&got), b"user");
    }

    #[test]
    fn a_copy_after_the_last_paste_beats_the_pending_restore() {
        let pending = PendingRestore {
            original: snap(b"user", 10),
            staged_count: 11,
            pasted_at: Instant::now(),
        };
        let key_down = snap(b"new copy", 12);
        let got = original_to_restore(Some(pending), Some(key_down), Some(12)).unwrap();
        assert_eq!(tag(&got), b"new copy");
    }

    #[test]
    fn with_nothing_pending_the_key_down_snapshot_is_reused_if_current() {
        let got = original_to_restore(None, Some(snap(b"user", 5)), Some(5)).unwrap();
        assert_eq!(tag(&got), b"user");
        assert!(original_to_restore(None, Some(snap(b"user", 5)), Some(6)).is_none());
        assert!(original_to_restore(None, None, Some(5)).is_none());
    }

    #[test]
    fn restore_runs_only_for_the_latest_paste_with_an_untouched_clipboard() {
        assert!(should_restore(Some(11), 11, Some(11)));
        // The user copied something in the window.
        assert!(!should_restore(Some(11), 11, Some(12)));
        // A later paste took over the restore.
        assert!(!should_restore(Some(13), 11, Some(13)));
        // The later paste's restore already ran.
        assert!(!should_restore(None, 11, Some(14)));
        assert!(!should_restore(Some(11), 11, None));
    }
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    fn snapshot(change_count: i64) -> ClipboardSnapshot {
        ClipboardSnapshot {
            items: vec![vec![(
                "public.utf8-plain-text".to_string(),
                b"copied".to_vec(),
            )]],
            change_count,
        }
    }

    #[test]
    fn a_snapshot_from_key_down_is_reused_when_nothing_was_copied_since() {
        let reused = reusable(Some(snapshot(7)), Some(7)).expect("reused");
        assert_eq!(reused.change_count, 7);
    }

    #[test]
    fn copying_during_dictation_makes_the_snapshot_stale() {
        assert!(reusable(Some(snapshot(7)), Some(8)).is_none());
    }

    #[test]
    fn no_snapshot_or_unknown_clipboard_means_saving_again() {
        assert!(reusable(None, Some(7)).is_none());
        assert!(reusable(Some(snapshot(7)), None).is_none());
    }
}
