//! Global hotkey → dictation effect bridge.
//!
//! Thin adapter from `keytap::chord::ChordMatcher` to Tauri events. keytap
//! owns the OS event tap + the chord state machine (Momentary vs Toggle,
//! longest-match resolution, sticky-toggle semantics); this module's only
//! job is:
//!
//!   1. Build a `ChordMatcher` from the user's saved PTT, Toggle and
//!      Command chords.
//!   2. Translate `ChordEvent` → kass's [`Effect`] on a dispatcher
//!      thread.
//!   3. Fan [`Effect`]s out into native dictation (microphone + streaming,
//!      see `dictation/`) and dictate-window show.
//!
//! The [`Effect::Restart`] signal is emitted when keytap fires
//! `End(PTT)` and `Start(Toggle)` with the *same* [`Instant`] — which
//! happens when the held set upgrades from a shorter chord to a longer
//! superset in a single event (the classic PTT→hands-free transition).
//! We detect the pair with a 5 ms peek on the matcher's receiver and
//! coalesce into one `Restart` so hosts can discard the transition-
//! moment audio rather than treat it as an unrelated Stop+Start pair.
//!
//! Left- and right-hand modifier variants are kept distinct all the way
//! down to the OS event tap (keytap's core promise). Defaults bind to
//! right-hand Cmd + right-hand Option so the usual left-hand shortcuts
//! stay with the OS / app.
//!
//! Escape cancels a take ([`dictation::cancel`]). It is watched on a second
//! tap of its own: in the matcher, a held chord (longest match) or a latched
//! toggle would hide it. The tap only observes, so Escape still reaches the
//! focused app too.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::host::{AppHandle, Emitter, Manager};
use keytap::chord::{Chord, ChordEvent, ChordMatcher};
use keytap::{EventKind, Key, RecvTimeoutError, Tap};

use crate::dictation;
use crate::dictation_handshake;
use crate::focus_capture;

// ========================================================================
// Public types
// ========================================================================

/// Semantic action a chord can be bound to. `PushToTalk` = hold chord to
/// record, release to stop. `ToggleToTalk` = press chord to start recording,
/// press again to stop. `Command` = hold to speak an instruction for the
/// selected text, release to rewrite it (docs/plans/COMMAND_MODE.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordAction {
    PushToTalk,
    ToggleToTalk,
    Command,
}

impl ChordAction {
    /// The name the webview's `chord:down` / `chord:up` events use.
    pub fn name(self) -> &'static str {
        match self {
            Self::PushToTalk => "push_to_talk",
            Self::ToggleToTalk => "toggle_to_talk",
            Self::Command => "command",
        }
    }

    /// What a take started by this chord is for.
    pub fn take_mode(self) -> dictation::TakeMode {
        match self {
            Self::Command => dictation::TakeMode::Command,
            Self::PushToTalk | Self::ToggleToTalk => dictation::TakeMode::Dictation,
        }
    }
}

/// Effect produced after the chord matcher resolves an event. Hosts
/// translate these into UI / recorder calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Start(ChordAction),
    Stop(ChordAction),
    /// Emitted when a push-to-talk chord is "upgraded" into the toggle
    /// chord mid-hold — hosts may want to discard the captured audio and
    /// restart so the transition moment isn't in the recording.
    Restart(ChordAction),
}

/// Chord key sets from capture settings. Both actions use the same
/// `HashSet<Key>` shape so callers don't need to know about keytap's
/// `Chord` type.
pub type Bindings = HashMap<ChordAction, HashSet<Key>>;

// ========================================================================
// Practice mode + dictation gate
// ========================================================================

/// What a chord press may do, set by the onboarding window
/// (docs/plans/ONBOARDING.md). Practice mode: chords only send their
/// `chord:down` / `chord:up` events. Gate: a chord that would start a take
/// shows this message in the pill instead.
#[derive(Default)]
pub struct ChordMode {
    practice: AtomicBool,
    gate: Mutex<Option<String>>,
}

impl ChordMode {
    /// Turn practice mode off, for when onboarding closes.
    pub fn end_practice(&self) {
        self.practice.store(false, Ordering::Relaxed);
    }

    fn on_start(&self) -> OnStart {
        let gate = self.gate.lock().ok().and_then(|g| g.clone());
        on_start(self.practice.load(Ordering::Relaxed), gate)
    }
}

/// What a chord's start does.
#[derive(Debug, PartialEq, Eq)]
enum OnStart {
    /// Start a take.
    Record,
    /// Practice mode: nothing records and the pill stays hidden.
    Practice,
    /// Nothing records; the pill shows this notice.
    Blocked(String),
}

fn on_start(practice: bool, gate: Option<String>) -> OnStart {
    if practice {
        return OnStart::Practice;
    }
    match gate {
        Some(message) => OnStart::Blocked(message),
        None => OnStart::Record,
    }
}

/// Practice mode on or off: while on, chords only send `chord:down` /
/// `chord:up` events and nothing records.
pub fn set_chord_practice(mode: crate::host::State<'_, ChordMode>, enabled: bool) {
    mode.practice.store(enabled, Ordering::Relaxed);
}

/// Block takes with a message for the pill (the models are still
/// downloading, for example), or unblock them with `None`.
pub fn set_dictation_gate(mode: crate::host::State<'_, ChordMode>, blocked: Option<String>) {
    if let Ok(mut gate) = mode.gate.lock() {
        *gate = blocked;
    }
}

/// Tell every window a chord went down or up, for the onboarding keycaps.
fn emit_chord(app: &AppHandle, event: &str, action: ChordAction) {
    let _ = app.emit(event, serde_json::json!({ "action": action.name() }));
}

// ========================================================================
// Monitor
// ========================================================================

pub struct HotkeyMonitor {
    app: AppHandle,
    active: Option<Active>,
}

struct Active {
    dispatcher: JoinHandle<()>,
    escape: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl Active {
    fn stop(self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = self.dispatcher.join();
        if let Some(escape) = self.escape {
            let _ = escape.join();
        }
    }
}

impl HotkeyMonitor {
    /// Build the monitor with initial bindings. Equivalent to constructing
    /// an empty monitor and calling [`Self::update_bindings`] once.
    pub fn spawn(app: AppHandle, bindings: Bindings) -> Self {
        let mut m = Self { app, active: None };
        m.apply(bindings);
        m
    }

    /// Swap in a fresh set of chord bindings. Tears down the existing
    /// `ChordMatcher` (which stops keytap's chord worker thread and
    /// closes the OS tap) and spawns a new one. No-op for the "all
    /// empty" case so "disable hotkey" doesn't keep a tap running for
    /// no reason.
    pub fn update_bindings(&mut self, bindings: Bindings) {
        self.apply(bindings);
    }

    fn apply(&mut self, bindings: Bindings) {
        // Tear down any existing matcher + dispatcher first. The
        // dispatcher sees the shutdown flag on its next recv_timeout
        // (≤100ms) and returns; joining waits for that. Dropping the
        // ChordMatcher stops keytap's chord-worker thread and the
        // underlying Tap.
        if let Some(active) = self.active.take() {
            active.stop();
        }

        if bindings.values().all(|set| set.is_empty()) {
            return;
        }

        let matcher = match build_matcher(&bindings) {
            Ok(m) => m,
            Err(err) => {
                eprintln!(
                    "HotkeyMonitor: ChordMatcher build failed ({err}). Global chord detection is disabled. On macOS, grant Input Monitoring in System Settings → Privacy & Security → Input Monitoring and relaunch."
                );
                return;
            }
        };

        let shutdown = Arc::new(AtomicBool::new(false));
        // Set by the Escape watcher after a cancel, for the dispatcher.
        let cancelled = Arc::new(AtomicBool::new(false));
        let escape = spawn_escape_watcher(&self.app, &bindings, &shutdown, &cancelled);
        let shutdown_for_thread = shutdown.clone();
        let app = self.app.clone();
        let dispatcher = thread::Builder::new()
            .name("kass-hotkey-dispatcher".into())
            .spawn(move || dispatcher_loop(app, bindings, matcher, shutdown_for_thread, cancelled))
            .expect("spawn hotkey dispatcher thread");

        self.active = Some(Active {
            dispatcher,
            escape,
            shutdown,
        });
    }
}

impl Drop for HotkeyMonitor {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop();
        }
    }
}

// ========================================================================
// Escape
// ========================================================================

/// Watch for Escape on a tap of its own. Skipped when a chord uses Escape
/// itself, so pressing that chord doesn't cancel what it starts.
fn spawn_escape_watcher(
    app: &AppHandle,
    bindings: &Bindings,
    shutdown: &Arc<AtomicBool>,
    cancelled: &Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    if bindings.values().any(|keys| keys.contains(&Key::Escape)) {
        return None;
    }
    let tap = match Tap::new() {
        Ok(tap) => tap,
        Err(err) => {
            eprintln!("HotkeyMonitor: Escape tap failed ({err}); Escape won't cancel dictation.");
            return None;
        }
    };
    let app = app.clone();
    let shutdown = shutdown.clone();
    let cancelled = cancelled.clone();
    let spawned = thread::Builder::new()
        .name("kass-escape-watcher".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match tap.recv_timeout(Duration::from_millis(100)) {
                    // Ignored when no take is open: Escape is only the app's.
                    Ok(event) if is_escape_press(event.kind) => {
                        if dictation::cancel(&app) {
                            cancelled.store(true, Ordering::Relaxed);
                        }
                    }
                    Ok(_) | Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
    match spawned {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("HotkeyMonitor: failed to spawn the Escape watcher ({err})");
            None
        }
    }
}

/// Escape going down. Auto-repeat doesn't count: holding Escape cancels once.
fn is_escape_press(kind: EventKind) -> bool {
    kind == EventKind::KeyDown(Key::Escape)
}

/// Whether keytap holds the toggle chord latched. It stays latched until the
/// chord's next press, even once Escape cancelled the take it started: that
/// press would only unlatch it, and until then keytap ignores the other
/// chords. So after a cancel the dispatcher swaps in a fresh matcher.
#[derive(Debug, Default)]
struct ToggleLatch {
    latched: bool,
}

impl ToggleLatch {
    fn observe(&mut self, effect: Effect) {
        match effect {
            Effect::Start(ChordAction::ToggleToTalk)
            | Effect::Restart(ChordAction::ToggleToTalk) => self.latched = true,
            Effect::Stop(ChordAction::ToggleToTalk) => self.latched = false,
            _ => {}
        }
    }

    /// After a cancel: whether the matcher needs replacing. A fresh one
    /// starts unlatched with nothing held, so keys still down from the
    /// cancelled take start and end nothing when released.
    fn needs_fresh_matcher(&mut self) -> bool {
        std::mem::take(&mut self.latched)
    }
}

// ========================================================================
// Matcher construction + dispatch
// ========================================================================

fn build_matcher(bindings: &Bindings) -> Result<ChordMatcher<ChordAction>, keytap::Error> {
    let mut builder = ChordMatcher::builder();
    if let Some(keys) = bindings.get(&ChordAction::PushToTalk) {
        if !keys.is_empty() {
            builder = builder.add(ChordAction::PushToTalk, Chord::of(keys.iter().copied()));
        }
    }
    if let Some(keys) = bindings.get(&ChordAction::ToggleToTalk) {
        if !keys.is_empty() {
            builder =
                builder.add_toggle(ChordAction::ToggleToTalk, Chord::of(keys.iter().copied()));
        }
    }
    if let Some(keys) = bindings.get(&ChordAction::Command) {
        if !keys.is_empty() {
            builder = builder.add(ChordAction::Command, Chord::of(keys.iter().copied()));
        }
    }
    builder.build()
}

fn dispatcher_loop(
    app: AppHandle,
    bindings: Bindings,
    matcher: ChordMatcher<ChordAction>,
    shutdown: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
) {
    let mut matcher = matcher;
    let mut latch = ToggleLatch::default();
    while !shutdown.load(Ordering::Relaxed) {
        if cancelled.swap(false, Ordering::Relaxed) && latch.needs_fresh_matcher() {
            // Built before the old one drops, so no key event is missed.
            match build_matcher(&bindings) {
                Ok(fresh) => matcher = fresh,
                Err(err) => eprintln!("HotkeyMonitor: could not reset the toggle chord ({err})"),
            }
        }
        match matcher.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => process_event(&app, &matcher, &mut latch, event),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Turn a single [`ChordEvent`] into zero or one [`Effect`]s, peeking at
/// the matcher once for a same-Instant follow-up so upgrade transitions
/// coalesce into [`Effect::Restart`] instead of a Stop+Start
/// pair.
fn process_event(
    app: &AppHandle,
    matcher: &ChordMatcher<ChordAction>,
    latch: &mut ToggleLatch,
    event: ChordEvent<ChordAction>,
) {
    let mut apply_effect = |effect: Effect, time: Instant| {
        latch.observe(effect);
        apply_effect(app, effect, time);
    };
    match event {
        ChordEvent::Start { id, time } => {
            apply_effect(Effect::Start(id), time);
        }
        ChordEvent::End {
            id: end_id,
            time: end_time,
        } => {
            // Peek for an immediately-following Start. keytap emits
            // End+Start atomically (same Instant) when the held set
            // transitions between registered chords — our 5 ms window
            // is well under perceptible latency but far longer than the
            // channel hop between keytap's chord worker and our
            // dispatcher.
            match matcher.recv_timeout(Duration::from_millis(5)) {
                Ok(ChordEvent::Start {
                    id: start_id,
                    time: start_time,
                }) if start_time == end_time => {
                    emit_chord(app, "chord:up", end_id);
                    apply_effect(Effect::Restart(start_id), start_time);
                }
                Ok(other) => {
                    apply_effect(Effect::Stop(end_id), end_time);
                    // The peeked event wasn't a transition partner;
                    // process it in its own right. Recursion depth is
                    // bounded by the number of back-to-back chord
                    // events, in practice 1–2.
                    process_event(app, matcher, latch, other);
                }
                Err(_) => {
                    apply_effect(Effect::Stop(end_id), end_time);
                }
            }
        }
    }
}

// ========================================================================
// Effect → Tauri
// ========================================================================

fn apply_effect(app: &AppHandle, effect: Effect, time: Instant) {
    match effect {
        Effect::Start(action) => {
            match app.state::<ChordMode>().on_start() {
                OnStart::Record => start_take(app, action, time),
                OnStart::Practice => {}
                OnStart::Blocked(message) => dictation::show_notice(app, message),
            }
            emit_chord(app, "chord:down", action);
        }
        Effect::Stop(action) => {
            // Stops nothing when the chord's start didn't record.
            dictation::stop_shortcut_take(app);
            emit_chord(app, "chord:up", action);
        }
        Effect::Restart(action) => {
            // PTT upgraded to hands-free mid-hold: keep the same take
            // recording (it was never interrupted) until the toggle ends it.
            emit_chord(app, "chord:down", action);
        }
    }
}

fn start_take(app: &AppHandle, action: ChordAction, time: Instant) {
    // Open the microphone before anything else: every word from
    // key-down must be captured. `time` is the key event's own
    // timestamp, so the logged latency includes our dispatch.
    let take = dictation::start(
        app,
        time,
        dictation::TakeOrigin::Shortcut,
        action.take_mode(),
    );

    // An app that opted in shows and focuses its field now, so the
    // snapshot below finds it (docs/DICTATION_HANDSHAKE.md). The microphone
    // is already open, so the wait loses no words.
    if let Some(take) = take {
        dictation_handshake::before_focus(take, action.take_mode());
    }

    // Snapshot focus BEFORE we touch the window — any AppKit
    // reshuffle triggered by set_position / show could in principle
    // steal key focus and poison the reading. In practice those
    // calls leave keyWindow alone, but capturing first is free.
    let focus = focus_capture::capture_focus().ok();
    if let Some(take) = take {
        dictation::set_focus(app, take, focus);
    }

    dictation::show_hud(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_records_when_nothing_holds_it_back() {
        assert_eq!(on_start(false, None), OnStart::Record);
    }

    #[test]
    fn practice_mode_records_nothing_and_shows_nothing() {
        assert_eq!(on_start(true, None), OnStart::Practice);
        assert_eq!(
            on_start(true, Some("Still downloading".into())),
            OnStart::Practice
        );
    }

    #[test]
    fn a_gate_shows_its_message_instead_of_recording() {
        assert_eq!(
            on_start(false, Some("Still downloading".into())),
            OnStart::Blocked("Still downloading".into())
        );
    }

    #[test]
    fn chord_actions_have_the_webview_names() {
        assert_eq!(ChordAction::PushToTalk.name(), "push_to_talk");
        assert_eq!(ChordAction::ToggleToTalk.name(), "toggle_to_talk");
        assert_eq!(ChordAction::Command.name(), "command");
    }

    #[test]
    fn only_escape_going_down_cancels() {
        assert!(is_escape_press(EventKind::KeyDown(Key::Escape)));
        assert!(!is_escape_press(EventKind::KeyRepeat(Key::Escape)));
        assert!(!is_escape_press(EventKind::KeyUp(Key::Escape)));
        assert!(!is_escape_press(EventKind::KeyDown(Key::Space)));
    }

    #[test]
    fn a_push_to_talk_cancel_keeps_the_matcher() {
        // Releasing the held chord later stops nothing: the take is gone.
        let mut latch = ToggleLatch::default();
        latch.observe(Effect::Start(ChordAction::PushToTalk));
        assert!(!latch.needs_fresh_matcher());
    }

    #[test]
    fn a_latched_toggle_cancel_gets_a_fresh_matcher() {
        let mut latch = ToggleLatch::default();
        latch.observe(Effect::Start(ChordAction::ToggleToTalk));
        assert!(latch.needs_fresh_matcher());
        // The fresh matcher starts unlatched.
        assert!(!latch.needs_fresh_matcher());
    }

    #[test]
    fn a_push_to_talk_upgraded_to_hands_free_is_latched() {
        let mut latch = ToggleLatch::default();
        latch.observe(Effect::Start(ChordAction::PushToTalk));
        latch.observe(Effect::Restart(ChordAction::ToggleToTalk));
        assert!(latch.needs_fresh_matcher());
    }

    #[test]
    fn a_toggle_already_pressed_off_needs_no_reset() {
        // Escape while its take is still being transcribed.
        let mut latch = ToggleLatch::default();
        latch.observe(Effect::Start(ChordAction::ToggleToTalk));
        latch.observe(Effect::Stop(ChordAction::ToggleToTalk));
        assert!(!latch.needs_fresh_matcher());
    }
}
