//! The Captures page: the app list, the capture list and the open capture.
//! It holds what the server sent and what the user is doing; the render
//! code for each column lives in its own module.

use crate::format::{matches_search, went_into_kass};
use crate::host::{CapturesHost, Route};
use crate::phrases::{DictionaryWord, PhraseDraft, new_phrase, spelling_entry};
use crate::player::PlayerView;
use crate::spelling::Places;
use crate::teach::{Submission, Teach, add_round};
use gpui_kit::component::input::{Escape, InputEvent, InputState, TextareaState};
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureAppFilter, CaptureAppsResponse, CaptureFeedbackCreate, CaptureFeedbackResponse,
    CaptureResponse, CaptureSettings, CaptureSource, CorrectionLearningStatus, DictionaryEntry,
    DictionaryPlaceInput, FeedbackTarget, MovedCorrections, UsagePeriod, UsageStatsResponse,
    WritingStyleHabit, WritingStylesResponse,
};
use kass_api::{ApiClient, CaptureEvent, Mac, SseCancel};
use kass_i18n::tr;
use kass_ui::{
    Button, Confirm, SelectOption, SelectOptions, Toast, ToastKind, close_dialog, dialog_footer,
    dialog_header, open_confirm, open_dialog, select_state, show_toast, text_input,
};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

actions!(
    kass_captures,
    [
        SelectNext,
        SelectPrevious,
        FocusSearch,
        ToggleDetails,
        DeleteCapture
    ]
);

pub(crate) const CONTEXT: &str = "Captures";
pub(crate) const SEARCH_CONTEXT: &str = "CapturesSearch";
const LIMIT: u32 = 200;

struct KeysBound;
impl Global for KeysBound {}

fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    let search = format!("{SEARCH_CONTEXT} > Input");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(&search)),
        KeyBinding::new("up", SelectPrevious, Some(&search)),
        KeyBinding::new("cmd-k", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("cmd-i", ToggleDetails, Some(CONTEXT)),
        KeyBinding::new("backspace", DeleteCapture, Some(CONTEXT)),
        KeyBinding::new("delete", DeleteCapture, Some(CONTEXT)),
    ]);
}

/// Something the server may still be sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    Loading,
    Ready,
    Failed,
}

/// The open capture and what the user is doing with it.
pub(crate) struct Detail {
    pub capture: CaptureResponse,
    /// Its corrections, newest first.
    pub reports: Vec<CaptureFeedbackResponse>,
    pub reports_load: Load,
    pub teach: Teach,
    pub editor: Entity<TextareaState>,
    pub notes: Entity<InputState>,
    pub player: Option<Entity<PlayerView>>,
    pub changes_open: bool,
    pub heard_open: bool,
    pub help_open: bool,
    pub copied: bool,
    /// What is selected in the edit box, so the page redraws as it changes.
    selection: std::ops::Range<usize>,
    _subscriptions: Vec<Subscription>,
}

/// The style picker in the app header, for one app.
pub(crate) struct StyleSelect {
    pub bundle: String,
    pub key: (Option<String>, bool, usize),
    pub state: Entity<SelectState<SelectOptions<String>>>,
    _subscription: Subscription,
}

struct Fetched {
    captures: Result<Vec<CaptureResponse>, String>,
    apps: Option<CaptureAppsResponse>,
    week: Option<UsageStatsResponse>,
    styles: Option<WritingStylesResponse>,
    examples: Option<u64>,
    not_ready: Option<bool>,
    dictionary: Option<Vec<DictionaryEntry>>,
    capture_settings: Option<CaptureSettings>,
}

pub struct CapturesPage {
    pub(crate) client: ApiClient,
    pub(crate) host: Rc<dyn CapturesHost>,
    pub(crate) focus: FocusHandle,
    pub(crate) captures: Vec<CaptureResponse>,
    pub(crate) captures_load: Load,
    pub(crate) apps: Option<CaptureAppsResponse>,
    pub(crate) week: Option<UsageStatsResponse>,
    pub(crate) styles: Option<WritingStylesResponse>,
    pub(crate) examples: u64,
    pub(crate) not_ready: bool,
    pub(crate) dictionary: Vec<DictionaryEntry>,
    pub(crate) capture_settings: Option<CaptureSettings>,
    pub(crate) learning: Option<CorrectionLearningStatus>,
    pub(crate) learning_failed: bool,
    pub(crate) learning_busy: bool,
    pub(crate) learning_open: bool,
    /// What the default writing style has learned about how the user
    /// punctuates, listed in the learning panel.
    pub(crate) habits: Vec<WritingStyleHabit>,
    pub(crate) filter: CaptureAppFilter,
    pub(crate) apps_open: bool,
    pub(crate) details_open: bool,
    pub(crate) search: Entity<InputState>,
    pub(crate) query: String,
    pub(crate) selected: Option<String>,
    pub(crate) detail: Option<Detail>,
    pub(crate) style_select: Option<StyleSelect>,
    pub(crate) list_scroll: ScrollHandle,
    pub(crate) adding_entry: bool,
    pub(crate) deleting: bool,
    /// Macs with an app listening to the server, this one included.
    pub(crate) peers: Vec<Mac>,
    /// A capture a link asked for, opened once the list has loaded.
    linked: Option<String>,
    refreshing: Option<Task<()>>,
    events: SseCancel,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Drop for CapturesPage {
    fn drop(&mut self) {
        self.events.cancel();
    }
}

impl CapturesPage {
    pub fn new(
        client: ApiClient,
        host: Rc<dyn CapturesHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        bind_keys(cx);
        let search = cx
            .new(|cx| InputState::new(window, cx).placeholder(tr!("captures-search-placeholder")));
        let subscriptions =
            vec![
                cx.subscribe_in(&search, window, |page, state, event, window, cx| {
                    if let InputEvent::Change = event {
                        page.query = state.read(cx).value().to_string();
                        page.keep_selection_visible(window, cx);
                        cx.notify();
                    }
                }),
            ];
        let mut page = Self {
            client,
            host,
            focus: cx.focus_handle(),
            captures: Vec::new(),
            captures_load: Load::Loading,
            apps: None,
            week: None,
            styles: None,
            examples: 0,
            not_ready: false,
            dictionary: Vec::new(),
            capture_settings: None,
            learning: None,
            learning_failed: false,
            learning_busy: false,
            learning_open: false,
            habits: Vec::new(),
            filter: CaptureAppFilter::All,
            apps_open: true,
            details_open: true,
            search,
            query: String::new(),
            selected: None,
            detail: None,
            style_select: None,
            list_scroll: ScrollHandle::new(),
            adding_entry: false,
            deleting: false,
            peers: Vec::new(),
            linked: None,
            refreshing: None,
            events: SseCancel::new(),
            _tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        page.refresh(false, window, cx);
        let listen = page.listen(window, cx);
        let poll = page.poll_learning(cx);
        page._tasks.extend([listen, poll]);
        window.focus(&page.focus, cx);
        page
    }

    /// Runs `request` off the main thread, then `done` with its result.
    pub(crate) fn call<T: Send + 'static>(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        request: impl FnOnce(&ApiClient) -> kass_api::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let client = self.client.clone();
        cx.spawn_in(window, async move |page, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { request(&client).map_err(|e| e.to_string()) })
                .await;
            page.update_in(cx, |page, window, cx| done(page, result, window, cx))
                .ok();
        })
        .detach();
    }

    pub(crate) fn toast_error(title: String, error: String, window: &mut Window, cx: &mut App) {
        show_toast(
            window,
            cx,
            Toast::new(title)
                .description(error)
                .kind(ToastKind::Destructive),
        );
    }

    /// Fetches everything the page shows for the current filter. With
    /// `follow_new`, a new capture on top is opened.
    pub(crate) fn refresh(
        &mut self,
        follow_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let client = self.client.clone();
        let filter = self.filter.clone();
        let top = self.captures.first().map(|c| c.id.clone());
        self.refreshing = Some(cx.spawn_in(window, async move |page, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    Fetched {
                        captures: client
                            .list_captures(LIMIT, 0, &filter)
                            .map(|list| list.items)
                            .map_err(|e| e.to_string()),
                        apps: client.list_capture_apps().ok(),
                        week: client.capture_stats(UsagePeriod::Days7, &filter).ok(),
                        styles: client.list_writing_styles().ok(),
                        examples: client.writing_style(None).ok().map(|s| s.example_count),
                        not_ready: client
                            .capture_readiness()
                            .ok()
                            .map(|r| !r.stt.ready || !r.llm.ready),
                        dictionary: client.list_dictionary().ok().map(|d| d.entries),
                        capture_settings: client.capture_settings().ok(),
                    }
                })
                .await;
            page.update_in(cx, |page, window, cx| {
                page.fetched(fetched, follow_new.then_some(top).flatten(), window, cx)
            })
            .ok();
        }));
    }

    fn fetched(
        &mut self,
        fetched: Fetched,
        old_top: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let old_visible: Vec<String> = self.visible().iter().map(|c| c.id.clone()).collect();
        match fetched.captures {
            Ok(captures) => {
                self.captures = captures;
                self.captures_load = Load::Ready;
            }
            Err(error) => {
                eprintln!("kass-captures: captures failed to load: {error}");
                if self.captures_load == Load::Loading {
                    self.captures_load = Load::Failed;
                }
            }
        }
        if let Some(apps) = fetched.apps {
            self.apps = Some(apps);
        }
        if let Some(week) = fetched.week {
            self.week = Some(week);
        }
        if let Some(styles) = fetched.styles {
            self.styles = Some(styles);
        }
        if let Some(examples) = fetched.examples {
            self.examples = examples;
        }
        if let Some(not_ready) = fetched.not_ready {
            self.not_ready = not_ready;
        }
        if let Some(dictionary) = fetched.dictionary {
            self.dictionary = dictionary;
        }
        if let Some(settings) = fetched.capture_settings {
            self.capture_settings = Some(settings);
        }

        let new_top = self.captures.first();
        let opened_new = match (old_top, new_top) {
            (Some(old), Some(top)) if old != top.id && !went_into_kass(top) => {
                let id = top.id.clone();
                self.visible().iter().any(|c| c.id == id).then_some(id)
            }
            _ => None,
        };
        match opened_new {
            Some(id) => self.select(Some(id), window, cx),
            None => self.reconcile_selection(&old_visible, window, cx),
        }
        if let Some(detail) = &mut self.detail
            && let Some(fresh) = self.captures.iter().find(|c| c.id == detail.capture.id)
        {
            detail.capture = fresh.clone();
        }
        self.sync_style_select(window, cx);
        self.sync_search_placeholder(window, cx);
        self.follow_link(window, cx);
        cx.notify();
    }

    /// Opens capture `id` (from the command palette or a `kass://` link):
    /// once the list has loaded, it shows every app and clears the search so
    /// the capture is in view. One older than the list is fetched and added.
    pub fn open_capture(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.linked = Some(id);
        self.follow_link(window, cx);
    }

    fn follow_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.linked.clone() else {
            return;
        };
        if self.captures_load == Load::Loading {
            return;
        }
        if self.captures.iter().any(|c| c.id == id) {
            self.linked = None;
            self.show_linked(id, window, cx);
            return;
        }
        if self.filter != CaptureAppFilter::All {
            self.set_filter(CaptureAppFilter::All, window, cx);
            return;
        }
        self.linked = None;
        self.call(
            window,
            cx,
            {
                let id = id.clone();
                move |client| client.capture(&id)
            },
            move |page, result, window, cx| {
                let Ok(capture) = result else {
                    return;
                };
                if !page.captures.iter().any(|c| c.id == capture.id) {
                    page.captures.push(capture);
                }
                page.show_linked(id, window, cx);
            },
        );
    }

    fn show_linked(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            self.query.clear();
            self.search.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.select(Some(id), window, cx);
        window.focus(&self.focus, cx);
    }

    fn sync_search_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = self.search_placeholder();
        self.search
            .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
    }

    /// Keeps the selection; when its capture is gone, picks the next one
    /// after it, else the one before, else the first.
    fn reconcile_selection(
        &mut self,
        old_visible: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible: Vec<String> = self.visible().iter().map(|c| c.id.clone()).collect();
        let next = match &self.selected {
            Some(id) if visible.contains(id) => Some(id.clone()),
            Some(id) => {
                let at = old_visible.iter().position(|v| v == id);
                at.and_then(|at| {
                    old_visible[at + 1..]
                        .iter()
                        .find(|v| visible.contains(v))
                        .or_else(|| old_visible[..at].iter().rev().find(|v| visible.contains(v)))
                        .cloned()
                })
                .or_else(|| visible.first().cloned())
            }
            None => visible.first().cloned(),
        };
        self.select(next, window, cx);
    }

    /// The captures the list shows: the filter's, matching the search.
    pub(crate) fn visible(&self) -> Vec<&CaptureResponse> {
        let query = self.query.trim();
        self.captures
            .iter()
            .filter(|c| query.is_empty() || matches_search(c, query))
            .collect()
    }

    fn keep_selection_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible();
        let shown = self
            .selected
            .as_ref()
            .is_some_and(|id| visible.iter().any(|c| &c.id == id));
        let first = visible.first().map(|c| c.id.clone());
        if !shown && first.is_some() {
            self.select(first, window, cx);
        }
    }

    /// Opens `id`, or nothing.
    pub(crate) fn select(
        &mut self,
        id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.detail.as_ref().map(|d| &d.capture.id) == id.as_ref() && id.is_some() {
            self.selected = id;
            return;
        }
        self.selected = id.clone();
        self.detail = None;
        let Some(capture) = id.and_then(|id| self.captures.iter().find(|c| c.id == id).cloned())
        else {
            cx.notify();
            return;
        };
        if let Some(at) = self.visible().iter().position(|c| c.id == capture.id) {
            self.list_scroll.scroll_to_item(at);
        }
        self.detail = Some(self.open_detail(capture, window, cx));
        cx.notify();
    }

    fn open_detail(
        &mut self,
        capture: CaptureResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Detail {
        let refined = capture.transcript_refined.clone().filter(|t| !t.is_empty());
        let target = if refined.is_some() {
            FeedbackTarget::Refined
        } else {
            FeedbackTarget::Raw
        };
        let original = refined.unwrap_or_else(|| capture.transcript_raw.clone());
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 40)
                .submit_on_enter(true)
        });
        let notes = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&editor, window, |page, state, event, window, cx| {
                let value = state.read(cx).value().to_string();
                page.editor_event(event, value, window, cx);
            }),
            cx.subscribe_in(&notes, window, |page, state, event, window, cx| {
                let value = state.read(cx).value().to_string();
                page.notes_event(event, value, window, cx);
            }),
            // Selecting a word offers it to the dictionary.
            cx.observe(&editor, |page, editor, cx| {
                let range = editor.read(cx).selected_range();
                if let Some(detail) = &mut page.detail
                    && detail.selection != range
                {
                    detail.selection = range;
                    cx.notify();
                }
            }),
        ];
        let has_audio = !capture.audio_path.is_empty() && !capture.audio_deleted;
        let player = has_audio.then(|| {
            let client = self.client.clone();
            let id = capture.id.clone();
            let ms = capture.duration_ms;
            cx.new(|cx| PlayerView::new(client, id, ms, cx))
        });
        if capture.source != CaptureSource::Command {
            let id = capture.id.clone();
            self.call(
                window,
                cx,
                move |client| client.list_capture_feedback(&id),
                |page, result, _, cx| {
                    let Some(detail) = &mut page.detail else {
                        return;
                    };
                    match result {
                        Ok(reports) => {
                            detail.reports = reports;
                            detail.reports_load = Load::Ready;
                        }
                        Err(error) => {
                            eprintln!("kass-captures: corrections failed to load: {error}");
                            detail.reports_load = Load::Failed;
                        }
                    }
                    cx.notify();
                },
            );
        }
        Detail {
            capture,
            reports: Vec::new(),
            reports_load: Load::Loading,
            teach: Teach::new(target, original),
            editor,
            notes,
            player,
            changes_open: false,
            heard_open: false,
            help_open: false,
            copied: false,
            selection: 0..0,
            _subscriptions: subscriptions,
        }
    }

    /// Moves the selection by `step` through the shown captures.
    pub(crate) fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let visible: Vec<String> = self.visible().iter().map(|c| c.id.clone()).collect();
        if visible.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|id| visible.iter().position(|v| v == id));
        let next = match at {
            Some(at) => (at as isize + step).clamp(0, visible.len() as isize - 1) as usize,
            None => 0,
        };
        self.select(Some(visible[next].clone()), window, cx);
    }

    /// Shows another app's captures; the search starts over.
    pub(crate) fn set_filter(
        &mut self,
        filter: CaptureAppFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.query.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.captures.clear();
        self.captures_load = Load::Loading;
        self.week = None;
        self.select(None, window, cx);
        self.sync_style_select(window, cx);
        self.sync_search_placeholder(window, cx);
        self.refresh(false, window, cx);
    }

    pub(crate) fn filter_app(&self) -> Option<&kass_api::types::CaptureAppCount> {
        let CaptureAppFilter::App(bundle) = &self.filter else {
            return None;
        };
        self.apps
            .as_ref()?
            .apps
            .iter()
            .find(|a| &a.app_bundle_id == bundle)
    }

    pub(crate) fn style_name(&self, id: &str) -> Option<String> {
        self.styles
            .as_ref()?
            .styles
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
    }

    /// Rebuilds the header's style picker when the app or its style changed.
    fn sync_style_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.filter_app().cloned() else {
            self.style_select = None;
            return;
        };
        let Some(styles) = self.styles.as_ref() else {
            return;
        };
        let key = (app.style_id.clone(), app.confirmed, styles.styles.len());
        if self
            .style_select
            .as_ref()
            .is_some_and(|s| s.bundle == app.app_bundle_id && s.key == key)
        {
            return;
        }
        let options: SelectOptions<String> = styles
            .styles
            .iter()
            .map(|s| SelectOption::new(s.id.clone(), s.name.clone()))
            .collect();
        let selected = app.confirmed.then(|| app.style_id.clone()).flatten();
        let state = cx.new(|cx| select_state(options, selected.as_ref(), window, cx));
        let bundle = app.app_bundle_id.clone();
        let name = app.app_name.clone();
        let subscription = cx.subscribe_in(&state, window, move |page, _, event, window, cx| {
            if let SelectEvent::Confirm(Some(style)) = event {
                page.assign_style(bundle.clone(), style.clone(), name.clone(), window, cx);
            }
        });
        self.style_select = Some(StyleSelect {
            bundle: app.app_bundle_id,
            key,
            state,
            _subscription: subscription,
        });
    }

    fn assign_style(
        &mut self,
        bundle: String,
        style: String,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.call(
            window,
            cx,
            move |client| {
                client.assign_app_style(&bundle, &style, name.as_deref(), MovedCorrections::Bring)
            },
            |page, result, window, cx| match result {
                Ok(styles) => {
                    page.styles = Some(styles);
                    page.refresh(false, window, cx);
                }
                Err(error) => Self::toast_error(tr!("captures-feedback-failed"), error, window, cx),
            },
        );
    }

    /// Listens for new and changed captures, reconnecting while the page is open.
    fn listen(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let (sender, receiver) = mpsc::channel::<CaptureEvent>();
        let client = self.client.clone();
        let cancel = self.events.clone();
        std::thread::Builder::new()
            .name("kass-captures-events".into())
            .spawn(move || {
                while !cancel.is_cancelled() {
                    if let Ok(stream) = client.capture_events(&cancel) {
                        for event in stream {
                            let Ok(event) = event else { break };
                            if sender.send(CaptureEvent::from(event)).is_err() {
                                return;
                            }
                        }
                    }
                    // While down, no Mac is known to be connected.
                    if sender.send(CaptureEvent::Peers(Vec::new())).is_err() {
                        return;
                    }
                    for _ in 0..30 {
                        if cancel.is_cancelled() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .ok();
        cx.spawn_in(window, async move |page, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(400))
                    .await;
                let (mut changed, mut learned, mut peers) = (false, false, None);
                loop {
                    match receiver.try_recv() {
                        Ok(CaptureEvent::Changed) => changed = true,
                        Ok(CaptureEvent::Learned) => learned = true,
                        Ok(CaptureEvent::Peers(macs)) => peers = Some(macs),
                        Ok(_) => {}
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                if !(changed || learned || peers.is_some()) {
                    continue;
                }
                let alive = page.update_in(cx, |page, window, cx| {
                    if let Some(peers) = peers
                        && page.peers != peers
                    {
                        page.peers = peers;
                        cx.notify();
                    }
                    if changed || learned {
                        page.refresh(changed, window, cx);
                    }
                    if learned {
                        page.reload_reports(window, cx);
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        })
    }

    fn reload_reports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        if detail.capture.source == CaptureSource::Command {
            return;
        }
        let id = detail.capture.id.clone();
        self.call(
            window,
            cx,
            {
                let id = id.clone();
                move |client| client.list_capture_feedback(&id)
            },
            move |page, result, _, cx| {
                if let (Some(detail), Ok(reports)) = (&mut page.detail, result)
                    && detail.capture.id == id
                {
                    detail.reports = reports;
                    detail.reports_load = Load::Ready;
                    cx.notify();
                }
            },
        );
    }

    /// Learning status and the learned writing habits: every 2s while a
    /// model trains, else every minute.
    fn poll_learning(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let client = self.client.clone();
        cx.spawn(async move |page, cx| {
            loop {
                let client = client.clone();
                let (status, style) = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            client.correction_learning_status(),
                            client.writing_style(None),
                        )
                    })
                    .await;
                let running = status
                    .as_ref()
                    .ok()
                    .and_then(|s| s.model.as_ref())
                    .is_some_and(|m| m.running);
                let alive = page.update(cx, |page, cx| {
                    match status {
                        Ok(status) => {
                            page.learning = Some(status);
                            page.learning_failed = false;
                        }
                        Err(_) => page.learning_failed = true,
                    }
                    if let Ok(style) = style {
                        page.habits = style.habits;
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
                let wait = if running { 2 } else { 60 };
                cx.background_executor()
                    .timer(Duration::from_secs(wait))
                    .await;
            }
        })
    }

    pub(crate) fn learning_action(
        &mut self,
        kind: LearningAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.learning_busy {
            return;
        }
        self.learning_busy = true;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| match kind {
                LearningAction::Run => client.run_correction_learning(),
                LearningAction::Rollback => client.rollback_correction_learning(),
                LearningAction::Cancel => {
                    client.cancel_model_learning()?;
                    client.correction_learning_status()
                }
            },
            |page, result, _, cx| {
                page.learning_busy = false;
                match result {
                    Ok(status) => {
                        page.learning = Some(status);
                        page.learning_failed = false;
                    }
                    Err(_) => page.learning_failed = true,
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn navigate(&self, route: Route, window: &mut Window, cx: &mut App) {
        self.host.navigate(route, window, cx);
    }

    pub(crate) fn ask_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        if self.deleting {
            return;
        }
        let id = detail.capture.id.clone();
        let voice_edit = crate::detail::is_voice_edit(&detail.capture);
        let page = cx.entity().downgrade();
        open_confirm(
            window,
            cx,
            Confirm {
                title: tr!("captures-delete-dialog-title").into(),
                description: Some(
                    tr!(if voice_edit {
                        "captures-delete-dialog-voice-edit-description"
                    } else {
                        "captures-delete-dialog-description"
                    })
                    .into(),
                ),
                confirm: tr!("common-delete").into(),
                cancel: tr!("common-cancel").into(),
                destructive: true,
                on_confirm: Rc::new(move |window, cx| {
                    let id = id.clone();
                    page.update(cx, |page, cx| page.delete(id, window, cx)).ok();
                }),
            },
        );
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.deleting = true;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.delete_capture(&id),
            |page, result, window, cx| {
                page.deleting = false;
                match result {
                    Ok(_) => page.refresh(false, window, cx),
                    Err(error) => {
                        Self::toast_error(tr!("captures-toast-delete-failed"), error, window, cx)
                    }
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn copy(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        show_toast(
            window,
            cx,
            Toast::new(tr!("captures-toast-transcript-copied")),
        );
        if let Some(detail) = &mut self.detail {
            detail.copied = true;
            let id = detail.capture.id.clone();
            cx.spawn(async move |page, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                page.update(cx, |page, cx| {
                    if let Some(detail) = &mut page.detail
                        && detail.capture.id == id
                    {
                        detail.copied = false;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    // Teaching a correction.

    pub(crate) fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_edit_at(None, window, cx);
    }

    /// Start editing with the cursor at byte offset `at` of the shown text
    /// (where the user clicked), or at the end when `None`.
    pub(crate) fn begin_edit_at(
        &mut self,
        at: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.teach.draft.is_some() || detail.teach.saving {
            return;
        }
        detail.teach.begin(&detail.reports);
        let text = detail.teach.current(&detail.reports);
        detail.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            if let Some(at) = at {
                state.set_selected_range(at..at, cx);
            }
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.teach.saving {
            return;
        }
        detail.teach.cancel();
        detail
            .notes
            .update(cx, |state, cx| state.set_value("", window, cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn editor_event(
        &mut self,
        event: &InputEvent,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        match event {
            InputEvent::Change => {
                if detail.teach.draft.is_some() && !detail.teach.saving {
                    detail.teach.draft = Some(value);
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } => self.save_edit(window, cx),
            InputEvent::Blur => self.settle(window, cx),
            InputEvent::Focus => {}
        }
    }

    fn notes_event(
        &mut self,
        event: &InputEvent,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        match event {
            InputEvent::Change => {
                detail.teach.notes = value;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.save_edit(window, cx),
            InputEvent::Blur => self.settle(window, cx),
            InputEvent::Focus => {}
        }
    }

    /// Leaves the edit when focus left it untouched.
    fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.teach.saving {
            return;
        }
        let focused = detail.editor.read(cx).focus_handle(cx).is_focused(window)
            || detail.notes.read(cx).focus_handle(cx).is_focused(window);
        if !focused {
            detail.teach.settle(&detail.reports);
            cx.notify();
        }
    }

    pub(crate) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else {
            return;
        };
        if let Some(submission) = detail.teach.save(&detail.reports) {
            self.submit(submission, window, cx);
        }
    }

    pub(crate) fn submit(
        &mut self,
        submission: Submission,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        detail.teach.saving = true;
        detail
            .editor
            .update(cx, |state, cx| state.set_disabled(true, cx));
        let capture = detail.capture.clone();
        let target = detail.teach.target;
        let body = CaptureFeedbackCreate {
            target,
            expected_text: submission.expected_text.clone(),
            notes: submission.notes.clone(),
            snapshot: capture.clone(),
            source: None,
            replaces: submission.replaces.clone(),
        };
        let id = capture.id.clone();
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.report_capture_output(&id, &body),
            move |page, result, window, cx| {
                let Some(detail) = &mut page.detail else {
                    return;
                };
                if detail.capture.id != capture.id {
                    return;
                }
                detail
                    .editor
                    .update(cx, |state, cx| state.set_disabled(false, cx));
                match result {
                    Ok(report) => {
                        let applied = page.host.apply_correction(
                            capture.id.clone(),
                            submission.before.clone(),
                            submission.expected_text.clone(),
                            cx,
                        );
                        cx.spawn_in(window, async move |_, cx| {
                            let Some(app) = applied.await else {
                                return;
                            };
                            let title = if app.is_empty() {
                                tr!("captures-teach-updated-in-app")
                            } else {
                                tr!("captures-teach-updated-in", app = app)
                            };
                            cx.update(|window, cx| show_toast(window, cx, Toast::new(title)))
                                .ok();
                        })
                        .detach();
                        let report_id = report.id.clone();
                        add_round(&mut detail.reports, report, submission.replaces.as_deref());
                        detail.teach.saved_round(report_id);
                        detail
                            .notes
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        window.focus(&page.focus, cx);
                        page.refresh(false, window, cx);
                    }
                    Err(error) => {
                        detail.teach.saving = false;
                        Self::toast_error(tr!("captures-feedback-failed"), error, window, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    /// Takes back a correction: the newest (Undo) or any (Remove).
    pub(crate) fn withdraw(
        &mut self,
        report_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.teach.removing.is_some() {
            return;
        }
        detail.teach.removing = Some(report_id.clone());
        let capture_id = detail.capture.id.clone();
        cx.notify();
        self.call(
            window,
            cx,
            {
                let (capture_id, report_id) = (capture_id.clone(), report_id.clone());
                move |client| client.withdraw_capture_report(&capture_id, &report_id)
            },
            move |page, result, window, cx| {
                let Some(detail) = &mut page.detail else {
                    return;
                };
                if detail.capture.id != capture_id {
                    return;
                }
                detail.teach.removing = None;
                match result {
                    Ok(()) => {
                        detail.reports.retain(|r| r.id != report_id);
                        if detail.teach.learned.as_deref() == Some(report_id.as_str()) {
                            detail.teach.learned = None;
                        }
                        page.refresh(false, window, cx);
                    }
                    Err(error) => {
                        Self::toast_error(tr!("captures-teach-undo-failed"), error, window, cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Adds a fixed word to the dictionary, then saves the correction with it.
    pub(crate) fn add_word(
        &mut self,
        word: DictionaryWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.adding_entry {
            return;
        }
        self.adding_entry = true;
        let body = spelling_entry(&word.said, &word.written, Vec::new());
        let written = body.written.clone();
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_dictionary_entry(&body),
            move |page, result, window, cx| {
                page.adding_entry = false;
                match result {
                    Ok(entry) => {
                        page.dictionary.push(entry);
                        show_toast(
                            window,
                            cx,
                            Toast::new(tr!("captures-dictionary-added", written = written)),
                        );
                        page.save_edit(window, cx);
                    }
                    Err(error) => {
                        Self::toast_error(tr!("captures-dictionary-add-failed"), error, window, cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Asks what to say and write for a new phrase, then adds it and saves
    /// the correction with it.
    pub(crate) fn make_phrase(
        &mut self,
        draft: PhraseDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let said = cx.new(|cx| InputState::new(window, cx).default_value(draft.said.clone()));
        let written = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .default_value(draft.written.clone())
        });
        let places = Places::new(self);
        let page = cx.entity().downgrade();
        open_dialog(window, cx, move |_, cx| {
            let t = kass_ui::theme(cx).clone();
            let (said, written, page, places) =
                (said.clone(), written.clone(), page.clone(), places.clone());
            let label = |text: String| {
                div()
                    .text_size(kass_ui::metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(text)
            };
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(dialog_header(
                    tr!("captures-phrase-title"),
                    Some(tr!("captures-phrase-description").into()),
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label(tr!("dictionary-phrases-say-placeholder")))
                        .child(text_input(&said, cx)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label(tr!("dictionary-phrases-write-placeholder")))
                        .child(
                            gpui_kit::component::input::Textarea::new(&written)
                                .px(px(10.))
                                .py(px(6.))
                                .rounded(kass_ui::metrics::RADIUS_MD)
                                .bg(t.card)
                                .text_size(kass_ui::metrics::TEXT_BASE),
                        ),
                )
                .child(places.render("phrase-places", cx))
                .child(label(tr!("captures-phrase-saves-correction")))
                .child(
                    dialog_footer()
                        .child(
                            Button::new("phrase-cancel")
                                .outline()
                                .label(tr!("common-cancel"))
                                .on_click(|_, window, cx| close_dialog(window, cx)),
                        )
                        .child(
                            Button::new("phrase-add")
                                .label(tr!("captures-phrase-submit"))
                                .on_click({
                                    let (said, written, page) =
                                        (said.clone(), written.clone(), page.clone());
                                    move |_, window, cx| {
                                        let spoken = said.read(cx).value().trim().to_string();
                                        let text = written.read(cx).value().to_string();
                                        if spoken.is_empty()
                                            || text.trim().is_empty()
                                            || places.is_empty()
                                        {
                                            return;
                                        }
                                        let chosen = places.chosen();
                                        close_dialog(window, cx);
                                        page.update(cx, |page, cx| {
                                            page.add_phrase(spoken, text, chosen, window, cx)
                                        })
                                        .ok();
                                    }
                                }),
                        ),
                )
                .into_any_element()
        });
    }

    fn add_phrase(
        &mut self,
        spoken: String,
        written: String,
        places: Vec<DictionaryPlaceInput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.adding_entry {
            return;
        }
        self.adding_entry = true;
        let body = new_phrase(&spoken, &written, places);
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_dictionary_entry(&body),
            move |page, result, window, cx| {
                page.adding_entry = false;
                match result {
                    Ok(entry) => {
                        page.dictionary.push(entry);
                        show_toast(
                            window,
                            cx,
                            Toast::new(tr!("captures-phrase-added", spoken = spoken)),
                        );
                        page.save_edit(window, cx);
                    }
                    Err(error) => {
                        Self::toast_error(tr!("captures-dictionary-add-failed"), error, window, cx)
                    }
                }
                cx.notify();
            },
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LearningAction {
    Run,
    Rollback,
    Cancel,
}

impl Focusable for CapturesPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CapturesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = kass_ui::theme(cx).clone();
        let list_width = if self.apps_open { px(400.) } else { px(440.) };
        div()
            .id("captures-page")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|page, _: &SelectNext, window, cx| page.step(1, window, cx)))
            .on_action(
                cx.listener(|page, _: &SelectPrevious, window, cx| page.step(-1, window, cx)),
            )
            .on_action(cx.listener(|page, _: &FocusSearch, window, cx| {
                page.search.update(cx, |s, cx| s.focus(window, cx));
            }))
            .on_action(cx.listener(|page, _: &ToggleDetails, _, cx| {
                page.details_open = !page.details_open;
                cx.notify();
            }))
            .on_action(
                cx.listener(|page, _: &DeleteCapture, window, cx| page.ask_delete(window, cx)),
            )
            .on_action(cx.listener(|page, _: &Escape, window, cx| page.cancel_edit(window, cx)))
            .size_full()
            .flex()
            .overflow_hidden()
            .bg(t.background)
            .text_color(t.foreground)
            .font_family(t.sans_font())
            .child(self.render_app_list(cx))
            .child(
                div()
                    .flex_none()
                    .w(list_width)
                    .h_full()
                    .border_r_1()
                    .border_color(t.border)
                    .child(self.render_list(window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_detail_column(window, cx)),
            )
    }
}
