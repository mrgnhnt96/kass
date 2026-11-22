//! Text insertion by typing: key events that carry the text itself.
//!
//! Each event is a synthetic key press whose characters are replaced with a
//! chunk of the text (`CGEventKeyboardSetUnicodeString`), so the target app
//! receives ordinary typed input: no clipboard, no paste menu item, and it
//! works in fields Accessibility cannot write to. The cost is that nothing
//! can be read back, so success is reported unverified, and that typed input
//! goes to whichever app is frontmost and passes through whatever that app
//! does per keystroke. The rules below therefore decline whenever the typed
//! result could differ from the text; the chain then tries the next method.
//!
//! Rules this module enforces:
//!
//! - Only type text that types as itself: no tabs or control characters
//!   (they move focus or are dropped), and no character that opens
//!   per-keystroke UI in chat apps and editors ([`TRIGGERS`]).
//! - Never type into terminals or remote/VM viewers, where keystrokes are
//!   forwarded to another system that may map them differently.
//! - Only type when the events can arrive intact: Secure Event Input off,
//!   the target frontmost, an ASCII-capable input source (an active IME
//!   would convert the text), and no modifier key held (the user may still
//!   be releasing the dictation chord; a held ⌘ would turn text into
//!   shortcuts).
//! - Newlines are typed as Shift+Return, which inserts a line break in chat
//!   apps where a plain Return sends the message.
//!
//! [`plan`] and the eligibility checks are pure and unit-tested. The OS
//! calls sit behind [`KeystrokeTarget`] so the orchestration ([`type_text`])
//! runs against fakes in tests.

use std::fmt;
use std::time::{Duration, Instant};

use crate::insert_chain::{Attempt, Inserter, Method, Request};

/// Most UTF-16 units one event carries. macOS silently truncates a longer
/// string (undocumented; observed limit).
pub const MAX_CHUNK_UTF16: usize = 20;

/// Keycode of the text events. The unicode string replaces what the key
/// would type, so the key is irrelevant to the text; 0 (`kVK_ANSI_A`) is the
/// conventional choice. Apps that read keycodes instead of characters
/// (terminals, VMs) are declined.
pub const TEXT_KEYCODE: u16 = 0x00;
/// `kVK_Return`.
pub const RETURN_KEYCODE: u16 = 0x24;
/// `kVK_Shift` (left).
pub const SHIFT_KEYCODE: u16 = 0x38;
/// `kCGEventFlagMaskShift`.
pub const FLAG_SHIFT: u64 = 0x0002_0000;

/// Modifier bits that block typing while held: Shift, Control, Option,
/// Command and Fn. Caps Lock (`0x10000`) is left out: the event's
/// characters are the unicode string, which Caps Lock does not change, and
/// the flags on every text event are set to 0 explicitly.
pub const HELD_MODIFIER_MASK: u64 =
    0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000 | 0x0080_0000;

/// Pause between keystrokes. Posting as fast as possible has been reported
/// to drop or reorder characters in some apps (Electron in particular) when
/// events outrun the app's event loop.
pub const KEYSTROKE_PACING: Duration = Duration::from_millis(2);

/// Most keystrokes worth typing. Measured in TextEdit (`insert_bench.rs`):
/// typed text lands in about 6 ms plus 2.7 ms per keystroke, and a paste in
/// about 18 ms whatever the length. Past 4 keystrokes (about 80 characters
/// on one line) the paste is faster, so typing declines and the chain moves
/// on.
pub const MAX_KEYSTROKES: usize = 4;

/// How long to wait for held modifiers to be released, and how often to
/// look. Past the timeout the method declines instead of typing.
pub const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_millis(150);
pub const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Remote desktop and VM viewers. They forward keycodes to another system
/// with its own layout, so the unicode text is lost or mistranslated.
const REMOTE_BUNDLES: &[&str] = &[
    // Microsoft Remote Desktop and its successor Windows App share this id.
    "com.microsoft.rdc.macos",
    // Microsoft Remote Desktop beta (unverified).
    "com.microsoft.rdc.osx.beta",
    "com.vmware.fusion",
    // The Parallels VM window process (unverified).
    "com.parallels.desktop.console",
    "com.utmapp.UTM",
    // Verified on this Mac.
    "com.apple.ScreenSharing",
    "com.realvnc.vncviewer",
    "com.teamviewer.TeamViewer",
    // Citrix Viewer (unverified).
    "com.citrix.receiver.icaviewer.mac",
    // VirtualBox VM window (unverified).
    "org.virtualbox.app.VirtualBoxVM",
];

/// Where a trigger character opens UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerPosition {
    /// Anywhere in the text.
    Anywhere,
    /// When the next character is not whitespace (a trigger followed by a
    /// space closes the popup again).
    BeforeNonSpace,
    /// As the first character of the text or of a line.
    LineStart,
}

/// Characters that open autocomplete while being typed: `@` mention pickers,
/// `:emoji` pickers, and `/` command menus at the start of a message. A
/// popup that is open when the next keystroke arrives can take it (Return,
/// or the text itself as a filter) and change what lands in the field, so
/// text containing one is left to methods that insert it all at once.
pub const TRIGGERS: &[(char, TriggerPosition)] = &[
    ('@', TriggerPosition::Anywhere),
    (':', TriggerPosition::BeforeNonSpace),
    ('/', TriggerPosition::LineStart),
];

/// One key press of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keystroke {
    /// Up to [`MAX_CHUNK_UTF16`] units of text, never starting with a line
    /// break or tab.
    Text(String),
    /// A line break, typed as Shift+Return.
    Newline,
}

/// One event to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    pub keycode: u16,
    pub down: bool,
    pub flags: u64,
    /// UTF-16 text the event carries instead of the key's own character.
    pub text: Option<Vec<u16>>,
}

impl Keystroke {
    /// The events that type this keystroke, in order.
    pub fn events(&self) -> Vec<KeyEvent> {
        match self {
            // The text goes on the key-up too, as on hardware where both
            // events carry the characters; apps insert on key-down only.
            Keystroke::Text(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                [true, false]
                    .into_iter()
                    .map(|down| KeyEvent {
                        keycode: TEXT_KEYCODE,
                        down,
                        flags: 0,
                        text: Some(units.clone()),
                    })
                    .collect()
            }
            // Shift is pressed as its own event with the Shift flag set, like
            // ⌘ in `send_paste`: Chromium/Electron track modifier state from
            // that event and would otherwise see a plain Return (a send).
            Keystroke::Newline => vec![
                key(SHIFT_KEYCODE, true, FLAG_SHIFT),
                key(RETURN_KEYCODE, true, FLAG_SHIFT),
                key(RETURN_KEYCODE, false, FLAG_SHIFT),
                key(SHIFT_KEYCODE, false, 0),
            ],
        }
    }
}

fn key(keycode: u16, down: bool, flags: u64) -> KeyEvent {
    KeyEvent {
        keycode,
        down,
        flags,
        text: None,
    }
}

/// Split `text` into keystrokes. `\n` and `\r\n` become [`Keystroke::Newline`];
/// everything else is packed into chunks of at most [`MAX_CHUNK_UTF16`]
/// units. Returns `None` when the text holds a character that cannot be
/// typed as text (see [`untypable_char`]).
///
/// Chunks break between clusters from [`clusters`], so a surrogate pair is
/// never split and common multi-code-point characters (accents, emoji with
/// skin tones, ZWJ sequences, flags) stay whole. A cluster longer than a
/// chunk is split between code points.
pub fn plan(text: &str) -> Option<Vec<Keystroke>> {
    if untypable_char(text).is_some() {
        return None;
    }
    let mut out = Vec::new();
    let mut lines = text.split('\n').peekable();
    while let Some(line) = lines.next() {
        let has_break = lines.peek().is_some();
        let line = if has_break {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        };
        pack(line, &mut out);
        if has_break {
            out.push(Keystroke::Newline);
        }
    }
    Some(out)
}

fn pack(line: &str, out: &mut Vec<Keystroke>) {
    let mut chunk = String::new();
    let mut units = 0;
    let mut flush = |chunk: &mut String, units: &mut usize| {
        if !chunk.is_empty() {
            out.push(Keystroke::Text(std::mem::take(chunk)));
            *units = 0;
        }
    };
    for cluster in clusters(line) {
        let len = cluster.encode_utf16().count();
        if units + len > MAX_CHUNK_UTF16 {
            flush(&mut chunk, &mut units);
        }
        if len <= MAX_CHUNK_UTF16 {
            chunk.push_str(cluster);
            units += len;
            continue;
        }
        // Longer than one event can carry: split between code points.
        for c in cluster.chars() {
            if units + c.len_utf16() > MAX_CHUNK_UTF16 {
                flush(&mut chunk, &mut units);
            }
            chunk.push(c);
            units += c.len_utf16();
        }
    }
    flush(&mut chunk, &mut units);
}

/// Split `line` into user-perceived characters, cheaply. A code point joins
/// the previous one when it is a combining mark, variation selector, ZWJ,
/// emoji skin-tone modifier or tag, when it follows a ZWJ, or when it is the
/// second regional indicator of a flag. This is a subset of UAX #29 (no
/// Hangul jamo or Indic conjunct rules); a cluster it misses is split
/// between code points, which inserts the same text over two events.
pub fn clusters(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<char> = None;
    let mut ri_in_cluster = 0;
    for (i, c) in line.char_indices() {
        let joins = prev.is_some()
            && (is_extender(c)
                || prev == Some(ZWJ)
                || (is_regional_indicator(c) && ri_in_cluster == 1));
        if !joins && i > 0 {
            out.push(&line[start..i]);
            start = i;
            ri_in_cluster = 0;
        }
        if is_regional_indicator(c) {
            ri_in_cluster += 1;
        }
        prev = Some(c);
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

const ZWJ: char = '\u{200D}';

fn is_extender(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F      // combining diacritical marks
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF // combining marks for symbols (incl. keycap)
            | 0xFE20..=0xFE2F
            | 0xFE00..=0xFE0F // variation selectors
            | 0xE0100..=0xE01EF
            | 0x200D          // zero-width joiner
            | 0x1F3FB..=0x1F3FF // emoji skin tones
            | 0xE0020..=0xE007F // tags (subdivision flags)
    )
}

fn is_regional_indicator(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

/// The first character that cannot be typed as text: a control character
/// other than `\n` or the `\r` of `\r\n` (tabs move focus, others are
/// dropped or act as keys), or a Unicode line/paragraph separator.
pub fn untypable_char(text: &str) -> Option<char> {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let ok = match c {
            '\n' => true,
            '\r' => chars.peek() == Some(&'\n'),
            '\u{2028}' | '\u{2029}' => false,
            c => !c.is_control(),
        };
        if !ok {
            return Some(c);
        }
    }
    None
}

/// The first trigger character in `text` at a position where it opens UI.
pub fn find_trigger(text: &str) -> Option<(char, TriggerPosition)> {
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        for &(t, pos) in TRIGGERS {
            if c != t {
                continue;
            }
            let hit = match pos {
                TriggerPosition::Anywhere => true,
                TriggerPosition::BeforeNonSpace => {
                    chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
                }
                TriggerPosition::LineStart => i == 0 || chars[i - 1] == '\n',
            };
            if hit {
                return Some((c, pos));
            }
        }
    }
    None
}

/// Why typing was not attempted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline {
    EmptyText,
    UntypableChar(char),
    TerminalApp,
    RemoteSession,
    TriggerChar(char),
    MultilineInSingleLineField,
    SecureInput,
    NotFrontmost,
    NonAsciiInputSource,
    UnknownInputSource,
    ModifiersHeld,
    SlowerThanPaste(usize),
    NoEventSource(String),
}

impl fmt::Display for Decline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decline::EmptyText => write!(f, "empty text"),
            Decline::UntypableChar(c) => write!(f, "untypable character {:?}", c),
            Decline::TerminalApp => write!(f, "terminal app"),
            Decline::RemoteSession => write!(f, "remote desktop or VM"),
            Decline::TriggerChar(c) => write!(f, "{c:?} would open autocomplete"),
            Decline::MultilineInSingleLineField => {
                write!(f, "newline in a single-line field would submit it")
            }
            Decline::SecureInput => write!(f, "Secure Event Input is on"),
            Decline::NotFrontmost => write!(f, "target is not frontmost"),
            Decline::NonAsciiInputSource => write!(f, "input method active"),
            Decline::UnknownInputSource => write!(f, "input source unknown"),
            Decline::ModifiersHeld => write!(f, "modifier keys still held"),
            Decline::SlowerThanPaste(n) => write!(f, "{n} keystrokes is slower than a paste"),
            Decline::NoEventSource(e) => write!(f, "no event source: {e}"),
        }
    }
}

/// Decide from the text and target app alone whether typing may be tried.
pub fn check_text(text: &str, bundle_id: Option<&str>) -> Result<(), Decline> {
    if text.is_empty() {
        return Err(Decline::EmptyText);
    }
    if let Some(id) = bundle_id {
        if crate::text_insert::CLIPBOARD_ONLY_BUNDLES.contains(&id) {
            return Err(Decline::TerminalApp);
        }
        if REMOTE_BUNDLES.contains(&id) {
            return Err(Decline::RemoteSession);
        }
    }
    if let Some(c) = untypable_char(text) {
        return Err(Decline::UntypableChar(c));
    }
    if let Some((c, _)) = find_trigger(text) {
        return Err(Decline::TriggerChar(c));
    }
    Ok(())
}

/// [`check_text`], plus the focused field: in a single-line field
/// (`AXTextField`) Shift+Return is still Return, which submits the form.
pub fn check_request(req: &Request) -> Result<(), Decline> {
    check_text(req.text, req.bundle_id)?;
    if req.role == Some(SINGLE_LINE_ROLE) && req.text.contains('\n') {
        return Err(Decline::MultilineInSingleLineField);
    }
    Ok(())
}

/// Text needing more than [`MAX_KEYSTROKES`] is faster to paste.
pub fn check_length(text: &str) -> Result<(), Decline> {
    let keystrokes = plan(text).map_or(0, |p| p.len());
    if keystrokes > MAX_KEYSTROKES {
        return Err(Decline::SlowerThanPaste(keystrokes));
    }
    Ok(())
}

const SINGLE_LINE_ROLE: &str = "AXTextField";

/// System state that decides whether posted events arrive as typed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Environment {
    pub secure_input: bool,
    pub frontmost_pid: Option<i32>,
    /// `None` when the input source is unknown.
    pub ascii_capable: Option<bool>,
}

/// Decide from system state whether events for `pid` would arrive intact.
pub fn check_environment(pid: i32, env: &Environment) -> Result<(), Decline> {
    if env.secure_input {
        return Err(Decline::SecureInput);
    }
    if env.frontmost_pid != Some(pid) {
        return Err(Decline::NotFrontmost);
    }
    match env.ascii_capable {
        Some(true) => Ok(()),
        Some(false) => Err(Decline::NonAsciiInputSource),
        None => Err(Decline::UnknownInputSource),
    }
}

/// The OS as the keystroke inserter sees it.
pub trait KeystrokeTarget {
    fn environment(&self) -> Environment;
    /// Hardware modifier flags right now.
    fn modifier_flags(&self) -> u64;
    fn post(&self, event: &KeyEvent) -> Result<(), String>;
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

/// Wait up to [`MODIFIER_RELEASE_TIMEOUT`] for every modifier in
/// [`HELD_MODIFIER_MASK`] to be released. Returns whether they were.
pub fn wait_for_modifier_release<T: KeystrokeTarget>(target: &T) -> bool {
    let started = target.now();
    loop {
        if target.modifier_flags() & HELD_MODIFIER_MASK == 0 {
            return true;
        }
        if target.now().saturating_duration_since(started) >= MODIFIER_RELEASE_TIMEOUT {
            return false;
        }
        target.sleep(MODIFIER_POLL_INTERVAL);
    }
}

/// Type `req.text` into `req.pid`. Declines before posting anything unless
/// every check passes. The environment is re-checked before each keystroke,
/// so switching apps mid-way stops the typing (reported as uncertain, since
/// part of the text has landed).
pub fn type_text<T: KeystrokeTarget>(target: &T, req: &Request) -> Attempt {
    let declined = |d: Decline| Attempt::Declined(d.to_string());
    if let Err(d) = check_request(req) {
        return declined(d);
    }
    let Some(keystrokes) = plan(req.text) else {
        return declined(Decline::UntypableChar('?'));
    };
    // Fail fast before waiting on modifiers.
    if let Err(d) = check_environment(req.pid, &target.environment()) {
        return declined(d);
    }
    if !wait_for_modifier_release(target) {
        return declined(Decline::ModifiersHeld);
    }

    let mut posted = 0usize;
    let mut shift_down = false;
    for (i, keystroke) in keystrokes.iter().enumerate() {
        if i > 0 {
            target.sleep(KEYSTROKE_PACING);
        }
        if let Err(d) = check_environment(req.pid, &target.environment()) {
            return if posted == 0 {
                declined(d)
            } else {
                uncertain(format!(
                    "stopped after {i} of {} keystrokes: {d}",
                    keystrokes.len()
                ))
            };
        }
        for event in keystroke.events() {
            if let Err(e) = target.post(&event) {
                if shift_down {
                    // Best effort: never leave Shift logically held.
                    let _ = target.post(&key(SHIFT_KEYCODE, false, 0));
                }
                return if posted == 0 {
                    declined(Decline::NoEventSource(e))
                } else {
                    uncertain(format!("posting failed mid-way: {e}"))
                };
            }
            posted += 1;
            if event.keycode == SHIFT_KEYCODE {
                shift_down = event.down;
            }
        }
    }
    Attempt::Inserted { verified: false }
}

fn uncertain(why: String) -> Attempt {
    Attempt::Uncertain(format!(
        "Typing the dictated text was interrupted ({why}). It was not inserted again; \
         copy it from Captures if part of it is missing."
    ))
}

/// The keystroke step of the insertion chain.
pub struct Keystrokes {
    _private: (),
}

impl Keystrokes {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for Keystrokes {
    fn default() -> Self {
        Self::new()
    }
}

impl Inserter for Keystrokes {
    fn method(&self) -> Method {
        Method::Keystrokes
    }

    fn attempt(&self, req: &Request) -> Attempt {
        // The text checks need no OS state; skip creating a source for them.
        if let Err(d) = check_request(req).and_then(|()| check_length(req.text)) {
            return Attempt::Declined(d.to_string());
        }
        match live::LiveTarget::new() {
            Ok(target) => type_text(&target, req),
            Err(e) => Attempt::Declined(Decline::NoEventSource(e).to_string()),
        }
    }
}

/// For manual runs outside the app: the input-source cache is filled on the
/// app's main thread, which a test does not run, so these assume a plain
/// keyboard layout.
#[cfg(test)]
pub(crate) mod assume_ascii {
    use super::*;

    pub struct AssumeAscii(pub live::LiveTarget);

    impl KeystrokeTarget for AssumeAscii {
        fn environment(&self) -> Environment {
            Environment {
                ascii_capable: Some(true),
                ..self.0.environment()
            }
        }
        fn modifier_flags(&self) -> u64 {
            self.0.modifier_flags()
        }
        fn post(&self, event: &KeyEvent) -> Result<(), String> {
            self.0.post(event)
        }
        fn now(&self) -> Instant {
            self.0.now()
        }
        fn sleep(&self, d: Duration) {
            self.0.sleep(d)
        }
    }

    /// [`Keystrokes`] with the ASCII input-source check assumed to pass.
    pub struct KeystrokesAssumingAscii;

    impl Inserter for KeystrokesAssumingAscii {
        fn method(&self) -> Method {
            Method::Keystrokes
        }
        fn attempt(&self, req: &Request) -> Attempt {
            if let Err(d) = check_request(req) {
                return Attempt::Declined(d.to_string());
            }
            match live::LiveTarget::new() {
                Ok(target) => type_text(&AssumeAscii(target), req),
                Err(e) => Attempt::Declined(e),
            }
        }
    }
}

mod live {
    use super::{Environment, KeyEvent, KeystrokeTarget};
    use crate::synthetic_keys::{hardware_modifier_flags, KeySource};
    use std::time::{Duration, Instant};

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        /// Set while any app has a password field focused (or a terminal's
        /// Secure Keyboard Entry is on). Synthetic key events are then not
        /// delivered.
        fn IsSecureEventInputEnabled() -> u8;
    }

    pub struct LiveTarget {
        source: KeySource,
    }

    impl LiveTarget {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                source: KeySource::new()?,
            })
        }
    }

    impl KeystrokeTarget for LiveTarget {
        fn environment(&self) -> Environment {
            Environment {
                secure_input: unsafe { IsSecureEventInputEnabled() } != 0,
                frontmost_pid: crate::focus_capture::frontmost_pid(),
                // Cached on the main thread: TIS calls off the main thread
                // crash on recent macOS.
                ascii_capable: crate::keyboard_layout::input_source_is_ascii_capable(),
            }
        }

        fn modifier_flags(&self) -> u64 {
            hardware_modifier_flags()
        }

        fn post(&self, event: &KeyEvent) -> Result<(), String> {
            self.source.post(
                event.keycode,
                event.down,
                event.flags,
                event.text.as_deref(),
            )
        }

        fn now(&self) -> Instant {
            Instant::now()
        }

        fn sleep(&self, duration: Duration) {
            std::thread::sleep(duration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn text(s: &str) -> Keystroke {
        Keystroke::Text(s.to_string())
    }

    fn units(k: &Keystroke) -> usize {
        match k {
            Keystroke::Text(s) => s.encode_utf16().count(),
            Keystroke::Newline => 0,
        }
    }

    /// Rebuild the text a plan types (`\r\n` comes back as `\n`).
    fn typed(plan: &[Keystroke]) -> String {
        plan.iter()
            .map(|k| match k {
                Keystroke::Text(s) => s.as_str(),
                Keystroke::Newline => "\n",
            })
            .collect()
    }

    fn assert_well_formed(plan: &[Keystroke]) {
        for k in plan {
            if let Keystroke::Text(s) = k {
                assert!(!s.is_empty(), "empty chunk");
                assert!(units(k) <= MAX_CHUNK_UTF16, "chunk too long: {s:?}");
                let first = s.chars().next().unwrap();
                assert!(!matches!(first, '\n' | '\r' | '\t'), "bad start: {s:?}");
                assert!(!s.contains('\n'), "newline inside chunk: {s:?}");
            }
        }
    }

    // ---------------------------------------------------------------- plan

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(plan("hello").unwrap(), vec![text("hello")]);
    }

    #[test]
    fn twenty_units_fit_in_one_chunk_and_twenty_one_do_not() {
        let a20 = "a".repeat(20);
        assert_eq!(plan(&a20).unwrap(), vec![text(&a20)]);
        let a21 = "a".repeat(21);
        assert_eq!(plan(&a21).unwrap(), vec![text(&a20), text("a")]);
    }

    #[test]
    fn surrogate_pair_at_the_boundary_moves_to_the_next_chunk() {
        let s = format!("{}😀", "a".repeat(19));
        assert_eq!(plan(&s).unwrap(), vec![text(&"a".repeat(19)), text("😀")]);
    }

    #[test]
    fn surrogate_pair_that_fits_exactly_stays() {
        let s = format!("{}😀b", "a".repeat(18));
        assert_eq!(
            plan(&s).unwrap(),
            vec![text(&format!("{}😀", "a".repeat(18))), text("b")]
        );
    }

    #[test]
    fn emoji_only_text_packs_whole_emoji() {
        let s = "😀".repeat(15); // 30 units
        let p = plan(&s).unwrap();
        assert_eq!(p, vec![text(&"😀".repeat(10)), text(&"😀".repeat(5))]);
        assert_eq!(typed(&p), s);
    }

    #[test]
    fn zwj_family_emoji_is_not_split() {
        let family = "👨\u{200D}👩\u{200D}👧\u{200D}👦"; // 11 units
        let s = format!("{}{family}", "a".repeat(15));
        let p = plan(&s).unwrap();
        assert_eq!(p, vec![text(&"a".repeat(15)), text(family)]);
    }

    #[test]
    fn skin_tone_and_flags_stay_whole() {
        let thumbs = "👍🏽"; // 4 units
        let s = format!("{}{thumbs}", "a".repeat(18));
        assert_eq!(plan(&s).unwrap(), vec![text(&"a".repeat(18)), text(thumbs)]);

        let flags = "🇺🇸🇯🇵🇫🇷"; // three flags, 4 units each
        assert_eq!(clusters(flags), vec!["🇺🇸", "🇯🇵", "🇫🇷"]);
        let s = format!("{}{flags}", "a".repeat(10));
        assert_eq!(
            plan(&s).unwrap(),
            vec![text(&format!("{}🇺🇸🇯🇵", "a".repeat(10))), text("🇫🇷")]
        );
    }

    #[test]
    fn combining_mark_stays_with_its_base() {
        let s = format!("{}e\u{301}", "a".repeat(19));
        assert_eq!(
            plan(&s).unwrap(),
            vec![text(&"a".repeat(19)), text("e\u{301}")]
        );
    }

    #[test]
    fn keycap_and_variation_selector_sequences_are_clusters() {
        assert_eq!(
            clusters("1\u{FE0F}\u{20E3}x"),
            vec!["1\u{FE0F}\u{20E3}", "x"]
        );
        assert_eq!(clusters("❤\u{FE0F}!"), vec!["❤\u{FE0F}", "!"]);
    }

    #[test]
    fn tag_sequence_flag_is_one_cluster() {
        let england = "🏴\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"; // 14 units
        assert_eq!(clusters(england), vec![england]);
        let s = format!("{}{england}", "a".repeat(10));
        assert_eq!(
            plan(&s).unwrap(),
            vec![text(&"a".repeat(10)), text(england)]
        );
    }

    #[test]
    fn oversized_cluster_splits_between_code_points() {
        let s = format!("a{}", "\u{301}".repeat(30)); // one 31-unit cluster
        let p = plan(&s).unwrap();
        assert_well_formed(&p);
        assert_eq!(p.len(), 2);
        assert_eq!(typed(&p), s);
    }

    #[test]
    fn oversized_emoji_cluster_never_splits_a_surrogate_pair() {
        let s = "😀\u{200D}".repeat(8); // one cluster, 24 units
        let p = plan(&s).unwrap();
        assert_well_formed(&p);
        assert_eq!(typed(&p), s);
        for k in &p {
            if let Keystroke::Text(t) = k {
                // Round-trips through UTF-16 only if no pair was split.
                let u: Vec<u16> = t.encode_utf16().collect();
                assert_eq!(String::from_utf16(&u).unwrap(), *t);
            }
        }
    }

    #[test]
    fn newlines_become_newline_keystrokes() {
        assert_eq!(
            plan("hi\nyo").unwrap(),
            vec![text("hi"), Keystroke::Newline, text("yo")]
        );
    }

    #[test]
    fn crlf_is_one_newline() {
        assert_eq!(
            plan("a\r\nb\r\n").unwrap(),
            vec![text("a"), Keystroke::Newline, text("b"), Keystroke::Newline]
        );
    }

    #[test]
    fn leading_trailing_and_repeated_newlines() {
        assert_eq!(
            plan("\nhi\n\n").unwrap(),
            vec![
                Keystroke::Newline,
                text("hi"),
                Keystroke::Newline,
                Keystroke::Newline
            ]
        );
        assert_eq!(plan("\n").unwrap(), vec![Keystroke::Newline]);
    }

    #[test]
    fn chunks_restart_after_a_newline() {
        let s = format!("{}\n{}", "a".repeat(15), "b".repeat(15));
        assert_eq!(
            plan(&s).unwrap(),
            vec![
                text(&"a".repeat(15)),
                Keystroke::Newline,
                text(&"b".repeat(15))
            ]
        );
    }

    #[test]
    fn thousand_chars_make_fifty_chunks() {
        let s = "abcdefghij".repeat(100);
        let p = plan(&s).unwrap();
        assert_eq!(p.len(), 50);
        assert!(p.iter().all(|k| units(k) == 20));
        assert_eq!(typed(&p), s);
    }

    #[test]
    fn plans_rebuild_their_text() {
        let samples = [
            "Hello, world. How are you?",
            "Line one\nLine two\r\nLine three",
            "naïve café — “quoted” 日本語のテキスト",
            "mixed 😀 emoji 👨\u{200D}💻 and flags 🇩🇪 in a long sentence that spans chunks",
            &"é".repeat(45),
        ];
        for s in samples {
            let p = plan(s).unwrap();
            assert_well_formed(&p);
            assert_eq!(typed(&p), s.replace("\r\n", "\n"), "{s:?}");
        }
    }

    #[test]
    fn untypable_text_has_no_plan() {
        for s in [
            "a\tb", "a\rb", "a\u{7}b", "\u{2028}", "x\u{85}", "\u{1b}[A", "end\r",
        ] {
            assert_eq!(plan(s), None, "{s:?}");
        }
    }

    #[test]
    fn text_keystroke_events_carry_the_text_with_no_flags() {
        let units: Vec<u16> = "hé😀".encode_utf16().collect();
        assert_eq!(
            text("hé😀").events(),
            vec![
                KeyEvent {
                    keycode: TEXT_KEYCODE,
                    down: true,
                    flags: 0,
                    text: Some(units.clone())
                },
                KeyEvent {
                    keycode: TEXT_KEYCODE,
                    down: false,
                    flags: 0,
                    text: Some(units)
                },
            ]
        );
    }

    #[test]
    fn newline_is_shift_return() {
        assert_eq!(
            Keystroke::Newline.events(),
            vec![
                key(0x38, true, FLAG_SHIFT),
                key(0x24, true, FLAG_SHIFT),
                key(0x24, false, FLAG_SHIFT),
                key(0x38, false, 0),
            ]
        );
    }

    /// Not a benchmark harness, just a guard that planning stays trivially
    /// cheap next to posting. Run with `--nocapture` to see the number.
    #[test]
    fn planning_2000_chars_is_fast() {
        let s = "The quick brown fox 😀 jumps over the lazy dog.\n".repeat(42);
        let s: String = s.chars().take(2000).collect();
        let started = Instant::now();
        let mut total = 0;
        for _ in 0..100 {
            total += plan(&s).unwrap().len();
        }
        let per = started.elapsed() / 100;
        println!(
            "plan(2000 chars): {per:?} per call, {} keystrokes",
            total / 100
        );
        assert!(per < Duration::from_millis(20));
    }

    // --------------------------------------------------------- eligibility

    #[test]
    fn text_eligibility_table() {
        use Decline::*;
        let cases: &[(&str, Option<&str>, Result<(), Decline>)] = &[
            ("hello there", None, Ok(())),
            ("hello there", Some("com.tinyspeck.slackmacgap"), Ok(())),
            ("", None, Err(EmptyText)),
            ("a\tb", None, Err(UntypableChar('\t'))),
            ("a\rb", None, Err(UntypableChar('\r'))),
            ("a\r\nb", None, Ok(())),
            ("a\nb", None, Ok(())),
            ("bell\u{7}", None, Err(UntypableChar('\u{7}'))),
            ("sep\u{2028}", None, Err(UntypableChar('\u{2028}'))),
            ("hi", Some("com.apple.Terminal"), Err(TerminalApp)),
            ("hi", Some("com.mitchellh.ghostty"), Err(TerminalApp)),
            ("hi", Some("com.microsoft.rdc.macos"), Err(RemoteSession)),
            ("hi", Some("com.apple.ScreenSharing"), Err(RemoteSession)),
            ("hi", Some("com.utmapp.UTM"), Err(RemoteSession)),
            // Trigger characters.
            ("email me@example.com", None, Err(TriggerChar('@'))),
            ("@sam hi", None, Err(TriggerChar('@'))),
            ("at 10:30", None, Err(TriggerChar(':'))),
            ("see :smile", None, Err(TriggerChar(':'))),
            ("https://x.com", None, Err(TriggerChar(':'))),
            ("Note: this is fine", None, Ok(())),
            ("ends with colon:", None, Ok(())),
            ("list:\nitem", None, Ok(())),
            ("/giphy cats", None, Err(TriggerChar('/'))),
            ("first\n/remind me", None, Err(TriggerChar('/'))),
            ("either/or", None, Ok(())),
            ("a / b", None, Ok(())),
            (" /leading space", None, Ok(())),
        ];
        for (text, bundle, want) in cases {
            assert_eq!(check_text(text, *bundle), *want, "{text:?} in {bundle:?}");
        }
    }

    #[test]
    fn every_listed_bundle_is_declined() {
        for id in crate::text_insert::CLIPBOARD_ONLY_BUNDLES {
            assert_eq!(check_text("hi", Some(id)), Err(Decline::TerminalApp));
        }
        for id in REMOTE_BUNDLES {
            assert_eq!(check_text("hi", Some(id)), Err(Decline::RemoteSession));
        }
    }

    fn good_env() -> Environment {
        Environment {
            secure_input: false,
            frontmost_pid: Some(42),
            ascii_capable: Some(true),
        }
    }

    #[test]
    fn environment_eligibility_table() {
        let with = |f: fn(&mut Environment)| {
            let mut e = good_env();
            f(&mut e);
            e
        };
        let cases = [
            (good_env(), Ok(())),
            (with(|e| e.secure_input = true), Err(Decline::SecureInput)),
            (
                with(|e| e.frontmost_pid = Some(7)),
                Err(Decline::NotFrontmost),
            ),
            (with(|e| e.frontmost_pid = None), Err(Decline::NotFrontmost)),
            (
                with(|e| e.ascii_capable = Some(false)),
                Err(Decline::NonAsciiInputSource),
            ),
            (
                with(|e| e.ascii_capable = None),
                Err(Decline::UnknownInputSource),
            ),
            // Secure input is reported first: it blocks everything.
            (
                with(|e| {
                    e.secure_input = true;
                    e.frontmost_pid = None;
                }),
                Err(Decline::SecureInput),
            ),
        ];
        for (env, want) in cases {
            assert_eq!(check_environment(42, &env), want, "{env:?}");
        }
    }

    #[test]
    fn caps_lock_is_not_a_held_modifier() {
        assert_eq!(0x0001_0000 & HELD_MODIFIER_MASK, 0);
        for bit in [
            0x0002_0000,
            0x0004_0000,
            0x0008_0000,
            0x0010_0000,
            0x0080_0000,
        ] {
            assert_ne!(bit & HELD_MODIFIER_MASK, 0);
        }
    }

    // ------------------------------------------------------- orchestration

    /// A scripted OS: a fake clock advanced by `sleep`, modifiers held until
    /// a given time, an environment that can change after N posts, and a
    /// post that can fail at a given index.
    struct Fake {
        env: Environment,
        env_after: Option<(usize, Environment)>,
        modifiers: u64,
        modifiers_until: Duration,
        fail_post_at: Option<usize>,
        base: Instant,
        clock: Cell<Duration>,
        posted: RefCell<Vec<KeyEvent>>,
        attempts: Cell<usize>,
        sleeps: RefCell<Vec<Duration>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                env: good_env(),
                env_after: None,
                modifiers: 0,
                modifiers_until: Duration::ZERO,
                fail_post_at: None,
                base: Instant::now(),
                clock: Cell::new(Duration::ZERO),
                posted: RefCell::default(),
                attempts: Cell::new(0),
                sleeps: RefCell::default(),
            }
        }
        fn posted(&self) -> Vec<KeyEvent> {
            self.posted.borrow().clone()
        }
    }

    impl KeystrokeTarget for Fake {
        fn environment(&self) -> Environment {
            match self.env_after {
                Some((n, env)) if self.posted.borrow().len() >= n => env,
                _ => self.env,
            }
        }
        fn modifier_flags(&self) -> u64 {
            if self.clock.get() < self.modifiers_until {
                self.modifiers
            } else {
                0
            }
        }
        fn post(&self, event: &KeyEvent) -> Result<(), String> {
            let n = self.attempts.get();
            self.attempts.set(n + 1);
            if self.fail_post_at == Some(n) {
                return Err("boom".into());
            }
            self.posted.borrow_mut().push(event.clone());
            Ok(())
        }
        fn now(&self) -> Instant {
            self.base + self.clock.get()
        }
        fn sleep(&self, d: Duration) {
            self.sleeps.borrow_mut().push(d);
            self.clock.set(self.clock.get() + d);
        }
    }

    fn req(text: &str) -> Request<'_> {
        Request {
            pid: 42,
            bundle_id: Some("com.tinyspeck.slackmacgap"),
            role: None,
            text,
        }
    }

    #[test]
    fn text_that_is_faster_to_paste_is_not_typed() {
        let four = "a".repeat(20 * MAX_KEYSTROKES);
        assert_eq!(check_length(&four), Ok(()));
        let five = "a".repeat(20 * MAX_KEYSTROKES + 1);
        assert_eq!(
            check_length(&five),
            Err(Decline::SlowerThanPaste(MAX_KEYSTROKES + 1))
        );
        // Each newline is a keystroke of its own.
        assert_eq!(check_length("a\nb"), Ok(()));
        assert_eq!(check_length("a\nb\nc"), Err(Decline::SlowerThanPaste(5)));
    }

    #[test]
    fn multiline_text_is_never_typed_into_a_single_line_field() {
        let field = |role, text| Request {
            role: Some(role),
            ..req(text)
        };
        assert_eq!(
            check_request(&field("AXTextField", "one\ntwo")),
            Err(Decline::MultilineInSingleLineField)
        );
        assert_eq!(check_request(&field("AXTextField", "one line")), Ok(()));
        assert_eq!(check_request(&field("AXTextArea", "one\ntwo")), Ok(()));
        // Unknown role: Shift+Return is the best guess.
        assert_eq!(check_request(&req("one\ntwo")), Ok(()));
    }

    fn events_of(plan: &[Keystroke]) -> Vec<KeyEvent> {
        plan.iter().flat_map(|k| k.events()).collect()
    }

    #[test]
    fn types_the_exact_event_sequence() {
        let fake = Fake::new();
        let attempt = type_text(&fake, &req("hi\nyo"));
        assert_eq!(attempt, Attempt::Inserted { verified: false });
        let mut want = text("hi").events();
        want.extend(Keystroke::Newline.events());
        want.extend(text("yo").events());
        assert_eq!(fake.posted(), want);
        // Paced between keystrokes, not after the last.
        assert_eq!(*fake.sleeps.borrow(), vec![KEYSTROKE_PACING; 2]);
    }

    #[test]
    fn long_text_posts_every_chunk_in_order() {
        let s = "abcdefghij".repeat(100);
        let fake = Fake::new();
        assert_eq!(
            type_text(&fake, &req(&s)),
            Attempt::Inserted { verified: false }
        );
        let posted = fake.posted();
        assert_eq!(posted.len(), 100);
        assert_eq!(posted, events_of(&plan(&s).unwrap()));
        assert_eq!(fake.clock.get(), KEYSTROKE_PACING * 49);
    }

    #[test]
    fn surrogates_and_crlf_are_posted_intact() {
        let s = format!("{}😀\r\nok", "a".repeat(19));
        let fake = Fake::new();
        type_text(&fake, &req(&s));
        let texts: Vec<String> = fake
            .posted()
            .iter()
            .filter(|e| e.down)
            .map(|e| match &e.text {
                Some(u) => String::from_utf16(u).unwrap(),
                None => format!("<{:#x}>", e.keycode),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "a".repeat(19),
                "😀".into(),
                "<0x38>".into(),
                "<0x24>".into(),
                "ok".into()
            ]
        );
    }

    #[test]
    fn waits_for_modifiers_to_be_released() {
        let mut fake = Fake::new();
        fake.modifiers = 0x0010_0000 | 0x0008_0000; // ⌘⌥ still held
        fake.modifiers_until = Duration::from_millis(40);
        assert_eq!(
            type_text(&fake, &req("hi")),
            Attempt::Inserted { verified: false }
        );
        assert_eq!(fake.clock.get(), Duration::from_millis(40));
        assert!(fake
            .sleeps
            .borrow()
            .iter()
            .all(|d| *d == MODIFIER_POLL_INTERVAL));
        assert_eq!(fake.posted().len(), 2);
    }

    #[test]
    fn declines_when_modifiers_stay_held() {
        let mut fake = Fake::new();
        fake.modifiers = 0x0010_0000;
        fake.modifiers_until = Duration::from_secs(10);
        let attempt = type_text(&fake, &req("hi"));
        assert_eq!(
            attempt,
            Attempt::Declined(Decline::ModifiersHeld.to_string())
        );
        assert!(fake.posted().is_empty());
        assert_eq!(fake.clock.get(), MODIFIER_RELEASE_TIMEOUT);
    }

    #[test]
    fn caps_lock_does_not_wait() {
        let mut fake = Fake::new();
        fake.modifiers = 0x0001_0000;
        fake.modifiers_until = Duration::from_secs(10);
        assert_eq!(
            type_text(&fake, &req("hi")),
            Attempt::Inserted { verified: false }
        );
        assert_eq!(fake.clock.get(), Duration::ZERO);
    }

    #[test]
    fn environment_problems_decline_before_waiting_or_posting() {
        for (env, why) in [
            (
                Environment {
                    secure_input: true,
                    ..good_env()
                },
                Decline::SecureInput,
            ),
            (
                Environment {
                    frontmost_pid: Some(1),
                    ..good_env()
                },
                Decline::NotFrontmost,
            ),
            (
                Environment {
                    ascii_capable: Some(false),
                    ..good_env()
                },
                Decline::NonAsciiInputSource,
            ),
        ] {
            let mut fake = Fake::new();
            fake.env = env;
            fake.modifiers = 0x0010_0000;
            fake.modifiers_until = Duration::from_secs(10);
            assert_eq!(
                type_text(&fake, &req("hi")),
                Attempt::Declined(why.to_string())
            );
            assert!(fake.posted().is_empty());
            assert_eq!(fake.clock.get(), Duration::ZERO, "must not wait");
        }
    }

    #[test]
    fn text_problems_decline_without_posting() {
        for s in ["", "hey @sam", "a\tb", "/giphy"] {
            let fake = Fake::new();
            assert!(
                matches!(type_text(&fake, &req(s)), Attempt::Declined(_)),
                "{s:?}"
            );
            assert!(fake.posted().is_empty());
        }
    }

    #[test]
    fn failure_on_the_first_post_declines() {
        let mut fake = Fake::new();
        fake.fail_post_at = Some(0);
        assert!(matches!(type_text(&fake, &req("hi")), Attempt::Declined(_)));
        assert!(fake.posted().is_empty());
    }

    #[test]
    fn failure_mid_way_is_uncertain() {
        let mut fake = Fake::new();
        fake.fail_post_at = Some(2); // after "hi" down+up
        let attempt = type_text(&fake, &req("hi\nyo"));
        assert!(matches!(attempt, Attempt::Uncertain(_)), "{attempt:?}");
        assert_eq!(fake.posted(), text("hi").events());
    }

    #[test]
    fn failure_inside_a_newline_releases_shift() {
        let mut fake = Fake::new();
        fake.fail_post_at = Some(3); // Shift down posted, Return down fails
        let attempt = type_text(&fake, &req("hi\nyo"));
        assert!(matches!(attempt, Attempt::Uncertain(_)));
        let posted = fake.posted();
        assert_eq!(posted[2], key(SHIFT_KEYCODE, true, FLAG_SHIFT));
        assert_eq!(posted.last(), Some(&key(SHIFT_KEYCODE, false, 0)));
        assert_eq!(posted.len(), 4);
    }

    #[test]
    fn switching_apps_mid_way_stops_typing() {
        let mut fake = Fake::new();
        fake.env_after = Some((
            4,
            Environment {
                frontmost_pid: Some(99),
                ..good_env()
            },
        ));
        let s = "a".repeat(100); // 5 keystrokes
        let attempt = type_text(&fake, &req(&s));
        assert!(matches!(attempt, Attempt::Uncertain(ref m) if m.contains("stopped after 2 of 5")));
        assert_eq!(fake.posted().len(), 4);
    }

    #[test]
    fn secure_input_turning_on_mid_way_stops_typing() {
        let mut fake = Fake::new();
        fake.env_after = Some((
            2,
            Environment {
                secure_input: true,
                ..good_env()
            },
        ));
        let attempt = type_text(&fake, &req(&"a".repeat(40)));
        assert!(matches!(attempt, Attempt::Uncertain(_)));
        assert_eq!(fake.posted().len(), 2);
    }

    #[test]
    fn inserter_reports_the_keystrokes_method() {
        assert_eq!(Keystrokes::new().method(), Method::Keystrokes);
    }

    #[test]
    fn inserter_declines_text_problems_without_touching_the_os() {
        let r = Request {
            pid: 1,
            bundle_id: Some("com.apple.Terminal"),
            role: None,
            text: "hi",
        };
        assert_eq!(
            Keystrokes::new().attempt(&r),
            Attempt::Declined(Decline::TerminalApp.to_string())
        );
    }

    /// MANUAL ONLY: types into whatever app is frontmost. Never run in CI or
    /// while someone is using the Mac. Focus a scratch text field, then run
    /// `cargo test manual_types_into_frontmost_app -- --ignored` within 3 s.
    #[test]
    #[ignore = "posts real keyboard events to the frontmost app"]
    fn manual_types_into_frontmost_app() {
        // The live target, but with the input source assumed ASCII: the
        // cache behind it is filled on the app's main thread, which a test
        // does not run. Use a plain keyboard layout.
        std::thread::sleep(Duration::from_secs(3));
        let pid = crate::focus_capture::frontmost_pid().expect("frontmost app");
        let r = Request {
            pid,
            bundle_id: None,
            role: None,
            text: "Keystroke test 😀 naïve\nsecond line",
        };
        let target = assume_ascii::AssumeAscii(live::LiveTarget::new().expect("event source"));
        println!("{:?}", type_text(&target, &r));
    }
}
