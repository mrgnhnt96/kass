//! The dictate window's content in GPUI: the pill follows the take through
//! the `dictation:*` events, and dictation learns from the shell's settings
//! where the server is and which microphone to use.

use std::rc::Rc;

use gpui_kit::{AnyView, App, AppContext, Entity, Global, Subscription, WeakEntity, Window};
use kass_api::LOCAL_SERVER_URL;
use kass_pill::{DictatePill, LevelEvent, PillHost, StateEvent, StyleEvent};
use serde::de::DeserializeOwned;

use crate::dictation::{self, DictationState};
use crate::host::{self, webview, AppHandle, Listener, Manager};
use crate::shell::{self, data::ShellData};
use crate::DICTATE_WINDOW_LABEL;

struct DictateGlobal {
    pill: WeakEntity<DictatePill>,
    configured: Option<Config>,
    _settings: Subscription,
}

impl Global for DictateGlobal {}

/// The panel the pill floats in.
struct Panel;

impl PillHost for Panel {
    fn stop(&self, _: &mut App) {
        dictation::dictation_stop(AppHandle);
    }

    fn hide(&self, _: &mut App) {
        if let Some(window) = AppHandle.get_webview_window(DICTATE_WINDOW_LABEL) {
            crate::park_dictate_window(&window);
        }
    }

    fn chip_space(&self, show: bool, _: &mut App) {
        crate::dictate_chip_space(AppHandle, show);
    }
}

/// What GPUI draws in the dictate window.
pub fn root(_: &mut Window, cx: &mut App) -> AnyView {
    let pill = cx.new(|_| DictatePill::new(Rc::new(Panel)));
    let first = !cx.has_global::<DictateGlobal>();
    let data = shell::shell_data(cx);
    let settings = cx.observe(&data, |data, cx| configure(&data, cx));
    cx.set_global(DictateGlobal {
        pill: pill.downgrade(),
        configured: None,
        _settings: settings,
    });
    configure(&data, cx);
    if first {
        forward::<StateEvent>("dictation:state", |pill, event, cx| {
            pill.on_state(event, cx)
        });
        forward::<LevelEvent>("dictation:level", |pill, event, _| pill.on_level(event));
        forward::<StyleEvent>("dictation:style", |pill, event, cx| {
            pill.on_style(event, cx)
        });
    }
    pill.into()
}

/// Hand each `event` to the pill, on GPUI's thread.
fn forward<E: DeserializeOwned + Send + 'static>(
    event: &'static str,
    apply: fn(&mut DictatePill, E, &mut gpui_kit::Context<DictatePill>),
) {
    AppHandle.listen(event, move |event| {
        let Ok(payload) = serde_json::from_str::<E>(event.payload()) else {
            return;
        };
        host::with_app(move |cx| {
            let Some(pill) = cx
                .try_global::<DictateGlobal>()
                .and_then(|g| g.pill.upgrade())
            else {
                return;
            };
            pill.update(cx, |pill, cx| apply(pill, payload, cx));
        });
    });
}

/// What dictation is told, as the dictate page told it.
#[derive(Clone, Debug, PartialEq)]
struct Config {
    server_url: String,
    fallback_url: Option<String>,
    input_device_id: Option<String>,
    device_known: bool,
    live_text: bool,
    voice_edits: Option<bool>,
}

impl Config {
    fn of(data: &ShellData) -> Self {
        let settings = data.settings.as_ref();
        let server_url = match data.synced.server_url.trim() {
            "" => LOCAL_SERVER_URL.to_string(),
            url => url.to_string(),
        };
        Self {
            server_url,
            // While using another Mac's server, this Mac's own stands by:
            // a take moves there when that one doesn't answer.
            fallback_url: data
                .synced
                .remote_server_url
                .as_ref()
                .map(|_| LOCAL_SERVER_URL.to_string()),
            input_device_id: settings.and_then(|s| s.input_device_id.clone()),
            // Until settings load, say nothing about the microphone so
            // dictation keeps the one it remembered from the last launch.
            device_known: settings.is_some(),
            live_text: settings.is_some_and(|s| s.live_text),
            voice_edits: settings.map(|s| s.voice_edits),
        }
    }
}

fn configure(data: &Entity<ShellData>, cx: &mut App) {
    let config = Config::of(data.read(cx));
    let Some(global) = cx.try_global::<DictateGlobal>() else {
        return;
    };
    if global.configured.as_ref() == Some(&config) {
        return;
    }
    cx.global_mut::<DictateGlobal>().configured = Some(config.clone());
    let app = AppHandle;
    if let Err(e) = dictation::dictation_configure(
        app.state::<DictationState>(),
        Some(config.server_url),
        config.fallback_url,
        Some(webview::page_origin()),
        config.input_device_id,
        Some(config.device_known),
        Some(config.live_text),
        config.voice_edits,
    ) {
        eprintln!("dictate: dictation_configure failed: {e}");
    }
}
