//! Native dictation: microphone capture and streaming, owned by Rust.
//!
//! On chord start the hotkey monitor calls [`start`]: the microphone opens on
//! its own thread and the `/captures/stream` socket connects in parallel,
//! with audio buffered until the server is ready. Chord end calls [`stop`],
//! which flushes the last frame and sends `finish`. The final capture is
//! pasted through `paste_final_text` into the target focused at chord start,
//! unless provisional text was already written into it live (see [`live`]).
//! The dictate webview only renders the pill from `dictation:state` events.
//!
//! A command take ([`TakeMode::Command`], docs/plans/COMMAND_MODE.md) is the
//! same take with an instruction for selected text: the selection is read
//! while the user speaks ([`command`]) and the rewrite replaces it.
//!
//! A dictation take that opens with "fix that" changes the last take
//! instead of pasting (docs/plans/VOICE_EDITS.md, [`last_take`]).
//!
//! Escape cancels a take until its text starts going in ([`cancel()`]).

pub mod audio;
pub mod cancel;
pub mod capture;
pub mod client;
pub mod command;
pub mod corrections;
pub mod delivery;
pub mod device_kind;
pub mod http;
pub mod last_take;
pub mod live;
pub mod mic;
pub mod paste_command;
pub mod protocol;
pub mod stream;
pub mod take;
pub mod transport;

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::{AppHandle, Emitter, Listener, Manager};

use crate::focus_capture::FocusSnapshot;
use crate::DICTATE_WINDOW_LABEL;
use cancel::{CancelSwitch, Takes};
use capture::{CaptureHooks, NativeInputDevice};
use client::StreamClient;
use live::{Finish, Live, LiveTarget};
use paste_command::Plan;
use protocol::TargetApp;
use stream::{AudioMsg, Recovery, Timeouts};
use take::{PillEvent, TakeEnv};

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:17493";
/// Audio held while the socket connects: one minute at 48 kHz, matching the
/// server's own pending-audio bound. Beyond it the take uses the batch path.
const MAX_PENDING_BYTES: usize = 48_000 * 2 * 60;
const LEARNING_PAUSE_INTERVAL: Duration = Duration::from_secs(30);
/// Label of Kass's main window (Tauri's default for the configured one).
const MAIN_WINDOW_LABEL: &str = "main";
/// How long the main window has to report an in-app insertion.
const IN_APP_INSERT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a Kass window has to report its selection for a command take.
const IN_APP_SELECTION_TIMEOUT: Duration = Duration::from_secs(1);

/// Where and how to capture. Pushed by the dictate webview, which owns the
/// server URL and capture settings.
#[derive(Debug, Clone)]
struct Config {
    server_url: String,
    /// The webview's origin, sent only to a non-loopback server.
    origin: Option<String>,
    input_device_id: Option<String>,
    /// Live text (docs/plans/STREAMING_INSERTION.md): type cleaned text into
    /// the app while cleanup is still writing it. The "Show text as it's
    /// written" setting; off by default.
    live_text: bool,
    /// Voice edits (docs/plans/VOICE_EDITS.md): a take that opens with "fix
    /// that" changes the last take. The "Voice edits" setting; on by default.
    voice_edits: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: DEFAULT_SERVER_URL.to_string(),
            origin: None,
            input_device_id: None,
            live_text: false,
            voice_edits: true,
        }
    }
}

struct ActiveTake {
    id: u64,
    origin: TakeOrigin,
    mode: TakeMode,
    /// The take's audio channel, for a command take's selection reader.
    audio: tokio::sync::mpsc::UnboundedSender<AudioMsg>,
    /// A command take's selection, once read.
    selection: Arc<Mutex<Option<String>>>,
    stop: std_mpsc::Sender<()>,
    focus: Arc<Mutex<Option<FocusSnapshot>>>,
    /// The focused field's text before the caret, read after key-down.
    field_before: Arc<Mutex<Option<String>>>,
    /// The text a voice edit may change, read after key-down; `None` with
    /// voice edits off.
    editable: Option<Arc<Mutex<Option<last_take::Editable>>>>,
    /// Key-up time, for the release-to-final log.
    released: Arc<OnceLock<Instant>>,
}

#[derive(Default)]
pub struct DictationState {
    config: Mutex<Config>,
    /// Where the chosen microphone is remembered between launches.
    device_file: OnceLock<std::path::PathBuf>,
    active: Mutex<Takes<ActiveTake>>,
    next_take: AtomicU64,
    http: OnceLock<reqwest::Client>,
    /// The onboarding window's microphone test ([`mic`]).
    preview: Mutex<Option<std_mpsc::Sender<()>>>,
}

impl DictationState {
    fn http(&self) -> reqwest::Client {
        self.http.get_or_init(http::client).clone()
    }

    /// A new id for the pill's events, shared with takes so they never clash.
    pub fn next_take_id(&self) -> u64 {
        self.next_take.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The server URL and HTTP client takes use.
    pub fn server(&self) -> (String, reqwest::Client) {
        let url = self
            .config
            .lock()
            .map(|c| c.server_url.clone())
            .unwrap_or_else(|_| DEFAULT_SERVER_URL.to_string());
        (url, self.http())
    }
}

/// Where a take was started, which decides where its text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeOrigin {
    /// The global shortcut: paste into the app focused at chord start.
    Shortcut,
    /// Kass's own Dictate button: the text stays in Captures.
    App,
}

/// What a take's words are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeMode {
    /// Text to insert.
    Dictation,
    /// An instruction for the text selected in the target app, which the
    /// rewrite replaces (docs/plans/COMMAND_MODE.md).
    Command,
}

/// Begin a take at chord start. `keydown` is the chord's event time, used for
/// latency logging. Returns the take id, or `None` if one is already recording.
pub fn start(app: &AppHandle, keydown: Instant, origin: TakeOrigin, mode: TakeMode) -> Option<u64> {
    let state = app.state::<DictationState>();
    let mut active = state.active.lock().ok()?;
    if active.is_recording() {
        return None;
    }
    // The take needs the microphone the test was showing.
    mic::stop_preview(&state);
    let config = state.config.lock().map(|c| c.clone()).unwrap_or_default();
    // Live text writes a dictation as it is cleaned up; a command's result
    // replaces the selection once, when it is complete.
    let live_text = config.live_text && mode == TakeMode::Dictation;
    // Only a dictation may be an edit; a command already has its selection.
    // Off, nothing is tracked, sent or applied.
    let voice_edits = config.voice_edits && mode == TakeMode::Dictation;
    let editable = voice_edits.then(|| Arc::new(Mutex::new(None)));
    let take_id = state.next_take_id();
    let selection: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let focus: Arc<Mutex<Option<FocusSnapshot>>> = Arc::new(Mutex::new(None));
    let field_before: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let released: Arc<OnceLock<Instant>> = Arc::new(OnceLock::new());
    let cancel = CancelSwitch::new();
    let written: Arc<Mutex<Option<crate::text_insert::Owned>>> = Arc::new(Mutex::new(None));
    let live = Live::new(AxLive {
        take_id,
        focus: focus.clone(),
        released: released.clone(),
        cancel: cancel.clone(),
        written: written.clone(),
    });
    let env = AppEnv {
        app: app.clone(),
        take_id,
        server_url: config.server_url.clone(),
        http: state.http(),
        focus: focus.clone(),
        clipboard: Arc::new(Mutex::new(None)),
        pastes: origin == TakeOrigin::Shortcut,
        live: live.clone(),
        mode,
        selection: selection.clone(),
        cancel: cancel.clone(),
        voice_edits,
        editable: editable.clone(),
        capture_id: Arc::new(Mutex::new(None)),
        written,
        last_state: Arc::new(Mutex::new(None)),
    };
    env.emit(PillEvent::Preparing);

    // The microphone first: nothing else may delay the first sample.
    let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel::<AudioMsg>();
    let recording = Arc::new(AtomicBool::new(true));
    let hooks = {
        let heard_env = env.clone();
        let level_env = env.clone();
        let stopped_env = env.clone();
        let error_env = env.clone();
        let stopped_flag = recording.clone();
        let error_flag = recording.clone();
        CaptureHooks {
            on_heard: Box::new(move || heard_env.emit(PillEvent::Recording)),
            on_level: Box::new(move |db| level_env.emit_level(db)),
            on_stopped: Box::new(move |duration| {
                stopped_flag.store(false, Ordering::Relaxed);
                stopped_env.emit(PillEvent::Transcribing {
                    elapsed_ms: duration.as_millis() as u64,
                });
            }),
            on_error: Box::new(move |message| {
                error_flag.store(false, Ordering::Relaxed);
                error_env.emit(PillEvent::error(message));
            }),
        }
    };
    let audio = audio_tx.clone();
    let capture = capture::spawn(config.input_device_id.clone(), keydown, audio_tx, hooks);
    // Right behind the microphone thread, never ahead of it. Only a message
    // to the player thread, so it costs the take nothing.
    let start_cue = crate::sound_cues::play(crate::sound_cues::Cue::Start);

    // Save the clipboard while the user speaks. Reading it can take seconds
    // when the copying app renders its data lazily.
    if env.pastes {
        let clipboard_slot = env.clipboard.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Ok(snapshot) = crate::clipboard::save_clipboard() {
                if let Ok(mut slot) = clipboard_slot.lock() {
                    *slot = Some(snapshot);
                }
            }
        });
    }
    let stop = capture.stopper();
    let done = capture.done;

    let (out_tx, in_rx) = transport::spawn(&config.server_url, config.origin.clone());
    let released_for_task = released.clone();
    let field_before_task = field_before.clone();

    let learning_env = env.clone();
    let learning_flag = recording.clone();
    tauri::async_runtime::spawn(async move {
        while learning_flag.load(Ordering::Relaxed) {
            http::pause_learning(&learning_env.http, &learning_env.server_url).await;
            tokio::time::sleep(LEARNING_PAUSE_INTERVAL).await;
        }
    });

    tauri::async_runtime::spawn(async move {
        let app_focus = env.focus.clone();
        let client = StreamClient::new(MAX_PENDING_BYTES)
            .with_target_app(move || target_app(&app_focus))
            .with_field_before(move || field_before_task.lock().ok()?.clone())
            .with_start_cue(if start_cue {
                crate::sound_cues::START_CUE_SPAN_MS
            } else {
                0
            });
        let style_env = env.clone();
        let client = client.with_style(
            crate::sound_cues::STYLE_CUE_DELAY_MS,
            crate::sound_cues::STYLE_CUE_SPAN_MS,
            move |from, to| style_env.emit_style(from, to),
        );
        let client = match env.mode {
            TakeMode::Dictation => client,
            TakeMode::Command => client.with_command(),
        };
        let client = match env.editable.clone() {
            // Sent once read from the app the take goes to.
            Some(editable) => {
                let edit_env = env.clone();
                client
                    .with_last_take(move || {
                        let editable = editable.lock().ok()?.clone()?;
                        Some(client::LastTakeText {
                            text: editable.owned.text.clone(),
                            capture_id: editable.capture_id(),
                            own_chars: editable.own_chars(),
                        })
                    })
                    .with_edit(
                        crate::sound_cues::EDIT_CUE_DELAY_MS,
                        crate::sound_cues::STYLE_CUE_SPAN_MS,
                        move || edit_env.emit_edit(),
                    )
            }
            None => client,
        };
        let client = if live_text {
            let offer = live.clone();
            client.with_provisional(move |text| {
                // The clipboard is only read for the final text.
                if !paste_command::contains_marker(&text) {
                    offer.offer(text);
                }
            })
        } else {
            client
        };
        let escape = env.cancel.cancelled();
        let outcome =
            stream::drive(client, out_tx, in_rx, audio_rx, Timeouts::default(), escape).await;
        let since_release = |at: &OnceLock<Instant>| {
            at.get()
                .map(|t| format!("{:.0}ms", t.elapsed().as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "n/a".into())
        };
        eprintln!(
            "[dictation] take {take_id}: {} release→outcome {}",
            outcome_label(&outcome),
            since_release(&released_for_task)
        );
        if matches!(outcome, client::Outcome::Declined(_)) {
            // Declined while the chord is still held: stop listening now.
            stop_matching(&env.app, |take| take.id == take_id);
        }
        let recorded = async move { done.await.ok().flatten() };
        take::settle(&env, outcome, recorded).await;
        // A take that ended without a paste must not leave live text behind.
        if let Finish::Done(result) = live.finish(None).await {
            eprintln!("[dictation] take {take_id}: live text withdrawn: {result:?}");
        }
        eprintln!(
            "[dictation] take {take_id}: release→delivered {}",
            since_release(&released_for_task)
        );
        report_take(&env, released_for_task.get().map(Instant::elapsed));
        let last = env.last_state.lock().ok().and_then(|l| l.clone());
        crate::dictation_handshake::take_ended(take_id, last.as_ref());
        recording.store(false, Ordering::Relaxed);
        // Settled: Escape has nothing left to cancel here.
        if let Ok(mut takes) = env.app.state::<DictationState>().active.lock() {
            takes.end(take_id);
        }
    });

    active.begin(
        take_id,
        cancel,
        ActiveTake {
            id: take_id,
            origin,
            mode,
            audio,
            selection,
            stop,
            focus,
            field_before,
            editable,
            released,
        },
    );
    Some(take_id)
}

/// The app a take went to, for its capture. None before focus is known.
/// Its category is read from the app bundle once per app, off the
/// microphone's path.
fn target_app(focus: &Mutex<Option<FocusSnapshot>>) -> Option<TargetApp> {
    let focus = focus.lock().ok()?.clone()?;
    let category = focus
        .bundle_id
        .as_deref()
        .and_then(crate::app_icon::app_category);
    Some(TargetApp {
        bundle_id: focus.bundle_id,
        name: focus.app_name,
        category,
    })
}

/// The Kass window a take aimed at Kass goes to: the focused one of
/// the main and onboarding windows, else the main window.
fn kass_window_label(app: &AppHandle) -> String {
    let windows: Vec<(String, bool)> = app
        .webview_windows()
        .into_iter()
        .map(|(label, window)| (label, window.is_focused().unwrap_or(false)))
        .collect();
    pick_kass_window(&windows).to_string()
}

fn pick_kass_window(windows: &[(String, bool)]) -> &str {
    windows
        .iter()
        .find(|(label, focused)| {
            *focused && (label == MAIN_WINDOW_LABEL || label == crate::ONBOARDING_WINDOW_LABEL)
        })
        .map(|(label, _)| label.as_str())
        .unwrap_or(MAIN_WINDOW_LABEL)
}

/// Type `text` into the field focused in Kass's own window, for a
/// shortcut take whose target is Kass (a correction, for example).
/// Synthetic ⌘V and Accessibility insertion are aimed at other apps; the
/// focused Kass window inserts through the DOM instead, so React sees
/// the edit.
async fn insert_in_app(app: &AppHandle, take_id: u64, text: String) -> Result<bool, String> {
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let tx = Mutex::new(Some(tx));
    let listener = app.listen("dictation:inserted", move |event| {
        let Ok(reply) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        if reply.get("take").and_then(Value::as_u64) != Some(take_id) {
            return;
        }
        let inserted = reply.get("inserted").and_then(Value::as_bool) == Some(true);
        if let Some(tx) = tx.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = tx.send(inserted);
        }
    });
    let payload = serde_json::json!({ "take": take_id, "text": text });
    let window = kass_window_label(app);
    let result = match app.emit_to(window.as_str(), "dictation:insert", payload) {
        Ok(()) => Ok(tokio::time::timeout(IN_APP_INSERT_TIMEOUT, rx)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false)),
        Err(e) => Err(format!("Could not reach the Kass window: {e}")),
    };
    app.unlisten(listener);
    result
}

/// Take back live text: the final text is delivered another way.
async fn withdraw_live(live: &Live<AxLive>) {
    if let Finish::Done(result) = live.finish(None).await {
        eprintln!("[dictation] live text withdrawn: {result:?}");
    }
}

/// The clipboard's text, once the user's clipboard is back from the last
/// paste; `None` when what was copied isn't text.
async fn clipboard_text() -> Option<String> {
    let read = tauri::async_runtime::spawn_blocking(|| {
        crate::clipboard::restore_pending();
        crate::clipboard::read_text()
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|read| read);
    read.unwrap_or_else(|e| {
        eprintln!("[dictation] clipboard unreadable: {e}");
        None
    })
}

/// Record the paste target for a take (captured right after [`start`], so
/// the microphone never waits on Accessibility calls).
pub fn set_focus(app: &AppHandle, take_id: u64, focus: Option<FocusSnapshot>) {
    let state = app.state::<DictationState>();
    let Ok(active) = state.active.lock() else {
        return;
    };
    let Some(take) = active.recording().filter(|t| t.id == take_id) else {
        return;
    };
    if take.mode == TakeMode::Command {
        read_selection(app, take, focus.clone());
    } else if let Some(focus) = focus.as_ref() {
        let (pid, bundle_id) = (focus.pid, focus.bundle_id.clone());
        let in_kass = bundle_id.as_deref() == Some(crate::KASS_BUNDLE_ID);
        let field_before = take.field_before.clone();
        let editable = take.editable.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // Turn on an Electron target's accessibility tree now, while the
            // user speaks, so it is built by the time the text is inserted.
            crate::text_insert::wake_electron(pid);
            // Whether the take continues a sentence already in the field
            // (docs/plans/MID_SENTENCE_DICTATION.md).
            if in_kass {
                return;
            }
            let before = crate::text_insert::sentence_before_focused(pid, bundle_id.as_deref());
            if let (Some(before), Ok(mut slot)) = (before, field_before.lock()) {
                *slot = Some(before);
            }
            // What a voice edit may change: the text before the caret,
            // whoever wrote it (docs/plans/VOICE_EDITS.md).
            let Some(editable) = editable else {
                return;
            };
            let near = crate::text_insert::owned_near_focused(pid, bundle_id.as_deref());
            if let Some(near) = near {
                let found = last_take::Editable::new(pid, near, last_take::current());
                if let Ok(mut slot) = editable.lock() {
                    *slot = Some(found);
                }
            }
        });
    }
    if let Ok(mut slot) = take.focus.lock() {
        *slot = focus;
    };
}

/// Read a command take's selection on a blocking thread, while the user
/// speaks, and hand it to the stream (or decline the take).
fn read_selection(app: &AppHandle, take: &ActiveTake, focus: Option<FocusSnapshot>) {
    let audio = take.audio.clone();
    let slot = take.selection.clone();
    let app = app.clone();
    let take_id = take.id;
    tauri::async_runtime::spawn_blocking(move || {
        let found = match focus {
            Some(focus) if focus.bundle_id.as_deref() == Some(crate::KASS_BUNDLE_ID) => {
                selection_in_app(&app, take_id)
            }
            // The pill never takes focus, so the target is still in front.
            Some(focus) => command::read_selection(&focus, true),
            None => Err(delivery::NO_FOCUS_MESSAGE),
        };
        let message = match found {
            Ok(text) => {
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(text.clone());
                }
                AudioMsg::Selection(text)
            }
            Err(message) => AudioMsg::Decline(message.to_string()),
        };
        let _ = audio.send(message);
    });
}

/// Ask the focused Kass window for its selection, for a command take
/// aimed at Kass (onboarding's rewrite step, for example). Blocking.
fn selection_in_app(app: &AppHandle, take_id: u64) -> Result<String, &'static str> {
    let (tx, rx) = std_mpsc::channel::<String>();
    let tx = Mutex::new(tx);
    let listener = app.listen("dictation:selection", move |event| {
        let Ok(reply) = serde_json::from_str::<Value>(event.payload()) else {
            return;
        };
        if reply.get("take").and_then(Value::as_u64) != Some(take_id) {
            return;
        }
        let text = reply
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(text.to_string());
        }
    });
    let window = kass_window_label(app);
    let payload = serde_json::json!({ "take": take_id });
    let reply = match app.emit_to(window.as_str(), "dictation:selection-request", payload) {
        Ok(()) => rx.recv_timeout(IN_APP_SELECTION_TIMEOUT).ok(),
        Err(e) => {
            eprintln!("[command] could not reach the Kass window: {e}");
            None
        }
    };
    app.unlisten(listener);
    command::decide_in_app(reply)
}

/// Show `message` in the pill without recording: a chord pressed while
/// dictation is blocked (docs/plans/ONBOARDING.md).
pub fn show_notice(app: &AppHandle, message: String) {
    let take_id = app.state::<DictationState>().next_take_id();
    show_hud(app);
    send_state(app, take_id, &PillEvent::notice(message));
}

/// End the recording take. Finalization continues in the background, so a
/// new take can start immediately.
pub fn stop(app: &AppHandle) {
    stop_matching(app, |_| true);
}

/// End the take at chord end, but only one the chord started: releasing a
/// chord pressed during an in-app take must not cut that take short.
pub fn stop_shortcut_take(app: &AppHandle) {
    stop_matching(app, |take| take.origin == TakeOrigin::Shortcut);
}

fn stop_matching(app: &AppHandle, matches: impl Fn(&ActiveTake) -> bool) {
    let state = app.state::<DictationState>();
    let taken = state
        .active
        .lock()
        .ok()
        .and_then(|mut takes| takes.release(&matches));
    if let Some(take) = taken {
        take.stop_recording();
    }
}

/// Escape: cancel every take whose text hasn't started going in. The
/// microphone stops, the stream tells the server to discard the take, and
/// the pill hides. Returns whether there was anything to cancel.
pub fn cancel(app: &AppHandle) -> bool {
    let state = app.state::<DictationState>();
    let Ok((cancelled, recording)) = state.active.lock().map(|mut takes| takes.cancel()) else {
        return false;
    };
    // After the switch flipped, so the microphone's own "transcribing" on
    // the way out is never shown.
    if let Some(take) = recording {
        take.stop_recording();
    }
    if cancelled.is_empty() {
        return false;
    }
    if let Some(cue) = PillEvent::Cancelled.cue() {
        crate::sound_cues::play(cue);
    }
    for take_id in &cancelled {
        send_state(app, *take_id, &PillEvent::Cancelled);
    }
    eprintln!("[dictation] cancelled with Escape: takes {cancelled:?}");
    true
}

impl ActiveTake {
    fn stop_recording(&self) {
        let _ = self.released.set(Instant::now());
        let _ = self.stop.send(());
    }
}

/// Tell the server how the take ended, for usage reports. A take that
/// finished without saving anything, like silence, isn't counted.
fn report_take(env: &AppEnv, since_release: Option<Duration>) {
    let last = env.last_state.lock().ok().and_then(|l| l.clone());
    let Some(outcome) = take::report_outcome(last.as_ref()) else {
        return;
    };
    let saved = env
        .capture_id
        .lock()
        .map(|id| id.is_some())
        .unwrap_or(false);
    if outcome == "delivered" && !saved {
        return;
    }
    let mode = match env.mode {
        TakeMode::Dictation => "dictation",
        TakeMode::Command => "command",
    };
    let latency_ms = (outcome == "delivered")
        .then_some(since_release)
        .flatten()
        .map(|d| d.as_millis() as u64);
    let http = env.http.clone();
    let server_url = env.server_url.clone();
    tauri::async_runtime::spawn(async move {
        http::report_take(&http, &server_url, mode, outcome, latency_ms).await;
    });
}

/// Send a take's pill state to every window: the HUD draws it, and the main
/// window's Dictate button follows it.
fn send_state(app: &AppHandle, take_id: u64, event: &PillEvent) {
    let mut payload = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = payload {
        map.insert("take".into(), Value::from(take_id));
    }
    let _ = app.emit("dictation:state", payload);
}

fn outcome_label(outcome: &client::Outcome) -> String {
    match outcome {
        client::Outcome::Final(_) => "final".into(),
        client::Outcome::FailedBeforeFinish(reason) => format!("stream failed ({reason}); batch"),
        client::Outcome::FailedAfterFinish { .. } => {
            "connection lost after finish; recovering".into()
        }
        client::Outcome::Cancelled => "cancelled".into(),
        client::Outcome::Declined(message) => format!("declined ({message})"),
    }
}

#[derive(Clone)]
struct AppEnv {
    app: AppHandle,
    take_id: u64,
    server_url: String,
    http: reqwest::Client,
    focus: Arc<Mutex<Option<FocusSnapshot>>>,
    /// Clipboard saved at key-down so paste needn't read it after release.
    clipboard: Arc<Mutex<Option<crate::clipboard::ClipboardSnapshot>>>,
    /// False for takes started from Kass's own window: nothing to paste into.
    pastes: bool,
    live: Arc<Live<AxLive>>,
    mode: TakeMode,
    /// A command take's selection, once read.
    selection: Arc<Mutex<Option<String>>>,
    /// Flipped by Escape until the text starts going in.
    cancel: Arc<CancelSwitch>,
    /// Whether what this take pastes is kept as the last take, for a voice
    /// edit (docs/plans/VOICE_EDITS.md).
    voice_edits: bool,
    /// The text a voice edit may change, once read ([`ActiveTake`]).
    editable: Option<Arc<Mutex<Option<last_take::Editable>>>>,
    /// The take's capture, once the server saved it.
    capture_id: Arc<Mutex<Option<String>>>,
    /// The live text as it ended, shared with [`AxLive`].
    written: Arc<Mutex<Option<crate::text_insert::Owned>>>,
    /// The last Done or Error shown, for the usage report.
    last_state: Arc<Mutex<Option<PillEvent>>>,
}

impl AppEnv {
    /// Input loudness for the HUD's level bars. Sent apart from
    /// `dictation:state` because it arrives 20 times a second.
    fn emit_level(&self, db: f32) {
        let payload = serde_json::json!({ "take": self.take_id, "db": db });
        let _ = self
            .app
            .emit_to(DICTATE_WINDOW_LABEL, "dictation:level", payload);
    }

    /// The writing style the user asked for by name, for the HUD's style
    /// chip, with its cue as the chip glows. `from` is the style it
    /// replaced, `None` when it was already that. Returns whether the cue
    /// was scheduled; Escape before it plays silences it.
    fn emit_style(&self, from: Option<String>, to: String) -> bool {
        let payload = serde_json::json!({ "take": self.take_id, "from": from, "to": to });
        let _ = self
            .app
            .emit_to(DICTATE_WINDOW_LABEL, "dictation:style", payload);
        let cancel = self.cancel.clone();
        crate::sound_cues::play_after(
            crate::sound_cues::Cue::Style,
            std::time::Duration::from_millis(crate::sound_cues::STYLE_CUE_DELAY_MS.into()),
            move || !cancel.is_cancelled(),
        )
    }

    /// The take opens as a voice edit: its cue, while the user still speaks.
    /// Returns whether the cue was scheduled. The pill doesn't change.
    fn emit_edit(&self) -> bool {
        let cancel = self.cancel.clone();
        crate::sound_cues::play_after(
            crate::sound_cues::Cue::Edit,
            std::time::Duration::from_millis(crate::sound_cues::EDIT_CUE_DELAY_MS.into()),
            move || !cancel.is_cancelled(),
        )
    }

    /// Keep what this take wrote into `pid`'s field as the last take.
    fn remember(&self, pid: i32, owned: crate::text_insert::Owned, typed: bool) {
        let capture_id = self.capture_id.lock().ok().and_then(|id| id.clone());
        last_take::remember(last_take::LastTake {
            pid,
            capture_id,
            owned,
            typed,
        });
    }

    /// Keep `text`, typed or pasted into `pid`'s field, as the last take
    /// once it reads back there, for a correction saved in Captures
    /// (docs/plans/CORRECTIONS_IN_PLACE.md). Keys and ⌘V land a moment
    /// after they are sent, so it is read on its own thread, and kept only
    /// while no newer take has gone in.
    fn remember_typed(&self, pid: i32, bundle_id: Option<String>, text: String) {
        let generation = last_take::generation();
        let capture_id = self.capture_id.clone();
        std::thread::spawn(move || {
            for _ in 0..TYPED_READ_BACK_TRIES {
                std::thread::sleep(TYPED_READ_BACK_INTERVAL);
                if last_take::generation() != generation {
                    return;
                }
                let read =
                    crate::text_insert::owned_before_focused(pid, bundle_id.as_deref(), &text);
                if let Some(owned) = read {
                    let capture_id = capture_id.lock().ok().and_then(|id| id.clone());
                    last_take::remember_if(
                        generation,
                        last_take::LastTake {
                            pid,
                            capture_id,
                            owned,
                            typed: true,
                        },
                    );
                    return;
                }
            }
        });
    }
}

/// How often, and how many times, text that went in by keys or ⌘V is looked
/// for before the caret: about a second in all.
const TYPED_READ_BACK_INTERVAL: Duration = Duration::from_millis(40);
const TYPED_READ_BACK_TRIES: u32 = 25;

/// Live text goes into the target's focused field through Accessibility.
struct AxLive {
    take_id: u64,
    focus: Arc<Mutex<Option<FocusSnapshot>>>,
    released: Arc<OnceLock<Instant>>,
    cancel: Arc<CancelSwitch>,
    /// The text as it ended, once the final text is in place.
    written: Arc<Mutex<Option<crate::text_insert::Owned>>>,
}

impl AxLive {
    fn target(&self) -> Option<(i32, Option<String>)> {
        let focus = self.focus.lock().ok()?.clone()?;
        Some((focus.pid, focus.bundle_id))
    }

    fn log(&self, what: std::fmt::Arguments) {
        let since = self
            .released
            .get()
            .map(|t| format!("{:.0}ms", t.elapsed().as_secs_f64() * 1000.0))
            .unwrap_or_else(|| "n/a".into());
        eprintln!(
            "[dictation] take {}: live {what} release→{since}",
            self.take_id
        );
    }
}

impl LiveTarget for AxLive {
    fn eligible(&self) -> bool {
        // The same preconditions as the paste, plus the target still being in
        // front: live text never activates another app.
        let Some((pid, bundle)) = self.target() else {
            return false;
        };
        bundle.as_deref() != Some(crate::KASS_BUNDLE_ID)
            && crate::accessibility::is_trusted()
            && crate::focus_capture::frontmost_pid() == Some(pid)
            // Last: live text is insertion, so from here Escape does nothing.
            && self.cancel.begin_delivery()
    }

    fn begin(&self, text: String) -> impl Future<Output = crate::text_insert::LiveStart> + Send {
        let target = self.target();
        async move {
            let Some((pid, bundle)) = target else {
                return crate::text_insert::LiveStart::Declined(
                    crate::text_insert::FallbackReason::NoFocusedElement,
                );
            };
            let outcome = tauri::async_runtime::spawn_blocking(move || {
                crate::text_insert::begin_live_focused(pid, bundle.as_deref(), &text)
            })
            .await
            .unwrap_or_else(|e| crate::text_insert::LiveStart::Broken(e.to_string()));
            self.log(format_args!("start {outcome:?}"));
            outcome
        }
    }

    fn extend(
        &self,
        owned: crate::text_insert::Owned,
        text: String,
    ) -> impl Future<Output = Result<crate::text_insert::Owned, crate::text_insert::LiveError>> + Send
    {
        let target = self.target();
        async move {
            let Some((pid, _)) = target else {
                return Err(crate::text_insert::LiveError::Edited);
            };
            let outcome = tauri::async_runtime::spawn_blocking(move || {
                crate::text_insert::extend_live_focused(pid, &owned, &text)
            })
            .await
            .unwrap_or_else(|e| Err(crate::text_insert::LiveError::Uncertain(e.to_string())));
            if let Err(error) = &outcome {
                self.log(format_args!("extend {error:?}"));
            }
            outcome
        }
    }

    fn finish(
        &self,
        owned: crate::text_insert::Owned,
        text: String,
    ) -> impl Future<Output = Result<(), crate::text_insert::LiveError>> + Send {
        let target = self.target();
        async move {
            let Some((pid, _)) = target else {
                return Err(crate::text_insert::LiveError::Edited);
            };
            let revised = !text.starts_with(owned.text.as_str());
            let outcome = tauri::async_runtime::spawn_blocking(move || {
                crate::text_insert::finish_live_focused(pid, &owned, &text)
            })
            .await
            .unwrap_or_else(|e| Err(crate::text_insert::LiveError::Uncertain(e.to_string())));
            self.log(format_args!("final (revised: {revised}) {outcome:?}"));
            outcome.map(|owned| {
                if let Ok(mut written) = self.written.lock() {
                    *written = Some(owned);
                }
            })
        }
    }
}

impl TakeEnv for AppEnv {
    fn emit(&self, event: PillEvent) {
        // Escape hid the pill: nothing the take does afterwards shows or
        // sounds (the microphone closing, a take too short to keep).
        if self.cancel.is_cancelled() {
            return;
        }
        if let Some(cue) = event.cue() {
            crate::sound_cues::play(cue);
        }
        if matches!(event, PillEvent::Done | PillEvent::Error { .. }) {
            if let Ok(mut last) = self.last_state.lock() {
                *last = Some(event.clone());
            }
        }
        send_state(&self.app, self.take_id, &event);
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    fn begin_delivery(&self) -> bool {
        self.cancel.begin_delivery()
    }

    fn discard(&self, capture_id: String) -> impl Future<Output = ()> + Send {
        let http = self.http.clone();
        let server_url = self.server_url.clone();
        let app = self.app.clone();
        async move {
            http::delete_capture(&http, &server_url, &capture_id).await;
            // Drops it from a Captures list that already showed it.
            let _ = app.emit("capture:updated", serde_json::json!({ "id": capture_id }));
        }
    }

    fn capture_created(&self, capture: &Value) {
        if let (Some(id), Ok(mut slot)) = (
            capture.get("id").and_then(Value::as_str),
            self.capture_id.lock(),
        ) {
            *slot = Some(id.to_string());
        }
        let _ = self
            .app
            .emit("capture:created", serde_json::json!({ "capture": capture }));
    }

    fn capture_updated(&self, capture_id: &str) {
        let _ = self
            .app
            .emit("capture:updated", serde_json::json!({ "id": capture_id }));
    }

    fn accessibility_missing(&self) {
        let _ = self.app.emit("system:accessibility-missing", ());
    }

    fn paste(&self, text: String) -> impl Future<Output = Result<bool, String>> + Send {
        let focus = self.focus.lock().ok().and_then(|f| f.clone());
        let prepared = self.clipboard.lock().ok().and_then(|mut c| c.take());
        let pastes = self.pastes;
        let live = self.live.clone();
        let app = self.app.clone();
        let take_id = self.take_id;
        let env = self.clone();
        async move {
            if !pastes {
                // Started from Kass itself: the capture is the result.
                return Ok(true);
            }
            // This take's text is the last take from here, once it is known
            // to be in the field as written.
            last_take::forget();
            let text = match paste_command::plan(&text) {
                Plan::Text(text) => text,
                Plan::Clipboard => {
                    withdraw_live(&live).await;
                    let focus = focus.ok_or_else(|| delivery::NO_FOCUS_MESSAGE.to_string())?;
                    return crate::paste_clipboard_into(focus).await;
                }
                Plan::Around(parts) => match clipboard_text().await {
                    Some(clipboard) => paste_command::fill(&parts, &clipboard),
                    None => {
                        // An image or file: pasted between the words.
                        withdraw_live(&live).await;
                        let focus = focus.ok_or_else(|| delivery::NO_FOCUS_MESSAGE.to_string())?;
                        return crate::paste_around_clipboard(parts, focus).await;
                    }
                },
            };
            // Text already written live is made final in place.
            if let Finish::Done(result) = live.finish(Some(text.clone())).await {
                let written = env.written.lock().ok().and_then(|mut w| w.take());
                if let (true, Some(focus), Some(owned)) = (env.voice_edits, &focus, written) {
                    env.remember(focus.pid, owned, false);
                }
                return result;
            }
            match focus {
                Some(focus) if focus.bundle_id.as_deref() == Some(crate::KASS_BUNDLE_ID) => {
                    insert_in_app(&app, take_id, text).await
                }
                Some(focus) => {
                    let pid = focus.pid;
                    let bundle_id = focus.bundle_id.clone();
                    let (result, tracked) =
                        crate::paste_final_text_tracked(text, focus, prepared, env.voice_edits)
                            .await;
                    match tracked {
                        crate::Tracked::Owned(owned) => env.remember(pid, owned, false),
                        crate::Tracked::Typed(text) => env.remember_typed(pid, bundle_id, text),
                        crate::Tracked::Untracked => {}
                    }
                    result
                }
                None => Err(delivery::NO_FOCUS_MESSAGE.to_string()),
            }
        }
    }

    /// A voice edit of the last take, in the field it is still in.
    fn edit(
        &self,
        before: String,
        after: String,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let focus = self.focus.lock().ok().and_then(|f| f.clone());
        let take_id = self.take_id;
        let editable = self
            .editable
            .as_ref()
            .and_then(|e| e.lock().ok().and_then(|e| e.clone()));
        async move {
            let focus = focus.ok_or_else(|| delivery::NO_FOCUS_MESSAGE.to_string())?;
            let editable = editable
                .filter(|editable| editable.pid == focus.pid)
                .ok_or_else(|| last_take::NOTHING_TO_FIX.to_string())?;
            let owned = editable.owned.clone();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let type_in = |text: &str| {
                    crate::type_over_selection(
                        focus.pid,
                        focus.bundle_id.as_deref(),
                        focus.role.as_deref(),
                        text,
                    )
                };
                crate::text_insert::edit_focused(focus.pid, &owned, &before, &after, &type_in)
            })
            .await
            .unwrap_or_else(|e| Err(crate::text_insert::EditError::Uncertain(e.to_string())));
            eprintln!("[dictation] take {take_id}: voice edit {result:?}");
            last_take::edited(&editable, &result);
            // TODO(voice-edits): the automatic report on the capture that
            // wrote the text is filed by the server (voice_edits.learn_from),
            // which can't know whether this write landed; confirm it here if
            // that ever matters.
            result
                .map(|_| ())
                .map_err(|error| last_take::message(&error))
        }
    }

    fn fetch_result(&self, session_id: String) -> impl Future<Output = Recovery> + Send {
        let http = self.http.clone();
        let server_url = self.server_url.clone();
        async move { http::fetch_result(&http, &server_url, &session_id).await }
    }

    fn upload(&self, wav: Vec<u8>) -> impl Future<Output = Result<Value, String>> + Send {
        let http = self.http.clone();
        let server_url = self.server_url.clone();
        let app = target_app(&self.focus);
        let source = match self.mode {
            TakeMode::Dictation => "dictation",
            TakeMode::Command => "command",
        };
        async move { http::upload(&http, &server_url, wav, source, app).await }
    }

    /// For a command take, the uploaded instruction's rewrite of the selection.
    fn refine(&self, capture_id: String) -> impl Future<Output = Result<Value, String>> + Send {
        let http = self.http.clone();
        let server_url = self.server_url.clone();
        let command = (self.mode == TakeMode::Command)
            .then(|| self.selection.lock().ok().and_then(|s| s.clone()));
        async move {
            match command {
                None => http::refine(&http, &server_url, &capture_id).await,
                Some(None) => Err(command::NO_SELECTION_MESSAGE.to_string()),
                Some(Some(selection)) => {
                    let input = http::CommandInput::Recording {
                        capture_id: &capture_id,
                    };
                    http::run_command(&http, &server_url, &selection, input).await
                }
            }
        }
    }
}

// ========================================================================
// Commands
// ========================================================================

/// Push the server URL, webview origin and saved microphone to Rust.
#[tauri::command]
pub fn dictation_configure(
    state: tauri::State<'_, DictationState>,
    server_url: Option<String>,
    origin: Option<String>,
    input_device_id: Option<String>,
    device_known: Option<bool>,
    live_text: Option<bool>,
    voice_edits: Option<bool>,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    if let Some(live_text) = live_text {
        config.live_text = live_text;
    }
    if let Some(voice_edits) = voice_edits {
        config.voice_edits = voice_edits;
        if !voice_edits {
            last_take::forget();
        }
    }
    config.server_url = server_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SERVER_URL.to_string());
    config.origin = origin.filter(|o| !o.is_empty() && o != "null");
    let known = device_known.unwrap_or(true);
    let device = next_device(
        config.input_device_id.clone(),
        known,
        input_device_id.filter(|id| !id.is_empty()),
    );
    if known && device != config.input_device_id {
        if let Some(path) = state.device_file.get() {
            save_device(path, device.as_deref());
        }
    }
    config.input_device_id = device;
    Ok(())
}

/// Remember the chosen microphone across launches. The webview only learns
/// the setting once the server is up (~30 s after launch); without this the
/// first take after launch used the macOS default input instead.
pub fn restore(app: &AppHandle) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let path = dir.join("dictation-device.txt");
    let state = app.state::<DictationState>();
    if let Ok(mut config) = state.config.lock() {
        config.input_device_id = load_device(&path);
    }
    let _ = state.device_file.set(path);
}

fn load_device(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

fn save_device(path: &std::path::Path, id: Option<&str>) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(path, id.unwrap_or("")) {
        eprintln!("[dictation] could not remember the microphone: {e}");
    }
}

/// The microphone to use after a configure call. Settings that haven't loaded
/// yet say nothing about the device, so the saved choice stands.
fn next_device(current: Option<String>, known: bool, sent: Option<String>) -> Option<String> {
    if known {
        sent
    } else {
        current
    }
}

/// Stop the current take (the pill's stop button).
#[tauri::command]
pub fn dictation_stop(app: AppHandle) {
    stop(&app);
}

/// Start a take from Kass's own Dictate button. The text lands in
/// Captures instead of being pasted. Returns the take id, or `None` when a
/// take is already recording.
#[tauri::command]
pub fn dictation_start(app: AppHandle) -> Option<u64> {
    let take = start(&app, Instant::now(), TakeOrigin::App, TakeMode::Dictation);
    if take.is_some() {
        show_hud(&app);
    }
    take
}

/// Run an instruction, or a transform by name, on the text selected in the
/// app the user came to Kass from (the ⌘K palette's transforms).
#[tauri::command]
pub async fn command_run(app: AppHandle, instruction: String) -> Result<(), String> {
    command::run_from_kass(&app, instruction).await
}

/// Bring the HUD up bottom-center without taking key focus from the app
/// being dictated into.
pub fn show_hud(app: &AppHandle) {
    let Some(window) = app.get_webview_window(DICTATE_WINDOW_LABEL) else {
        return;
    };
    // Restore bottom-center placement after the hide path parks the pill
    // off-screen, then make its controls clickable again.
    if let Err(e) = crate::position_dictate_window(&window) {
        eprintln!("dictate:start: failed to position pill: {e}");
    }
    let _ = window.set_ignore_cursor_events(false);
    // Not `window.show()`: that makes the pill key, and the app the user is
    // dictating into stops receiving keystrokes. This orders it into the
    // active Space (incl. a foreign app's fullscreen Space) without focus.
    crate::force_order_front(&window);
}

/// Native microphones, with ids suitable for `input_device_id`.
#[tauri::command]
pub async fn list_input_devices() -> Result<Vec<NativeInputDevice>, String> {
    tauri::async_runtime::spawn_blocking(capture::list_input_devices)
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod kass_window_tests {
    use super::*;

    fn windows(list: &[(&str, bool)]) -> Vec<(String, bool)> {
        list.iter().map(|(l, f)| (l.to_string(), *f)).collect()
    }

    #[test]
    fn text_goes_to_the_focused_onboarding_window() {
        let list = windows(&[("main", false), ("dictate", false), ("onboarding", true)]);
        assert_eq!(pick_kass_window(&list), "onboarding");
    }

    #[test]
    fn text_goes_to_the_focused_main_window() {
        let list = windows(&[("main", true), ("onboarding", false)]);
        assert_eq!(pick_kass_window(&list), "main");
    }

    #[test]
    fn the_pill_or_no_focus_falls_back_to_the_main_window() {
        assert_eq!(
            pick_kass_window(&windows(&[("dictate", true), ("onboarding", false)])),
            "main"
        );
        assert_eq!(pick_kass_window(&windows(&[])), "main");
    }
}

#[cfg(test)]
mod saved_device_tests {
    use super::*;

    #[test]
    fn a_saved_microphone_is_restored_at_launch() {
        let dir = tempfile_dir();
        let path = dir.join("dictation-device.txt");
        save_device(&path, Some("native:MacBook Pro Microphone"));
        assert_eq!(
            load_device(&path),
            Some("native:MacBook Pro Microphone".to_string())
        );
    }

    #[test]
    fn the_system_default_is_saved_as_no_device() {
        let dir = tempfile_dir();
        let path = dir.join("dictation-device.txt");
        save_device(&path, Some("native:AirPods"));
        save_device(&path, None);
        assert_eq!(load_device(&path), None);
    }

    #[test]
    fn a_missing_file_means_the_system_default() {
        let dir = tempfile_dir();
        assert_eq!(load_device(&dir.join("missing.txt")), None);
    }

    #[test]
    fn settings_that_have_not_loaded_keep_the_saved_microphone() {
        let saved = Some("native:MacBook Pro Microphone".to_string());
        assert_eq!(next_device(saved.clone(), false, None), saved);
        assert_eq!(next_device(saved.clone(), true, None), None);
        assert_eq!(
            next_device(saved, true, Some("native:AirPods".to_string())),
            Some("native:AirPods".to_string())
        );
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kass-device-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
