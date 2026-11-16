//! What onboarding needs from the app around it: the permissions, the global
//! keys, the microphone, dictation and the window itself. The app hands the
//! page an [`OnboardingHost`]; [`NoHost`] runs it with none of them (a
//! preview), where every check says "not yet".

use gpui_kit::{App, Subscription};
use kass_api::types::CaptureResponse;
use std::path::PathBuf;
use std::rc::Rc;

/// Something the app saw that the page follows.
#[derive(Clone, Debug)]
pub enum HostEvent {
    /// A chord went down: `push`, `toggle` or `command`.
    ChordDown(String),
    /// The chord came up.
    ChordUp,
    /// The microphone test's loudness, in dB.
    MicLevel(f64),
    /// The microphone test couldn't open the microphone.
    MicError(String),
    /// A take moved on: `preparing`, `recording`, `transcribing`,
    /// `refining`, `done`, `cancelled` or `error` (with its message).
    Take {
        state: String,
        take: u64,
        message: Option<String>,
    },
    /// A take was saved.
    Capture(Box<CaptureResponse>),
    /// A take's text, to type into this window's focused field.
    Insert { take: u64, text: String },
    /// A command take asks for this window's selected text.
    SelectionRequest { take: u64 },
}

/// Called with each [`HostEvent`].
pub type OnHostEvent = Rc<dyn Fn(HostEvent, &mut App)>;

/// Whether Kass opens at login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStatus {
    Enabled,
    Disabled,
    /// Registered, but turned off in System Settings › Login Items.
    RequiresApproval,
    /// Not the installed app, or macOS is too old.
    Unavailable,
}

/// The microphone permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicPermission {
    Granted,
    Denied,
    Restricted,
    /// macOS hasn't asked yet.
    Undetermined,
}

impl MicPermission {
    pub fn parse(name: &str) -> Self {
        match name {
            "granted" => Self::Granted,
            "denied" => Self::Denied,
            "restricted" => Self::Restricted,
            _ => Self::Undetermined,
        }
    }
}

/// The user's three chords, as key names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chords {
    pub push: Vec<String>,
    pub toggle: Vec<String>,
    pub command: Vec<String>,
}

pub trait OnboardingHost: 'static {
    /// Follows the app's events until the subscription is dropped.
    fn follow(&self, _on_event: OnHostEvent, _cx: &mut App) -> Subscription {
        Subscription::new(|| {})
    }

    /// Where the step reached is kept across a relaunch.
    fn progress_path(&self) -> Option<PathBuf> {
        None
    }

    fn input_monitoring(&self) -> bool {
        false
    }

    fn accessibility(&self) -> bool {
        false
    }

    /// Asks for Input Monitoring, or opens its System Settings pane.
    fn open_input_monitoring_settings(&self) {}

    /// Asks for Accessibility, or opens its System Settings pane.
    fn open_accessibility_settings(&self) {}

    /// Arms the global keys with `chords`, or quiets them with `None`.
    fn arm_hotkey(&self, _chords: Option<&Chords>) {}

    /// While on, chords only send their down and up events.
    fn set_chord_practice(&self, _on: bool) {}

    /// Blocks takes with a message for the pill, or unblocks them.
    fn set_dictation_gate(&self, _blocked: Option<String>) {}

    /// Starts a take from a Dictate button; its id, if it started.
    fn dictation_start(&self) -> Option<u64> {
        None
    }

    fn dictation_stop(&self) {}

    fn microphone_permission(&self) -> MicPermission {
        MicPermission::Undetermined
    }

    /// Opens the microphone (`None` for the system default) and sends its
    /// level. Asks for the permission when macOS hasn't yet.
    fn mic_preview_start(&self, _device: Option<String>) -> Result<(), String> {
        Ok(())
    }

    fn mic_preview_stop(&self) {}

    /// The microphones, as `(id, name)`.
    fn input_devices(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    fn launch_at_login(&self) -> LoginStatus {
        LoginStatus::Unavailable
    }

    /// Turns opening at login on or off; the status after.
    fn set_launch_at_login(&self, _on: bool) -> LoginStatus {
        LoginStatus::Unavailable
    }

    /// Answers a [`HostEvent::Insert`]: whether the text went in.
    fn inserted(&self, _take: u64, _inserted: bool) {}

    /// Answers a [`HostEvent::SelectionRequest`].
    fn selection(&self, _take: u64, _text: String) {}

    /// Closes onboarding and shows the main window, at `show` if given.
    fn finish(&self, _show: Option<String>) {}
}

/// No app around the page.
pub struct NoHost;

impl OnboardingHost for NoHost {}
