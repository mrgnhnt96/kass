//! The onboarding window's page (crates/kass-onboarding), drawn natively, and
//! what it needs from the app: the permissions, the global keys, the
//! microphone, dictation, and the window itself.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{AnyView, App, AppContext, Subscription, Window};
use kass_api::types::CaptureResponse;
use kass_onboarding::{
    Chords, HostEvent, LoginStatus, MicPermission, OnHostEvent, Onboarding, OnboardingHost,
};
use serde::Deserialize;
use serde_json::json;

use crate::host::{self, AppHandle, Emitter, Event, Listener, Manager};
use crate::{dictation, hotkey_monitor, login_item};

#[derive(Default)]
struct Followers {
    list: Vec<(u64, OnHostEvent)>,
    next: u64,
    listening: bool,
}

thread_local! {
    // GPUI's main thread only: events reach it through `host::with_app`.
    static FOLLOWERS: RefCell<Followers> = RefCell::new(Followers::default());
    // Whether the next page starts over instead of picking up its step.
    static RESTART: Cell<bool> = const { Cell::new(false) };
}

/// The next onboarding page starts from the first step (opened by the user),
/// or picks up the step it was on (the launch after a permission's quit).
pub fn start_over(restart: bool) {
    RESTART.set(restart);
}

/// What GPUI draws in the onboarding window.
pub fn root(window: &mut Window, cx: &mut App) -> AnyView {
    let api = crate::shell::shell_data(cx).read(cx).api.clone();
    let restart = RESTART.replace(false);
    cx.new(|cx| Onboarding::new(Rc::new(AppHost), api, restart, window, cx))
        .into()
}

#[derive(Deserialize)]
struct Chord {
    action: String,
}

#[derive(Deserialize)]
struct Level {
    db: f64,
}

#[derive(Deserialize)]
struct Message {
    message: String,
}

#[derive(Deserialize)]
struct TakeState {
    state: String,
    take: u64,
    message: Option<String>,
}

#[derive(Deserialize)]
struct Created {
    capture: CaptureResponse,
}

#[derive(Deserialize)]
struct Insert {
    take: u64,
    text: String,
}

#[derive(Deserialize)]
struct SelectionRequest {
    take: u64,
}

fn parse<T: for<'de> Deserialize<'de>>(event: &Event) -> Option<T> {
    serde_json::from_str(event.payload()).ok()
}

/// Only what's sent to every window, or to this one.
fn for_onboarding(event: &Event) -> bool {
    event
        .target()
        .is_none_or(|target| target == crate::ONBOARDING_WINDOW_LABEL)
}

fn to_host_event(name: &str, event: &Event) -> Option<HostEvent> {
    if !for_onboarding(event) {
        return None;
    }
    Some(match name {
        "chord:down" => HostEvent::ChordDown(parse::<Chord>(event)?.action),
        "chord:up" => HostEvent::ChordUp,
        "mic:level" => HostEvent::MicLevel(parse::<Level>(event)?.db),
        "mic:error" => HostEvent::MicError(parse::<Message>(event)?.message),
        "dictation:state" => {
            let TakeState {
                state,
                take,
                message,
            } = parse(event)?;
            HostEvent::Take {
                state,
                take,
                message,
            }
        }
        "capture:created" => HostEvent::Capture(Box::new(parse::<Created>(event)?.capture)),
        // Asked of the focused Kass window only, so never of every window.
        "dictation:insert" if event.target().is_some() => {
            let Insert { take, text } = parse(event)?;
            HostEvent::Insert { take, text }
        }
        "dictation:selection-request" if event.target().is_some() => HostEvent::SelectionRequest {
            take: parse::<SelectionRequest>(event)?.take,
        },
        _ => return None,
    })
}

const EVENTS: [&str; 8] = [
    "chord:down",
    "chord:up",
    "mic:level",
    "mic:error",
    "dictation:state",
    "capture:created",
    "dictation:insert",
    "dictation:selection-request",
];

fn send(event: HostEvent, cx: &mut App) {
    let followers: Vec<OnHostEvent> =
        FOLLOWERS.with_borrow(|f| f.list.iter().map(|(_, each)| each.clone()).collect());
    for follower in followers {
        follower(event.clone(), cx);
    }
}

fn login_status(status: login_item::Status) -> LoginStatus {
    match status {
        login_item::Status::Enabled => LoginStatus::Enabled,
        login_item::Status::Disabled => LoginStatus::Disabled,
        login_item::Status::RequiresApproval => LoginStatus::RequiresApproval,
        login_item::Status::Unavailable => LoginStatus::Unavailable,
    }
}

struct AppHost;

impl OnboardingHost for AppHost {
    fn follow(&self, on_event: OnHostEvent, _: &mut App) -> Subscription {
        let id = FOLLOWERS.with_borrow_mut(|f| {
            if !f.listening {
                f.listening = true;
                for name in EVENTS {
                    AppHandle.listen(name, move |event| {
                        if let Some(event) = to_host_event(name, &event) {
                            host::with_app(move |cx| send(event, cx));
                        }
                    });
                }
            }
            f.next += 1;
            f.list.push((f.next, on_event));
            f.next
        });
        Subscription::new(move || {
            FOLLOWERS.with_borrow_mut(|f| f.list.retain(|(each, _)| *each != id));
        })
    }

    fn progress_path(&self) -> Option<PathBuf> {
        AppHandle
            .path()
            .app_data_dir()
            .ok()
            .map(|dir| dir.join("onboarding.json"))
    }

    fn input_monitoring(&self) -> bool {
        crate::input_monitoring::is_trusted()
    }

    fn accessibility(&self) -> bool {
        crate::accessibility::is_trusted()
    }

    fn open_input_monitoring_settings(&self) {
        if let Err(e) = crate::open_input_monitoring_settings() {
            eprintln!("[onboarding] {e}");
        }
    }

    fn open_accessibility_settings(&self) {
        if let Err(e) = crate::open_accessibility_settings(AppHandle) {
            eprintln!("[onboarding] {e}");
        }
    }

    fn arm_hotkey(&self, chords: Option<&Chords>) {
        let state = AppHandle.state::<crate::HotkeyState>();
        let result = match chords {
            Some(chords) => crate::enable_hotkey(
                AppHandle,
                state,
                chords.push.clone(),
                chords.toggle.clone(),
                Some(chords.command.clone()),
            ),
            None => crate::disable_hotkey(state),
        };
        if let Err(e) = result {
            eprintln!("[onboarding] could not set the keys: {e}");
        }
    }

    fn set_chord_practice(&self, on: bool) {
        hotkey_monitor::set_chord_practice(AppHandle.state(), on);
    }

    fn set_dictation_gate(&self, blocked: Option<String>) {
        hotkey_monitor::set_dictation_gate(AppHandle.state(), blocked);
    }

    fn dictation_start(&self) -> Option<u64> {
        dictation::dictation_start(AppHandle)
    }

    fn dictation_stop(&self) {
        dictation::dictation_stop(AppHandle);
    }

    fn microphone_permission(&self) -> MicPermission {
        MicPermission::parse(dictation::mic::microphone_permission())
    }

    fn mic_preview_start(&self, device: Option<String>) -> Result<(), String> {
        dictation::mic::mic_preview_start(AppHandle, device)
    }

    fn mic_preview_stop(&self) {
        dictation::mic::mic_preview_stop(AppHandle);
    }

    fn input_devices(&self) -> Vec<(String, String)> {
        dictation::capture::list_input_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|device| (device.id, device.name))
            .collect()
    }

    fn launch_at_login(&self) -> LoginStatus {
        login_status(login_item::launch_at_login_status())
    }

    fn set_launch_at_login(&self, on: bool) -> LoginStatus {
        match login_item::set_launch_at_login(on) {
            Ok(status) => login_status(status),
            Err(e) => {
                eprintln!("[onboarding] could not set launch at login: {e}");
                self.launch_at_login()
            }
        }
    }

    fn inserted(&self, take: u64, inserted: bool) {
        let _ = AppHandle.emit(
            "dictation:inserted",
            json!({ "take": take, "inserted": inserted }),
        );
    }

    fn selection(&self, take: u64, text: String) {
        let _ = AppHandle.emit("dictation:selection", json!({ "take": take, "text": text }));
    }

    fn finish(&self, show: Option<String>) {
        crate::finish_onboarding(AppHandle, show);
    }
}

#[cfg(test)]
mod tests {
    use super::to_host_event;
    use crate::host::Event;

    #[test]
    fn insert_goes_only_to_the_window_asked() {
        let payload = r#"{"take":3,"text":"hi"}"#;
        let to =
            |target: Option<&str>| to_host_event("dictation:insert", &Event::new(payload, target));
        assert!(to(Some("onboarding")).is_some());
        assert!(to(Some("main")).is_none());
        assert!(to(None).is_none());
    }

    #[test]
    fn chord_names_come_through() {
        let event = Event::new(r#"{"action":"push"}"#, None);
        assert!(matches!(
            to_host_event("chord:down", &event),
            Some(kass_onboarding::HostEvent::ChordDown(action)) if action == "push"
        ));
    }
}
