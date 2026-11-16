//! The onboarding window: one poster per step, the downloads running along
//! the top, and the app's keys, microphone and dictation followed throughout.

use crate::downloads::{Action, Downloads, Inputs, Summary};
use crate::flow::{Progress, RETRY_DELAY_MS, Step};
use crate::host::{Chords, HostEvent, LoginStatus, MicPermission, OnHostEvent, OnboardingHost};
use crate::motion::{self, Frame};
use crate::poster::{self, Poster};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::select::SelectState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_api::types::{
    ActiveDownloadTask, CaptureReadinessResponse, CaptureResponse, CaptureSettings,
    CaptureSettingsUpdate, TeachSession,
};
use kass_i18n::{t, tr};
use kass_ui::{SelectOptions, metrics};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The push-to-talk chord before the user picks one.
pub fn default_push_keys() -> Vec<String> {
    vec!["MetaRight".into(), "AltGr".into()]
}

/// Where a take is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakePhase {
    Idle,
    Listening,
    Working,
}

impl TakePhase {
    /// A `dictation:state` name as a phase.
    pub fn from_state(state: &str) -> Self {
        match state {
            "preparing" | "recording" => Self::Listening,
            "transcribing" | "refining" => Self::Working,
            _ => Self::Idle,
        }
    }
}

/// The color spreading from the last click.
pub(crate) struct Wipe {
    pub from: Hsla,
    pub start: Instant,
    pub origin: Point<Pixels>,
}

#[derive(Default)]
pub(crate) struct Mic {
    pub permission: Option<MicPermission>,
    pub previewing: bool,
    pub db: f64,
    pub error: Option<String>,
}

pub(crate) struct MicSelect {
    pub key: (Option<String>, Vec<(String, String)>),
    pub state: Entity<SelectState<SelectOptions<String>>>,
    pub _subscription: Subscription,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamePhase {
    Ask,
    Confirm,
    Edit,
    Saved,
}

pub(crate) struct NameState {
    pub phase: NamePhase,
    pub heard: String,
    pub name: String,
    pub fixed: bool,
    pub at: Instant,
}

pub(crate) struct MessyState {
    /// What was heard and what was sent.
    pub result: Option<(String, String)>,
    pub at: Instant,
}

pub(crate) struct RewriteState {
    /// The instruction that ran, once the text was rewritten.
    pub done: Option<String>,
    pub busy: bool,
    pub error: Option<String>,
    pub at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StylePhase {
    Reply,
    Check,
    Edit,
    Reveal,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StyleTake {
    pub heard: String,
    pub shown: String,
    pub written: String,
}

pub(crate) struct StyleState {
    pub phase: StylePhase,
    pub session: Option<TeachSession>,
    /// The open conversation and how many replies it got.
    pub open: Option<(String, u32)>,
    pub take: StyleTake,
    pub tried_work: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub at: Instant,
}

pub struct Onboarding {
    pub(crate) host: Rc<dyn OnboardingHost>,
    pub(crate) api: ApiClient,
    pub(crate) progress: Progress,
    /// The last move was back a step.
    pub(crate) back: bool,
    /// When the open screen came in.
    pub(crate) entered: Instant,
    pub(crate) locked: bool,
    pub(crate) wipe: Option<Wipe>,

    pub(crate) settings: Option<CaptureSettings>,
    pub(crate) readiness: Option<CaptureReadinessResponse>,
    pub(crate) tasks: Vec<ActiveDownloadTask>,
    pub(crate) free_mb: Option<f64>,
    pub(crate) input_monitoring: bool,
    pub(crate) accessibility: bool,
    pub(crate) downloads: Downloads,
    pub(crate) failure_details: bool,

    armed: Option<Option<Chords>>,
    gate: Option<Option<String>>,
    practice: Option<bool>,
    enabling_hotkey: bool,
    keys_advancing: bool,

    pub(crate) held: Option<String>,
    pub(crate) held_at: Instant,
    pub(crate) pressed_once: bool,
    pub(crate) pressed_at: Instant,
    pub(crate) take: TakePhase,
    pub(crate) take_at: Instant,
    pub(crate) take_error: Option<String>,

    pub(crate) trying: bool,
    pub(crate) mic: Mic,
    pub(crate) mic_select: Option<MicSelect>,
    pub(crate) devices: Vec<(String, String)>,
    pub(crate) login: LoginStatus,
    pub(crate) name: NameState,
    pub(crate) messy: MessyState,
    pub(crate) rewrite: RewriteState,
    pub(crate) style: StyleState,

    pub(crate) name_field: Entity<InputState>,
    pub(crate) messy_field: Entity<TextareaState>,
    pub(crate) reply_field: Entity<InputState>,
    pub(crate) edit_field: Entity<TextareaState>,
    pub(crate) rewrite_field: Entity<TextareaState>,

    _subscriptions: Vec<Subscription>,
    _follow: Option<Subscription>,
}

impl Onboarding {
    pub fn new(
        host: Rc<dyn OnboardingHost>,
        api: ApiClient,
        restart: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut progress = host
            .progress_path()
            .map(|p| Progress::load(&p))
            .unwrap_or_default();
        if restart {
            progress = progress.restarted();
        }
        let now = Instant::now();
        let name_field =
            cx.new(|cx| InputState::new(window, cx).placeholder(t("onboarding-name-placeholder")));
        let messy_field = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(2)
                .placeholder(t("onboarding-messy-field-placeholder"))
        });
        let reply_field = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("onboarding-style-field-placeholder"))
        });
        let edit_field = cx.new(|cx| TextareaState::new(window, cx).rows(2));
        let rewrite_field = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .default_value(t("onboarding-rewrite-paragraph"))
        });
        let subscriptions = vec![
            cx.subscribe_in(&name_field, window, Self::name_input),
            cx.subscribe_in(&reply_field, window, Self::reply_input),
            cx.subscribe_in(&edit_field, window, Self::edit_input),
        ];
        let login = host.launch_at_login();
        let mut page = Self {
            host,
            api,
            progress,
            back: false,
            entered: now,
            locked: false,
            wipe: None,
            settings: None,
            readiness: None,
            tasks: Vec::new(),
            free_mb: None,
            input_monitoring: false,
            accessibility: false,
            downloads: Downloads::default(),
            failure_details: false,
            armed: None,
            gate: None,
            practice: None,
            enabling_hotkey: false,
            keys_advancing: false,
            held: None,
            held_at: now,
            pressed_once: false,
            pressed_at: now,
            take: TakePhase::Idle,
            take_at: now,
            take_error: None,
            trying: false,
            mic: Mic::default(),
            mic_select: None,
            devices: Vec::new(),
            login,
            name: NameState::new(),
            messy: MessyState::new(),
            rewrite: RewriteState::new(),
            style: StyleState::new(),
            name_field,
            messy_field,
            reply_field,
            edit_field,
            rewrite_field,
            _subscriptions: subscriptions,
            _follow: None,
        };
        page.follow_host(window, cx);
        page.poll(window, cx);
        page.check_disk(cx);
        page.enter_step(window, cx);
        page
    }

    fn follow_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        let page = cx.entity().downgrade();
        let on_event: OnHostEvent = Rc::new(move |event, cx| {
            let page = page.clone();
            let _ = handle.update(cx, |_, window, cx| {
                page.update(cx, |page, cx| page.on_host_event(event, window, cx))
            });
        });
        self._follow = Some(self.host.follow(on_event, cx));
    }

    /// Reads the server and the permissions every second while the window is open.
    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn_in(window, async move |page, cx| {
            loop {
                let api = api.clone();
                let read = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            api.capture_settings().ok(),
                            api.capture_readiness().ok(),
                            api.active_tasks().ok(),
                        )
                    })
                    .await;
                if page
                    .update_in(cx, |page, window, cx| page.polled(read, window, cx))
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        })
        .detach();
    }

    fn polled(
        &mut self,
        (settings, readiness, tasks): (
            Option<CaptureSettings>,
            Option<CaptureReadinessResponse>,
            Option<kass_api::types::ActiveTasksResponse>,
        ),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(settings) = settings {
            self.settings = Some(settings);
        }
        if readiness.is_some() {
            self.readiness = readiness;
        }
        if let Some(tasks) = tasks {
            self.tasks = tasks.downloads;
        }
        self.input_monitoring = self.host.input_monitoring();
        self.accessibility = self.host.accessibility();
        if self.progress.step == Step::Microphone {
            let permission = self.host.microphone_permission();
            if self.mic.permission != Some(permission) {
                self.mic.permission = Some(permission);
                if permission == MicPermission::Granted {
                    self.devices = self.host.input_devices();
                    if !self.mic.previewing {
                        self.start_mic();
                    }
                }
            }
        }
        self.run_downloads(cx);
        self.sync_host(cx);
        let locked = self.progress.step.is_locked(self.summary().ready);
        if locked != self.locked {
            // A step unlocking when the models land is a new screen too.
            self.locked = locked;
            self.entered = Instant::now();
        }
        self.maybe_advance_keys(window, cx);
        cx.notify();
    }

    pub(crate) fn check_disk(&mut self, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn(async move |page, cx| {
            let free = cx
                .background_executor()
                .spawn(async move { api.models_disk_space_mb().ok() })
                .await;
            page.update(cx, |page, cx| {
                page.free_mb = free;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            speech: self.readiness.as_ref().map(|r| &r.stt),
            cleanup: self.readiness.as_ref().map(|r| &r.llm),
            needs_cleanup: self.settings.as_ref().is_some_and(|s| s.auto_refine),
            tasks: &self.tasks,
            free_mb: self.free_mb,
            started: self.progress.downloads_started,
        }
    }

    pub(crate) fn summary(&self) -> Summary {
        self.downloads.summary(&self.inputs())
    }

    pub(crate) fn models_ready(&self) -> bool {
        self.readiness.is_some() && self.summary().ready
    }

    fn run_downloads(&mut self, cx: &mut Context<Self>) {
        if self.readiness.is_none() {
            return;
        }
        let inputs = Inputs {
            speech: self.readiness.as_ref().map(|r| &r.stt),
            cleanup: self.readiness.as_ref().map(|r| &r.llm),
            needs_cleanup: self.settings.as_ref().is_some_and(|s| s.auto_refine),
            tasks: &self.tasks,
            free_mb: self.free_mb,
            started: self.progress.downloads_started,
        };
        let actions = self.downloads.step(&inputs);
        for action in actions {
            match action {
                Action::Request(model) => self.request_download(model, false, cx),
                Action::RetryLater(model) => self.request_download(model, true, cx),
            }
        }
    }

    /// Asks the server for `model`; `again` cancels the failed try first,
    /// after the retry delay.
    fn request_download(&mut self, model: String, again: bool, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn(async move |page, cx| {
            if again {
                cx.background_executor()
                    .timer(Duration::from_millis(RETRY_DELAY_MS))
                    .await;
            }
            let name = model.clone();
            let sent = cx
                .background_executor()
                .spawn(async move {
                    if again {
                        let _ = api.cancel_download(&name);
                    }
                    api.trigger_model_download(&name)
                })
                .await;
            if sent.is_err() {
                page.update(cx, |page, _| page.downloads.request_failed(&model))
                    .ok();
            }
        })
        .detach();
    }

    /// Starts the downloads, from the download step.
    pub(crate) fn start_downloads(&mut self, cx: &mut Context<Self>) {
        self.progress.downloads_started = true;
        self.save_progress();
        self.run_downloads(cx);
        cx.notify();
    }

    /// The failed model, tried again from the start.
    pub(crate) fn retry_download(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self
            .summary()
            .failed
            .and_then(|f| f.model)
            .map(|m| m.model_name)
        else {
            return;
        };
        self.downloads.retry(&model);
        self.request_download(model, true, cx);
        cx.notify();
    }

    /// The speech model keeps failing: switch to a smaller one.
    pub(crate) fn use_smaller_speech_model(&mut self, cx: &mut Context<Self>) {
        let failed = self
            .summary()
            .failed
            .and_then(|f| f.model)
            .map(|m| m.model_name);
        let api = self.api.clone();
        cx.spawn(async move |page, cx| {
            let settings = cx
                .background_executor()
                .spawn(async move {
                    if let Some(model) = failed {
                        let _ = api.cancel_download(&model);
                    }
                    api.update_capture_settings(&CaptureSettingsUpdate {
                        stt_model: Some(crate::flow::FALLBACK_STT_MODEL),
                        ..Default::default()
                    })
                    .ok()
                })
                .await;
            page.update(cx, |page, cx| {
                if settings.is_some() {
                    page.settings = settings;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Saves a settings change and keeps what the server sends back.
    pub(crate) fn update_settings(&mut self, patch: CaptureSettingsUpdate, cx: &mut Context<Self>) {
        let api = self.api.clone();
        cx.spawn(async move |page, cx| {
            let settings = cx
                .background_executor()
                .spawn(async move { api.update_capture_settings(&patch).ok() })
                .await;
            page.update(cx, |page, cx| {
                if settings.is_some() {
                    page.settings = settings;
                    page.sync_host(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn chords(&self) -> Chords {
        let settings = self.settings.as_ref();
        Chords {
            push: settings
                .map(|s| s.chord_push_to_talk_keys.clone())
                .filter(|k| !k.is_empty())
                .unwrap_or_else(default_push_keys),
            toggle: settings
                .map(|s| s.chord_toggle_to_talk_keys.clone())
                .unwrap_or_default(),
            command: settings
                .map(|s| s.chord_command_keys.clone())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn push_keys(&self) -> Vec<String> {
        self.chords().push
    }

    /// Keeps the app's keys in step: armed once Input Monitoring is on,
    /// blocked from dictating until the models are in, and practicing where
    /// a step only shows the keys going down.
    fn sync_host(&mut self, cx: &mut Context<Self>) {
        let Some(settings) = self.settings.clone() else {
            return;
        };
        if self.input_monitoring && !settings.hotkey_enabled && !self.enabling_hotkey {
            self.enabling_hotkey = true;
            self.update_settings(
                CaptureSettingsUpdate {
                    hotkey_enabled: Some(true),
                    ..Default::default()
                },
                cx,
            );
        }
        let arm = (settings.hotkey_enabled && self.input_monitoring).then(|| self.chords());
        if self.armed.as_ref() != Some(&arm) {
            self.host.arm_hotkey(arm.as_ref());
            self.armed = Some(arm);
        }
        let ready = self.models_ready();
        let gate = (!ready).then(|| t("dictation-still-downloading"));
        if self.gate.as_ref() != Some(&gate) {
            self.host.set_dictation_gate(gate.clone());
            self.gate = Some(gate);
        }
        let step = self.progress.step;
        let practice =
            (step == Step::InputMonitoring && self.trying) || (step == Step::Keys && !ready);
        if self.practice != Some(practice) {
            self.host.set_chord_practice(practice);
            self.practice = Some(practice);
        }
    }

    fn save_progress(&self) {
        if let Some(path) = self.host.progress_path() {
            self.progress.save(&path);
        }
    }

    /// Opens `step`, its color spreading from where the user clicked.
    pub(crate) fn go(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let from = self.progress.step;
        if from == step {
            return;
        }
        self.leave_step(from, cx);
        self.back = step.index() < from.index();
        self.wipe = Some(Wipe {
            from: from.color(),
            start: Instant::now(),
            origin: window.mouse_position(),
        });
        self.progress.step = step;
        self.save_progress();
        self.enter_step(window, cx);
        self.sync_host(cx);
        cx.notify();
    }

    pub(crate) fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let step = self.progress.step.next();
        self.go(step, window, cx);
    }

    fn leave_step(&mut self, step: Step, cx: &mut Context<Self>) {
        match step {
            Step::Microphone => {
                self.host.mic_preview_stop();
                self.mic.previewing = false;
            }
            Step::Style => self.leave_style(cx),
            _ => {}
        }
    }

    fn enter_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.entered = now;
        self.locked = self.progress.step.is_locked(self.models_ready());
        self.trying = false;
        self.pressed_once = false;
        self.keys_advancing = false;
        self.take_error = None;
        self.failure_details = false;
        match self.progress.step {
            Step::Microphone => {
                self.mic = Mic::default();
                let permission = self.host.microphone_permission();
                self.mic.permission = Some(permission);
                if permission == MicPermission::Granted {
                    self.devices = self.host.input_devices();
                    self.start_mic();
                }
            }
            Step::Keys => self.login = self.host.launch_at_login(),
            Step::Name => {
                self.name = NameState::new();
                self.name_field
                    .update(cx, |field, cx| field.set_value("", window, cx));
            }
            Step::Messy => {
                self.messy = MessyState::new();
                self.messy_field
                    .update(cx, |field, cx| field.set_value("", window, cx));
            }
            Step::Rewrite => {
                self.rewrite = RewriteState::new();
                self.rewrite_field.update(cx, |field, cx| {
                    field.set_value(t("onboarding-rewrite-paragraph"), window, cx);
                    field.focus(window, cx);
                    field.select_all(window, cx);
                });
            }
            Step::Style => self.enter_style(window, cx),
            _ => {}
        }
    }

    pub(crate) fn start_mic(&mut self) {
        let device = self
            .settings
            .as_ref()
            .and_then(|s| s.input_device_id.clone());
        match self.host.mic_preview_start(device) {
            Ok(()) => {
                self.mic.previewing = true;
                self.mic.error = None;
            }
            Err(error) => self.mic.error = Some(error),
        }
        self.mic.permission = Some(self.host.microphone_permission());
    }

    /// With the models in, the first press on the keys step is a real take:
    /// the name step opens while the keys are still held.
    fn maybe_advance_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.progress.step != Step::Keys
            || !self.pressed_once
            || self.keys_advancing
            || !self.models_ready()
        {
            return;
        }
        self.keys_advancing = true;
        cx.spawn_in(window, async move |page, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            page.update_in(cx, |page, window, cx| {
                if page.progress.step == Step::Keys {
                    page.next(window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_host_event(&mut self, event: HostEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HostEvent::ChordDown(action) => {
                let now = Instant::now();
                self.held = Some(action);
                self.held_at = now;
                if !self.pressed_once {
                    self.pressed_once = true;
                    self.pressed_at = now;
                }
                self.maybe_advance_keys(window, cx);
            }
            HostEvent::ChordUp => self.held = None,
            HostEvent::MicLevel(db) => {
                if self.progress.step != Step::Microphone {
                    return;
                }
                self.mic.db = db;
            }
            HostEvent::MicError(message) => self.mic.error = Some(message),
            HostEvent::Take { state, message, .. } => {
                let phase = TakePhase::from_state(&state);
                if phase != self.take {
                    self.take_at = Instant::now();
                }
                self.take = phase;
                match state.as_str() {
                    "preparing" | "recording" => self.take_error = None,
                    "error" => self.take_error = message,
                    _ => {}
                }
            }
            HostEvent::Capture(capture) => self.on_capture(*capture, window, cx),
            HostEvent::Insert { take, text } => {
                let inserted = self.insert_focused(text, window, cx);
                self.host.inserted(take, inserted);
            }
            HostEvent::SelectionRequest { take } => {
                let text = if self.progress.step == Step::Rewrite {
                    self.rewrite_field.read(cx).selected_text().to_string()
                } else {
                    String::new()
                };
                self.host.selection(take, text);
            }
        }
        cx.notify();
    }

    /// Types a take's text into whichever field has focus; whether one did.
    fn insert_focused(
        &mut self,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let focused = |field: &Entity<InputState>, cx: &App| {
            field.read(cx).focus_handle(cx).is_focused(window)
        };
        let line = [&self.name_field, &self.reply_field]
            .into_iter()
            .find(|f| focused(f, cx))
            .cloned();
        if let Some(field) = line {
            field.update(cx, |field, cx| field.insert(text, window, cx));
            return true;
        }
        let area = [&self.messy_field, &self.edit_field, &self.rewrite_field]
            .into_iter()
            .find(|f| f.read(cx).focus_handle(cx).is_focused(window))
            .cloned();
        if let Some(field) = area {
            field.update(cx, |field, cx| field.insert(text, window, cx));
            return true;
        }
        false
    }

    fn on_capture(
        &mut self,
        capture: CaptureResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.locked {
            return;
        }
        match self.progress.step {
            Step::Name => self.name_captured(&capture, window, cx),
            Step::Messy => self.messy_captured(&capture),
            Step::Style => self.style_captured(&capture, window, cx),
            Step::Rewrite => self.rewrite_captured(&capture, window, cx),
            _ => {}
        }
    }

    /// The Dictate button: starts a take, or stops the one listening.
    pub(crate) fn toggle_take(&mut self, cx: &mut Context<Self>) {
        if self.take == TakePhase::Listening {
            self.host.dictation_stop();
        } else if self.host.dictation_start().is_some() {
            self.take = TakePhase::Listening;
            self.take_at = Instant::now();
            self.take_error = None;
        }
        cx.notify();
    }

    /// What the status line under a spoken step says.
    pub(crate) fn take_status(&self) -> Option<SharedString> {
        if self.take == TakePhase::Working {
            Some(t("onboarding-working").into())
        } else {
            self.take_error.clone().map(Into::into)
        }
    }

    /// Ends onboarding: shows the main window, at `show` if given.
    pub(crate) fn finish(&mut self, show: Option<String>, cx: &mut Context<Self>) {
        if let Some(path) = self.host.progress_path() {
            Progress::clear(&path);
        }
        let api = self.api.clone();
        let host = self.host.clone();
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .spawn(async move {
                    let _ = api.update_capture_settings(&CaptureSettingsUpdate {
                        onboarding_completed: Some(true),
                        ..Default::default()
                    });
                })
                .await;
            cx.update(|_| host.finish(show));
        })
        .detach();
    }

    fn name_input(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let value = self.name_field.read(cx).value().to_string();
                if self.name.phase == NamePhase::Ask {
                    let trimmed = crate::flow::without_end_punctuation(&value);
                    if trimmed != value {
                        self.name_field
                            .update(cx, |field, cx| field.set_value(trimmed, window, cx));
                    }
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.name_entered(window, cx),
            _ => {}
        }
    }

    fn reply_input(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => cx.notify(),
            InputEvent::PressEnter { .. } => self.use_typed_reply(window, cx),
            _ => {}
        }
    }

    fn edit_input(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { shift: false, .. } = event {
            self.send_edited(window, cx);
        }
    }

    fn corner(&self, summary: &Summary) -> Option<String> {
        if !self.progress.downloads_started {
            return None;
        }
        if summary.failed.is_some() {
            return Some(t("onboarding-corner-stopped"));
        }
        if summary.ready {
            return Some(t("onboarding-corner-ready"));
        }
        if summary.speech.state != crate::downloads::ModelState::Ready {
            return Some(tr!(
                "onboarding-corner-speech",
                percent = summary.speech.percent.round() as i64
            ));
        }
        summary.cleanup.as_ref().map(|cleanup| {
            tr!(
                "onboarding-corner-cleanup",
                percent = cleanup.percent.round() as i64
            )
        })
    }

    fn render_body(
        &mut self,
        poster: Poster,
        summary: &Summary,
        frame: &Frame,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        if self.locked {
            return self.render_locked(poster, summary, frame, cx);
        }
        match self.progress.step {
            Step::Welcome => self.render_welcome(poster, frame, cx),
            Step::Download => self.render_download(poster, summary, cx),
            Step::InputMonitoring => self.render_input_monitoring(poster, frame, cx),
            Step::Accessibility => self.render_accessibility(poster, frame, cx),
            Step::Microphone => self.render_microphone(poster, window, cx),
            Step::Keys => self.render_keys(poster, frame, cx),
            Step::Name => self.render_name(poster, frame, cx),
            Step::Messy => self.render_messy(poster, frame, cx),
            Step::Style => self.render_style(poster, frame, cx),
            Step::Rewrite => self.render_rewrite(poster, frame, cx),
            Step::Done => self.render_done(poster, frame, cx),
        }
    }
}

impl NameState {
    fn new() -> Self {
        Self {
            phase: NamePhase::Ask,
            heard: String::new(),
            name: String::new(),
            fixed: false,
            at: Instant::now(),
        }
    }
}

impl MessyState {
    fn new() -> Self {
        Self {
            result: None,
            at: Instant::now(),
        }
    }
}

impl RewriteState {
    fn new() -> Self {
        Self {
            done: None,
            busy: false,
            error: None,
            at: Instant::now(),
        }
    }
}

impl StyleState {
    pub(crate) fn new() -> Self {
        Self {
            phase: StylePhase::Reply,
            session: None,
            open: None,
            take: StyleTake::default(),
            tried_work: false,
            busy: false,
            error: None,
            at: Instant::now(),
        }
    }
}

/// A take's text as sent: refined, or what was heard.
pub(crate) fn sent_text(capture: &CaptureResponse) -> String {
    capture
        .transcript_refined
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or(&capture.transcript_raw)
        .trim()
        .to_string()
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = Frame::new(cx.reduce_motion());
        let step = self.progress.step;
        let poster = Poster {
            bg: step.color(),
            fg: step.fg(),
        };
        let summary = self.summary();
        let corner = self.corner(&summary);
        let filling = self.progress.downloads_started && !summary.ready && summary.failed.is_none();
        let failed = summary.failed.is_some();
        let body = self.render_body(poster, &summary, &frame, window, cx);
        let (entered, back) = (self.entered, self.back);
        let wipe = self.wipe.as_ref().map(|wipe| {
            motion::color_wipe(&frame, wipe.from, poster.bg, wipe.start, wipe.origin)
                .into_any_element()
        });
        let shimmer = filling.then(|| frame.looping(entered) % 1600. / 1600.);
        let overall = if self.progress.downloads_started {
            summary.overall
        } else {
            0.
        };

        let top_bar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(5.))
            .bg(poster.fg.opacity(0.22))
            .child(
                div()
                    .relative()
                    .h(px(5.))
                    .w(relative((overall / 100.).clamp(0., 1.) as f32))
                    .overflow_hidden()
                    .bg(if failed {
                        Hsla::from(rgb(poster::FAIL))
                    } else {
                        poster.fg
                    })
                    .when_some(shimmer, |bar, t| {
                        bar.child(
                            div()
                                .absolute()
                                .top_0()
                                .h(px(5.))
                                .w(relative(0.3))
                                .left(relative(t * 1.6 - 0.3))
                                .bg(poster.bg.opacity(0.35)),
                        )
                    }),
            );

        let corner_el = corner.map(|text| {
            if failed && step != Step::Download {
                div()
                    .id("corner")
                    .cursor_pointer()
                    .child(poster::mono(text).underline())
                    .on_click(
                        cx.listener(|page, _, window, cx| page.go(Step::Download, window, cx)),
                    )
                    .into_any_element()
            } else {
                poster::mono(text).into_any_element()
            }
        });

        let drag = div()
            .id("drag")
            .relative()
            .flex()
            .flex_none()
            .h(px(36.))
            .items_center()
            .justify_end()
            .px_4()
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .children(corner_el);

        let screen = div()
            .id(ElementId::Name(
                format!("screen-{}-{}", step.index(), self.locked).into(),
            ))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_4()
            .px(px(64.))
            .pt(px(40.))
            .pb(px(56.))
            .children(
                body.into_iter()
                    .enumerate()
                    .map(|(i, piece)| motion::rise(div().child(piece), &frame, entered, i, back)),
            );

        let back_button = (step != Step::Welcome && step != Step::Done).then(|| {
            div()
                .id("back")
                .absolute()
                .bottom(px(16.))
                .left(px(60.))
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .opacity(0.6)
                .hover(|s| s.opacity(1.))
                .cursor_pointer()
                .child(poster::icon(IconName::ArrowLeft, px(14.), poster.fg))
                .child(t("onboarding-back"))
                .on_click(
                    cx.listener(move |page, _, window, cx| page.go(step.previous(), window, cx)),
                )
        });

        let confetti = self.confetti(&frame);
        if frame.moving() {
            window.request_animation_frame();
        }

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(poster.bg)
            .text_color(poster.fg)
            .font_family(metrics::FONT_SANS)
            .text_size(px(15.))
            .children(wipe)
            .child(top_bar)
            .child(drag)
            .child(screen)
            .children(back_button)
            .children(confetti)
    }
}

impl Onboarding {
    /// The bursts the open step has earned.
    fn confetti(&self, frame: &Frame) -> Vec<AnyElement> {
        use motion::Burst;
        let mut bursts = Vec::new();
        match self.progress.step {
            _ if self.locked => {}
            Step::InputMonitoring if self.trying && self.pressed_once => bursts.extend(
                motion::confetti(frame, Burst::Small(0.2, 0.62), self.pressed_at, 0.),
            ),
            Step::Keys if self.pressed_once => bursts.extend(motion::confetti(
                frame,
                Burst::Small(0.2, 0.4),
                self.pressed_at,
                0.,
            )),
            Step::Name if self.name.phase == NamePhase::Confirm => bursts.extend(motion::confetti(
                frame,
                Burst::Small(0.25, 0.28),
                self.name.at,
                350.,
            )),
            Step::Done => bursts.extend(motion::confetti(
                frame,
                Burst::Celebrate,
                self.entered,
                250.,
            )),
            _ => {}
        }
        bursts
    }
}
