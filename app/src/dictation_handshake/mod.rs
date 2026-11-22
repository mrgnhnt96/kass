//! The dictation handshake (docs/DICTATION_HANDSHAKE.md): an app that opts
//! in is told a take is starting before Kass reads its focus, so it can
//! show and focus a real text field first (a terminal's composer, say).
//!
//! At chord-down, once the microphone is open and before the focus
//! snapshot, [`before_focus`] posts `dictationWillBegin` to the frontmost
//! app if it opted in, and waits up to [`READY_TIMEOUT`] for its
//! `dictationReady`. No reply and Kass carries on as if it had never asked.
//! When the take is over, [`take_ended`] posts `dictationDidEnd`.
//!
//! An app opts in with `KassDictationHandshake = true` in its Info.plist,
//! or at runtime by posting `handshakeSupported` with its pid (dev builds
//! that aren't bundles). Any other app costs one pid lookup and a cached
//! answer: nothing is posted and nothing waits.

pub mod wire;

use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use crate::dictation::take::{report_outcome, PillEvent};
use crate::dictation::TakeMode;
use crate::focus_capture::{self, AutoreleasePool};
use wire::Value;

/// How long Kass waits for an app to say its field is ready. Long enough
/// for an app to show a view on its main thread, short enough that the
/// pill never feels late.
pub const READY_TIMEOUT: Duration = Duration::from_millis(150);
/// The Info.plist key an app sets to `true` to take part.
pub const INFO_PLIST_KEY: &str = "KassDictationHandshake";

// ========================================================================
// Decisions
// ========================================================================

/// The `mode` Kass sends with `dictationWillBegin`.
pub fn mode_name(mode: TakeMode) -> &'static str {
    match mode {
        TakeMode::Dictation => "dictate",
        TakeMode::Command => "command",
    }
}

/// How a take ended, as `dictationDidEnd` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The text went in.
    Inserted,
    /// Nothing went in and nothing went wrong: Escape, a take too short to
    /// keep, silence.
    Cancelled,
    Failed,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// The outcome of a take whose pill last showed `last`. Follows the usage
/// report's rule, so the two never disagree on what delivered or failed.
pub fn outcome_of(last: Option<&PillEvent>) -> Outcome {
    match report_outcome(last) {
        Some("delivered") => Outcome::Inserted,
        Some("failed") => Outcome::Failed,
        _ => Outcome::Cancelled,
    }
}

/// A pid from a notification: a real process, not a stray number.
fn pid_from(value: Option<i64>) -> Option<i32> {
    value
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
}

/// What came of waiting for `dictationReady`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    Ready(Duration),
    TimedOut,
}

fn log_line(pid: i32, take_id: u64, waited: Waited) -> String {
    match waited {
        Waited::Ready(after) => format!(
            "[handshake] take {take_id}: pid {pid} ready after {:.1}ms",
            after.as_secs_f64() * 1000.0
        ),
        Waited::TimedOut => format!(
            "[handshake] take {take_id}: pid {pid} sent no dictationReady within {}ms; capturing focus anyway",
            READY_TIMEOUT.as_millis()
        ),
    }
}

/// Who takes part, which takes were announced, and the take waiting for
/// its reply.
#[derive(Default)]
struct Handshakes {
    /// Apps that registered at runtime, by pid.
    registered: HashSet<i32>,
    /// Each app bundle's Info.plist answer, by path.
    bundles: HashMap<String, bool>,
    /// Announced takes and the app each was announced to.
    announced: HashMap<u64, i32>,
    waiting: Option<Waiter>,
}

struct Waiter {
    pid: i32,
    ready: mpsc::Sender<()>,
}

impl Handshakes {
    fn register(&mut self, pid: i32) {
        self.registered.insert(pid);
    }

    /// Whether `pid` takes part. `read_plist` reads the bundle's key, once
    /// per bundle.
    fn opts_in(
        &mut self,
        pid: i32,
        bundle: Option<String>,
        read_plist: impl FnOnce(&str) -> bool,
    ) -> bool {
        if self.registered.contains(&pid) {
            return true;
        }
        let Some(bundle) = bundle else {
            return false;
        };
        if let Some(&cached) = self.bundles.get(&bundle) {
            return cached;
        }
        let opted_in = read_plist(&bundle);
        self.bundles.insert(bundle, opted_in);
        opted_in
    }

    /// Announce `take_id` to `pid`: remember it for `dictationDidEnd` and
    /// wait for the app's reply.
    fn begin(&mut self, take_id: u64, pid: i32) -> mpsc::Receiver<()> {
        let (ready, reply) = mpsc::channel();
        self.announced.insert(take_id, pid);
        self.waiting = Some(Waiter { pid, ready });
        reply
    }

    /// A `dictationReady` from `pid`. Wakes the waiting take when it's the
    /// app that take was announced to.
    fn ready(&mut self, pid: i32) -> bool {
        if self.waiting.as_ref().map(|w| w.pid) != Some(pid) {
            return false;
        }
        let waiter = self.waiting.take().expect("checked above");
        waiter.ready.send(()).is_ok()
    }

    /// Stop waiting: the reply came, or it never will.
    fn stop_waiting(&mut self) {
        self.waiting = None;
    }

    /// The app a finished take was announced to; `None` for a take that
    /// wasn't.
    fn end(&mut self, take_id: u64) -> Option<i32> {
        self.announced.remove(&take_id)
    }
}

// ========================================================================
// Glue
// ========================================================================

fn handshakes() -> MutexGuard<'static, Handshakes> {
    static HANDSHAKES: OnceLock<Mutex<Handshakes>> = OnceLock::new();
    HANDSHAKES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Listen for apps registering and replying. Call once at launch, on the
/// main thread, where replies arrive while a chord's dispatcher waits.
pub fn listen() {
    wire::listen(&[wire::READY, wire::SUPPORTED], on_notification);
}

fn on_notification(name: &str, pid: Option<i64>) {
    let Some(pid) = pid_from(pid) else {
        eprintln!("[handshake] {name} without a pid; ignored");
        return;
    };
    match name {
        wire::READY => {
            if !handshakes().ready(pid) {
                eprintln!("[handshake] late or unexpected dictationReady from pid {pid}");
            }
        }
        wire::SUPPORTED => {
            handshakes().register(pid);
            eprintln!("[handshake] pid {pid} registered");
        }
        _ => {}
    }
}

/// At chord-down, after the microphone opened and before the focus
/// snapshot: if the frontmost app opted in, tell it `take_id` is starting
/// and give it [`READY_TIMEOUT`] to focus its field. Blocking.
pub fn before_focus(take_id: u64, mode: TakeMode) {
    let Some(pid) = focus_capture::frontmost_pid() else {
        return;
    };
    let bundle = focus_capture::app_bundle_path(pid);
    let reply = {
        let mut handshakes = handshakes();
        if !handshakes.opts_in(pid, bundle, info_plist_opts_in) {
            return;
        }
        handshakes.begin(take_id, pid)
    };
    let asked = Instant::now();
    wire::post(
        wire::WILL_BEGIN,
        &[
            ("pid", Value::Int(pid.into())),
            ("mode", Value::Text(mode_name(mode))),
        ],
    );
    let waited = match reply.recv_timeout(READY_TIMEOUT) {
        Ok(()) => Waited::Ready(asked.elapsed()),
        Err(_) => {
            handshakes().stop_waiting();
            Waited::TimedOut
        }
    };
    eprintln!("{}", log_line(pid, take_id, waited));
}

/// When a take is over: tell the app it was announced to how it ended.
/// Nothing for a take that wasn't announced.
pub fn take_ended(take_id: u64, last: Option<&PillEvent>) {
    let Some(pid) = handshakes().end(take_id) else {
        return;
    };
    let outcome = outcome_of(last);
    wire::post(
        wire::DID_END,
        &[
            ("pid", Value::Int(pid.into())),
            ("outcome", Value::Text(outcome.name())),
        ],
    );
    eprintln!(
        "[handshake] take {take_id}: told pid {pid} it ended ({})",
        outcome.name()
    );
}

/// Whether the app bundle at `path` sets [`INFO_PLIST_KEY`] to true.
fn info_plist_opts_in(path: &str) -> bool {
    let (Ok(path), Ok(key)) = (
        std::ffi::CString::new(path),
        std::ffi::CString::new(INFO_PLIST_KEY),
    ) else {
        return false;
    };
    unsafe {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        type Id = *mut Object;

        let _pool = AutoreleasePool::new();
        let path: Id = msg_send![class!(NSString), stringWithUTF8String: path.as_ptr()];
        let bundle: Id = msg_send![class!(NSBundle), bundleWithPath: path];
        if bundle.is_null() {
            return false;
        }
        let key: Id = msg_send![class!(NSString), stringWithUTF8String: key.as_ptr()];
        let value: Id = msg_send![bundle, objectForInfoDictionaryKey: key];
        if value.is_null() {
            return false;
        }
        // A plist boolean is an NSNumber; a string "YES" isn't accepted.
        let is_number: bool = msg_send![value, isKindOfClass: class!(NSNumber)];
        if !is_number {
            return false;
        }
        let on: bool = msg_send![value, boolValue];
        on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: i32 = 4242;
    const BUNDLE: &str = "/Applications/Midna.app";

    fn plist(answer: bool) -> impl FnOnce(&str) -> bool {
        move |_| answer
    }

    #[test]
    fn an_app_that_sets_the_plist_key_opts_in() {
        let mut h = Handshakes::default();
        assert!(h.opts_in(PID, Some(BUNDLE.into()), plist(true)));
    }

    #[test]
    fn an_app_without_the_key_or_a_bundle_does_not() {
        let mut h = Handshakes::default();
        assert!(!h.opts_in(PID, Some(BUNDLE.into()), plist(false)));
        assert!(!h.opts_in(PID + 1, None, plist(true)));
    }

    #[test]
    fn a_bundle_is_read_once() {
        let mut h = Handshakes::default();
        assert!(h.opts_in(PID, Some(BUNDLE.into()), plist(true)));
        // A second process of the same app; the plist isn't read again.
        assert!(h.opts_in(PID + 1, Some(BUNDLE.into()), |_| panic!("read twice")));
    }

    #[test]
    fn a_registered_pid_opts_in_without_a_bundle() {
        let mut h = Handshakes::default();
        h.register(PID);
        assert!(h.opts_in(PID, None, |_| panic!("no plist needed")));
        assert!(!h.opts_in(PID + 1, None, plist(true)));
    }

    #[test]
    fn the_reply_from_the_announced_app_wakes_the_take() {
        let mut h = Handshakes::default();
        let reply = h.begin(1, PID);
        assert!(h.ready(PID));
        assert_eq!(reply.try_recv(), Ok(()));
    }

    #[test]
    fn a_reply_from_another_app_is_ignored() {
        let mut h = Handshakes::default();
        let reply = h.begin(1, PID);
        assert!(!h.ready(PID + 1));
        assert!(reply.try_recv().is_err());
        // The right app can still answer.
        assert!(h.ready(PID));
    }

    #[test]
    fn a_reply_after_the_timeout_wakes_nothing() {
        let mut h = Handshakes::default();
        let _reply = h.begin(1, PID);
        h.stop_waiting();
        assert!(!h.ready(PID));
    }

    #[test]
    fn a_second_reply_wakes_nothing() {
        let mut h = Handshakes::default();
        let _reply = h.begin(1, PID);
        assert!(h.ready(PID));
        assert!(!h.ready(PID));
    }

    #[test]
    fn only_an_announced_take_ends_with_a_notification() {
        let mut h = Handshakes::default();
        let _reply = h.begin(7, PID);
        assert_eq!(h.end(8), None);
        assert_eq!(h.end(7), Some(PID));
        // Once.
        assert_eq!(h.end(7), None);
    }

    #[test]
    fn a_take_ends_even_when_its_app_never_replied() {
        let mut h = Handshakes::default();
        let _reply = h.begin(7, PID);
        h.stop_waiting();
        assert_eq!(h.end(7), Some(PID));
    }

    #[test]
    fn modes_have_their_protocol_names() {
        assert_eq!(mode_name(TakeMode::Dictation), "dictate");
        assert_eq!(mode_name(TakeMode::Command), "command");
    }

    #[test]
    fn outcomes_follow_the_last_pill_state() {
        assert_eq!(outcome_of(Some(&PillEvent::Done)), Outcome::Inserted);
        assert_eq!(outcome_of(Some(&PillEvent::Cancelled)), Outcome::Cancelled);
        assert_eq!(
            outcome_of(Some(&PillEvent::error("Couldn't paste"))),
            Outcome::Failed
        );
        // Too short to keep, or a notice: nothing went in, nothing failed.
        assert_eq!(
            outcome_of(Some(&PillEvent::notice("Still downloading"))),
            Outcome::Cancelled
        );
        assert_eq!(outcome_of(None), Outcome::Cancelled);
        assert_eq!(Outcome::Inserted.name(), "inserted");
        assert_eq!(Outcome::Cancelled.name(), "cancelled");
        assert_eq!(Outcome::Failed.name(), "failed");
    }

    #[test]
    fn only_a_real_pid_is_accepted() {
        assert_eq!(pid_from(Some(4242)), Some(4242));
        assert_eq!(pid_from(Some(0)), None);
        assert_eq!(pid_from(Some(-1)), None);
        assert_eq!(pid_from(Some(i64::MAX)), None);
        assert_eq!(pid_from(None), None);
    }

    /// A bundle at a fresh path whose Info.plist holds `entry`.
    fn bundle_with(name: &str, entry: &str) -> String {
        let dir = std::env::temp_dir()
            .join(format!("kass-handshake-{}-{name}", std::process::id()))
            .join("Test.app");
        let contents = dir.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.example.{name}</string>
{entry}
</dict></plist>"#
        );
        std::fs::write(contents.join("Info.plist"), plist).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn the_info_plist_key_is_read_from_the_bundle() {
        let on = bundle_with("on", "<key>KassDictationHandshake</key><true/>");
        let off = bundle_with("off", "<key>KassDictationHandshake</key><false/>");
        let text = bundle_with(
            "text",
            "<key>KassDictationHandshake</key><string>YES</string>",
        );
        let missing = bundle_with("missing", "");
        assert!(info_plist_opts_in(&on));
        assert!(!info_plist_opts_in(&off));
        assert!(!info_plist_opts_in(&text));
        assert!(!info_plist_opts_in(&missing));
        assert!(!info_plist_opts_in("/nonexistent/Nothing.app"));
    }

    #[test]
    fn a_timeout_is_logged_with_the_wait() {
        assert_eq!(
            log_line(PID, 3, Waited::TimedOut),
            "[handshake] take 3: pid 4242 sent no dictationReady within 150ms; capturing focus anyway"
        );
        assert_eq!(
            log_line(PID, 3, Waited::Ready(Duration::from_micros(2500))),
            "[handshake] take 3: pid 4242 ready after 2.5ms"
        );
    }
}
