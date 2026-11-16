//! What Kass does at launch without any window open, once done by the
//! main window's hidden page: start this Mac's server, and keep the
//! shortcut keys, the dictation gate and the sound cues in step with the
//! saved settings.

use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::{div, px, AnyElement, App, Entity, Global, Subscription};
use kass_api::types::{CaptureReadinessResponse, CaptureSettings};
use kass_api::LOCAL_SERVER_URL;
use kass_ui::{metrics, theme, Button};

use super::data::ShellData;
use crate::host::{self, AppHandle, Manager};
use crate::{hotkey_monitor, prefs, sound_cues, HotkeyState, ServerState};

/// How long a server already on the port (one started by hand, say) gets
/// to answer as Kass before the start counts as failed.
const PORT_TAKEN_WAIT: Duration = Duration::from_secs(120);
const PORT_TAKEN_POLL: Duration = Duration::from_secs(2);

/// What the settings ask of the shortcut keys.
#[derive(Clone, Debug, PartialEq)]
enum Keys {
    Off,
    On {
        push: Vec<String>,
        toggle: Vec<String>,
        command: Vec<String>,
    },
}

/// What was last applied, so a poll with the same answers changes nothing.
#[derive(Default)]
struct Applied {
    keys: Option<Keys>,
    gate: Option<Option<String>>,
    cues: Option<(bool, f64)>,
}

struct LaunchGlobal {
    applied: Applied,
    _settings: Subscription,
}

impl Global for LaunchGlobal {}

/// Follow the shell's settings from now on, and start the server.
pub fn init(data: &Entity<ShellData>, cx: &mut App) {
    let settings = cx.observe(data, |data, cx| follow(&data, cx));
    cx.set_global(LaunchGlobal {
        applied: Applied::default(),
        _settings: settings,
    });
    start_server();
}

/// Onboarding hands the keys back: apply the settings again, even while
/// its window is still on its way out.
pub fn resume(cx: &mut App) {
    forget_applied(cx);
    if let Some(data) = super::try_shell_data(cx) {
        apply(&data, cx);
    }
}

fn follow(data: &Entity<ShellData>, cx: &mut App) {
    // The onboarding window owns the chord and the gate while it's open.
    if crate::onboarding_window_open(AppHandle) {
        forget_applied(cx);
        return;
    }
    apply(data, cx);
}

fn apply(data: &Entity<ShellData>, cx: &mut App) {
    let data = data.read(cx);
    let settings = data.settings.clone();
    let keys = keys(settings.as_ref(), data.input_monitoring);
    let gate = gate(settings.as_ref(), data.readiness.as_ref());
    if !cx.has_global::<LaunchGlobal>() {
        return;
    }
    let applied = &mut cx.global_mut::<LaunchGlobal>().applied;

    if let Some(keys) = keys {
        if applied.keys.as_ref() != Some(&keys) {
            if let Err(e) = arm(&keys) {
                eprintln!("[launch] the shortcut keys could not be set: {e}");
            }
            applied.keys = Some(keys);
        }
    }
    if applied.gate.as_ref() != Some(&gate) {
        hotkey_monitor::set_dictation_gate(AppHandle.state(), gate.clone());
        applied.gate = Some(gate);
    }
    if let Some(settings) = settings {
        let cues = (settings.sound_cues, settings.sound_cue_volume);
        if applied.cues != Some(cues) {
            sound_cues::configure_sound_cues(Some(cues.0), Some(cues.1 as f32));
            applied.cues = Some(cues);
        }
    }
}

fn forget_applied(cx: &mut App) {
    if cx.has_global::<LaunchGlobal>() {
        cx.global_mut::<LaunchGlobal>().applied = Applied::default();
    }
}

/// The keys to arm: none until settings load, and only with Input
/// Monitoring granted, so its prompt never shows for someone who hasn't
/// turned the keys on.
fn keys(settings: Option<&CaptureSettings>, input_monitoring: bool) -> Option<Keys> {
    let settings = settings?;
    Some(if settings.hotkey_enabled && input_monitoring {
        Keys::On {
            push: settings.chord_push_to_talk_keys.clone(),
            toggle: settings.chord_toggle_to_talk_keys.clone(),
            command: settings.chord_command_keys.clone(),
        }
    } else {
        Keys::Off
    })
}

/// While a model dictation needs is missing, a chord shows "Still
/// downloading" in the pill instead of recording into nowhere.
fn gate(
    settings: Option<&CaptureSettings>,
    readiness: Option<&CaptureReadinessResponse>,
) -> Option<String> {
    let refine = settings.is_none_or(|s| s.auto_refine);
    let ready = readiness.is_some_and(|r| r.stt.ready && (!refine || r.llm.ready));
    (!ready).then(|| kass_i18n::t("dictation-still-downloading"))
}

fn arm(keys: &Keys) -> Result<(), String> {
    let state = AppHandle.state::<HotkeyState>();
    match keys {
        Keys::Off => crate::disable_hotkey(state),
        Keys::On {
            push,
            toggle,
            command,
        } => crate::enable_hotkey(
            AppHandle,
            state,
            push.clone(),
            toggle.clone(),
            Some(command.clone()),
        ),
    }
}

/// Start this Mac's server, in its saved models folder. While another
/// Mac's is in use, this one starts too, to stand in when that one is
/// away. A development build leaves it to be run by hand.
pub fn start_server() {
    if cfg!(debug_assertions) {
        eprintln!("[launch] development build: run the server separately");
        return;
    }
    set_failure(None);
    host::async_runtime::spawn(async {
        let dir = prefs::get().custom_models_dir;
        let state = AppHandle.state::<ServerState>();
        let Err(e) = crate::start_server(AppHandle, state, dir).await else {
            return;
        };
        eprintln!("[launch] the server didn't start: {e}");
        if prefs::get().remote_server_url.is_some() {
            return;
        }
        if port_taken(&e) && answers_in_time().await {
            return;
        }
        set_failure(Some(e));
    });
}

/// The port is held, maybe by a server started by hand.
fn port_taken(error: &str) -> bool {
    ["already in use", "port", "EADDRINUSE"]
        .iter()
        .any(|needle| error.contains(needle))
}

async fn answers_in_time() -> bool {
    let until = Instant::now() + PORT_TAKEN_WAIT;
    while Instant::now() < until {
        let up = host::async_runtime::spawn_blocking(|| {
            kass_api::answers_as_kass(LOCAL_SERVER_URL, PORT_TAKEN_POLL)
        })
        .await
        .unwrap_or(false);
        if up {
            return true;
        }
        tokio::time::sleep(PORT_TAKEN_POLL).await;
    }
    false
}

fn set_failure(error: Option<String>) {
    host::with_app(move |cx| {
        if let Some(data) = super::try_shell_data(cx) {
            data.update(cx, |data, cx| {
                data.startup_error = error;
                cx.notify();
            });
        }
    });
}

/// In place of the screen while the server couldn't start: why, and a
/// way to try again.
pub fn failure(error: &str, cx: &App) -> AnyElement {
    let t = theme(cx);
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .p(px(24.))
        .child(
            div()
                .text_size(metrics::TEXT_LG)
                .text_color(t.destructive)
                .child(kass_i18n::t("common-server-failed")),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_size(metrics::TEXT_SM)
                .text_color(t.muted_foreground)
                .child(error.to_string()),
        )
        .child(
            Button::new("server-retry")
                .label(kass_i18n::t("common-server-retry"))
                .on_click(|_, _, _| start_server()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::ModelReadiness;

    fn settings() -> CaptureSettings {
        serde_json::from_str(include_str!(
            "../../../../crates/kass-api/tests/fixtures/capture_settings.json"
        ))
        .unwrap()
    }

    fn model(ready: bool) -> ModelReadiness {
        ModelReadiness {
            ready,
            model_name: String::new(),
            display_name: String::new(),
            size: String::new(),
            size_mb: None,
        }
    }

    fn readiness(stt: bool, llm: bool) -> CaptureReadinessResponse {
        CaptureReadinessResponse {
            stt: model(stt),
            llm: model(llm),
        }
    }

    #[test]
    fn port_errors_wait_for_a_server_already_there() {
        assert!(port_taken(
            "Port 17493 is already in use by another application"
        ));
        assert!(!port_taken("The model worker exited"));
    }

    #[test]
    fn keys_wait_for_settings_and_input_monitoring() {
        let mut on = settings();
        on.hotkey_enabled = true;
        assert_eq!(keys(None, true), None);
        assert_eq!(keys(Some(&on), false), Some(Keys::Off));
        assert_eq!(
            keys(Some(&on), true),
            Some(Keys::On {
                push: on.chord_push_to_talk_keys.clone(),
                toggle: on.chord_toggle_to_talk_keys.clone(),
                command: on.chord_command_keys.clone(),
            })
        );
        on.hotkey_enabled = false;
        assert_eq!(keys(Some(&on), true), Some(Keys::Off));
    }

    #[test]
    fn the_gate_holds_until_the_models_dictation_needs_are_ready() {
        let mut s = settings();
        s.auto_refine = true;
        assert!(gate(Some(&s), None).is_some());
        assert!(gate(Some(&s), Some(&readiness(true, false))).is_some());
        assert_eq!(gate(Some(&s), Some(&readiness(true, true))), None);
        s.auto_refine = false;
        assert_eq!(gate(Some(&s), Some(&readiness(true, false))), None);
        assert!(gate(Some(&s), Some(&readiness(false, true))).is_some());
    }
}
