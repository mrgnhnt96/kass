//! The Dictionary page: the scope list on the left, and on the right what
//! applies in the chosen scope: an add form, the scope's own words or
//! phrases, and what it picks up from elsewhere.

use crate::dictation::{PhraseDictation, PhraseTake};
use crate::keys::default_chord;
use crate::places::{OnToggle, Selected, places_menu, scope_label};
use crate::scopes::{
    EntryAge, EntryKind, EntryNote, InheritedEntry, InheritedGroup, InheritedSource, MAX_ENTRIES,
    MAX_LENGTH, MAX_PHRASE_LENGTH, Scope, ScopeOption, ScopeOptions, build_scope_options,
    entries_in, entry_age, entry_note, entry_place_keys, inherited_for_app, inherited_for_style,
    inherited_of_kind, looks_like_code, new_entry, new_phrase, place_input, places_from_keys,
    same_places, spoken_phrase, toggle_place,
};
use crate::widgets::{arrow, icon, mono_label, scope_icon, with_keys};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_api::types::{
    DictionaryEntry, DictionaryEntryUpdate, ResolvedDictionaryResponse, WritingStylesResponse,
};
use kass_i18n::tr;
use kass_ui::{
    Button, ButtonSize, Toast, ToastKind, Toggle, metrics, show_toast, text_input, theme,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

actions!(kass_dictionary, [SubmitPhrase]);

/// ⌘⏎ in a phrase's text saves it; a plain Enter is a new line.
const PHRASE_WRITE: &str = "PhraseWrite";

struct KeysBound;
impl Global for KeysBound {}

fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    cx.bind_keys([KeyBinding::new(
        "cmd-enter",
        SubmitPhrase,
        Some("PhraseWrite Input"),
    )]);
}

/// The dictionary entries, styles and chord as last fetched.
struct Fetched {
    entries: Result<Vec<DictionaryEntry>, String>,
    styles: Option<WritingStylesResponse>,
    chord: Option<Vec<String>>,
    resolved: Option<(String, Result<ResolvedDictionaryResponse, String>)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Take {
    #[default]
    Idle,
    Listening {
        from_button: bool,
    },
    Cleaning,
}

enum EditText {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl EditText {
    fn value(&self, cx: &App) -> String {
        match self {
            EditText::Line(state) => state.read(cx).value().to_string(),
            EditText::Area(state) => state.read(cx).value().to_string(),
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            EditText::Line(state) => state.read(cx).focus_handle(cx),
            EditText::Area(state) => state.read(cx).focus_handle(cx),
        }
    }
}

/// The row being edited.
struct Edit {
    id: String,
    phrase: bool,
    spoken: Entity<InputState>,
    written: EditText,
    places: Vec<String>,
    match_sound: bool,
    saving: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

pub struct DictionaryPage {
    client: ApiClient,
    dictation: Option<Rc<dyn PhraseDictation>>,
    following: Option<Subscription>,
    /// Whether a shortcut take should fill the phrase box instead of being typed.
    aims_here: Rc<Cell<bool>>,

    loaded: bool,
    load_error: Option<String>,
    entries: Vec<DictionaryEntry>,
    styles: Option<WritingStylesResponse>,
    options: Rc<ScopeOptions>,
    chord: Vec<String>,
    /// The chosen app's resolved dictionary, by bundle id.
    resolved: Option<(String, ResolvedDictionaryResponse)>,
    loading: Option<Task<()>>,

    scope: Scope,
    kind: EntryKind,

    say: Entity<InputState>,
    write: Entity<InputState>,
    phrase_say: Entity<InputState>,
    phrase_write: Entity<TextareaState>,
    adding: bool,
    add_error: Option<String>,
    take: Take,
    heard: Option<String>,
    filled: bool,

    edit: Option<Edit>,
    confirming: Option<String>,
    deleting: HashSet<String>,
    open_groups: HashSet<String>,

    _subscriptions: Vec<Subscription>,
}

impl DictionaryPage {
    pub fn new(client: ApiClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);
        let line = |placeholder: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .validate(|text, _| text.chars().count() <= MAX_LENGTH)
            })
        };
        let say = line(tr!("dictionary-add-say-placeholder"), window, cx);
        let write = line(tr!("dictionary-add-write-placeholder"), window, cx);
        let phrase_say = line(tr!("dictionary-phrases-say-placeholder"), window, cx);
        let phrase_write = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(tr!("dictionary-phrases-write-placeholder"))
        });

        let subscriptions = vec![
            cx.subscribe_in(&say, window, |page, _, event, window, cx| {
                page.word_input(event, window, cx)
            }),
            cx.subscribe_in(&write, window, |page, _, event, window, cx| {
                page.word_input(event, window, cx)
            }),
            cx.subscribe_in(&phrase_say, window, |page, _, event, window, cx| {
                page.phrase_input(event, true, window, cx)
            }),
            cx.subscribe_in(&phrase_write, window, |page, _, event, window, cx| {
                page.phrase_input(event, false, window, cx)
            }),
        ];

        let mut page = Self {
            client,
            dictation: None,
            following: None,
            aims_here: Rc::new(Cell::new(false)),
            loaded: false,
            load_error: None,
            entries: Vec::new(),
            styles: None,
            options: Rc::new(build_scope_options(None, &[])),
            chord: default_chord(),
            resolved: None,
            loading: None,
            scope: Scope::Global,
            kind: EntryKind::Words,
            say,
            write,
            phrase_say,
            phrase_write,
            adding: false,
            add_error: None,
            take: Take::Idle,
            heard: None,
            filled: false,
            edit: None,
            confirming: None,
            deleting: HashSet::new(),
            open_groups: HashSet::new(),
            _subscriptions: subscriptions,
        };
        page.refresh(cx);
        page
    }

    /// Lets the "When I say" box of a phrase be dictated: a mic beside it,
    /// and the dictation shortcut fills it while the page shows phrases.
    pub fn with_dictation(
        mut self,
        dictation: Rc<dyn PhraseDictation>,
        cx: &mut Context<Self>,
    ) -> Self {
        let aims = self.aims_here.clone();
        let page = cx.weak_entity();
        self.following = Some(dictation.follow(
            Rc::new(move |_| aims.get()),
            Rc::new(move |take, cx| {
                page.update(cx, |page, cx| page.on_take(take, cx)).ok();
            }),
            cx,
        ));
        self.dictation = Some(dictation);
        self
    }

    /// Fetches everything again: after a change made elsewhere, or when the page is shown.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let app = self.app_bundle();
        self.loading = Some(cx.spawn(async move |page, cx| {
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    Fetched {
                        entries: client
                            .list_dictionary()
                            .map(|list| list.entries)
                            .map_err(|e| e.to_string()),
                        styles: client.list_writing_styles().ok(),
                        chord: client
                            .capture_settings()
                            .ok()
                            .map(|s| s.chord_push_to_talk_keys)
                            .filter(|keys| !keys.is_empty()),
                        resolved: app.map(|bundle| {
                            let resolved = client
                                .resolve_dictionary(&bundle)
                                .map_err(|e| e.to_string());
                            (bundle, resolved)
                        }),
                    }
                })
                .await;
            page.update(cx, |page, cx| page.fetched(fetched, cx)).ok();
        }));
    }

    /// Opens a scope (`global`, `style:<id>` or `app:<bundle id>`; `None`
    /// keeps the current one), on its phrases or its words.
    pub fn show(&mut self, scope: Option<String>, phrases: bool, cx: &mut Context<Self>) {
        if let Some(key) = scope {
            self.choose_scope(Scope::parse(&key), cx);
        }
        self.choose_kind(EntryKind::of(phrases), cx);
    }

    fn fetched(&mut self, fetched: Fetched, cx: &mut Context<Self>) {
        self.loading = None;
        match fetched.entries {
            Ok(entries) => {
                self.entries = entries;
                self.load_error = None;
                self.loaded = true;
            }
            Err(error) => self.load_error = Some(error),
        }
        if fetched.styles.is_some() {
            self.styles = fetched.styles;
        }
        if let Some(chord) = fetched.chord {
            self.chord = chord;
        }
        if let Some((bundle, Ok(resolved))) = fetched.resolved {
            self.resolved = Some((bundle, resolved));
        }
        self.rebuild_options();
        cx.notify();
    }

    fn rebuild_options(&mut self) {
        self.options = Rc::new(build_scope_options(self.styles.as_ref(), &self.entries));
    }

    fn app_bundle(&self) -> Option<String> {
        match &self.scope {
            Scope::App(bundle) => Some(bundle.clone()),
            _ => None,
        }
    }

    fn option(&self) -> ScopeOption {
        self.options
            .find(&self.scope.key())
            .cloned()
            .unwrap_or_else(|| ScopeOption {
                key: self.scope.key(),
                scope: self.scope.clone(),
                name: match &self.scope {
                    Scope::Global => String::new(),
                    Scope::Style(id) | Scope::App(id) => id.clone(),
                },
                count: 0,
                style_id: None,
                app_count: 0,
            })
    }

    fn full(&self) -> bool {
        self.entries.len() >= MAX_ENTRIES
    }

    fn choose_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.edit = None;
        self.confirming = None;
        self.add_error = None;
        if self.resolved.as_ref().map(|(b, _)| b) != self.app_bundle().as_ref() {
            self.resolved = None;
        }
        if self.app_bundle().is_some() {
            self.refresh(cx);
        }
        cx.notify();
    }

    fn choose_kind(&mut self, kind: EntryKind, cx: &mut Context<Self>) {
        if kind == self.kind {
            return;
        }
        self.kind = kind;
        self.edit = None;
        self.confirming = None;
        self.add_error = None;
        cx.notify();
    }

    /// Runs a server call off the main thread, then `done` with its result.
    fn call<T: Send + 'static>(
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

    fn update_aim(&self, window: &Window, cx: &App) {
        let focused = |handle: FocusHandle| handle.is_focused(window);
        let mut elsewhere = focused(self.say.read(cx).focus_handle(cx))
            || focused(self.write.read(cx).focus_handle(cx))
            || focused(self.phrase_write.read(cx).focus_handle(cx));
        if let Some(edit) = &self.edit {
            elsewhere = elsewhere
                || focused(edit.spoken.read(cx).focus_handle(cx))
                || focused(edit.written.focus_handle(cx));
        }
        self.aims_here
            .set(self.kind == EntryKind::Phrases && !elsewhere);
    }

    // Adding

    fn word_input(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                if self.add_error.take().is_some() {
                    cx.notify();
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.add_word(window, cx),
            InputEvent::Focus | InputEvent::Blur => self.update_aim(window, cx),
        }
    }

    fn phrase_input(
        &mut self,
        event: &InputEvent,
        say: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.add_error = None;
                cx.notify();
            }
            InputEvent::PressEnter { .. } if say => self.add_phrase(window, cx),
            InputEvent::PressEnter { .. } => {}
            InputEvent::Focus | InputEvent::Blur => self.update_aim(window, cx),
        }
    }

    fn app_name(&self) -> Option<String> {
        matches!(self.scope, Scope::App(_)).then(|| self.option().name)
    }

    fn add_word(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let written = self.write.read(cx).value().trim().to_string();
        if written.is_empty() || self.adding || self.full() {
            return;
        }
        let spoken = self.say.read(cx).value().to_string();
        let body = new_entry(&self.scope, &written, &spoken, self.app_name().as_deref());
        self.adding = true;
        self.add_error = None;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_dictionary_entry(&body),
            |page, result, window, cx| {
                page.adding = false;
                match result {
                    Ok(entry) => {
                        page.added(entry, cx);
                        page.say.update(cx, |s, cx| s.set_value("", window, cx));
                        page.write.update(cx, |s, cx| s.set_value("", window, cx));
                        page.say.update(cx, |s, cx| s.focus(window, cx));
                    }
                    Err(error) => page.add_error = Some(error),
                }
                cx.notify();
            },
        );
    }

    fn phrase_ready(&self, cx: &App) -> Option<(String, String)> {
        let spoken = self.phrase_say.read(cx).value().trim().to_string();
        let written = self.phrase_write.read(cx).value().trim().to_string();
        let ready = !spoken.is_empty()
            && !written.is_empty()
            && written.chars().count() <= MAX_PHRASE_LENGTH
            && !self.full()
            && !self.adding;
        ready.then_some((spoken, written))
    }

    fn add_phrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((spoken, written)) = self.phrase_ready(cx) else {
            return;
        };
        let place = place_input(&self.scope, self.app_name().as_deref());
        let body = new_phrase(&spoken, &written, vec![place]);
        self.adding = true;
        self.add_error = None;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_dictionary_entry(&body),
            |page, result, window, cx| {
                page.adding = false;
                match result {
                    Ok(entry) => {
                        page.added(entry, cx);
                        page.phrase_say
                            .update(cx, |s, cx| s.set_value("", window, cx));
                        page.phrase_write
                            .update(cx, |s, cx| s.set_value("", window, cx));
                    }
                    Err(error) => page.add_error = Some(error),
                }
                cx.notify();
            },
        );
    }

    fn added(&mut self, entry: DictionaryEntry, cx: &mut Context<Self>) {
        self.entries.retain(|e| e.id != entry.id);
        self.entries.insert(0, entry);
        self.rebuild_options();
        self.refresh(cx);
    }

    // Saying a phrase

    fn on_take(&mut self, take: PhraseTake, cx: &mut Context<Self>) {
        self.take = match take {
            PhraseTake::Listening { from_button } => Take::Listening { from_button },
            PhraseTake::Cleaning => Take::Cleaning,
            PhraseTake::Idle => Take::Idle,
            PhraseTake::Heard(text) => {
                let phrase = spoken_phrase(&text);
                if !phrase.is_empty() {
                    self.heard = Some(phrase);
                }
                Take::Idle
            }
        };
        cx.notify();
    }

    /// Puts a heard phrase in the box, which flashes to show it was filled.
    fn fill_heard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(phrase) = self.heard.take() else {
            return;
        };
        self.phrase_say
            .update(cx, |s, cx| s.set_value(phrase, window, cx));
        self.add_error = None;
        self.filled = true;
        cx.spawn(async move |page, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            page.update(cx, |page, cx| {
                page.filled = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_mic(&mut self, cx: &mut Context<Self>) {
        let Some(dictation) = self.dictation.clone() else {
            return;
        };
        match self.take {
            Take::Listening { from_button: true } => dictation.stop(cx),
            Take::Idle => dictation.start(cx),
            _ => {}
        }
    }

    // Editing

    fn begin_edit(&mut self, entry: &DictionaryEntry, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = None;
        let spoken = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("dictionary-add-say"))
                .default_value(entry.spoken.clone().unwrap_or_default())
                .validate(|text, _| text.chars().count() <= MAX_LENGTH)
        });
        let mut subscriptions =
            vec![
                cx.subscribe_in(&spoken, window, |page, _, event, window, cx| {
                    page.edit_input(event, window, cx)
                }),
            ];
        let written = if entry.phrase {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(2, 6)
                    .placeholder(tr!("dictionary-phrases-write"))
                    .default_value(entry.written.clone())
            });
            subscriptions.push(
                cx.subscribe_in(&state, window, |page, _, event, window, cx| {
                    if !matches!(event, InputEvent::PressEnter { .. }) {
                        page.edit_input(event, window, cx)
                    }
                }),
            );
            state.update(cx, |s, cx| s.focus(window, cx));
            EditText::Area(state)
        } else {
            let state = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(tr!("dictionary-add-write"))
                    .default_value(entry.written.clone())
                    .validate(|text, _| text.chars().count() <= MAX_LENGTH)
            });
            subscriptions.push(
                cx.subscribe_in(&state, window, |page, _, event, window, cx| {
                    page.edit_input(event, window, cx)
                }),
            );
            state.update(cx, |s, cx| s.focus(window, cx));
            EditText::Line(state)
        };
        self.edit = Some(Edit {
            id: entry.id.clone(),
            phrase: entry.phrase,
            spoken,
            written,
            places: entry_place_keys(entry),
            match_sound: entry.match_sound,
            saving: false,
            error: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    fn edit_input(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                if let Some(edit) = &mut self.edit {
                    edit.error = None;
                }
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.save_edit(window, cx),
            InputEvent::Focus | InputEvent::Blur => self.update_aim(window, cx),
        }
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            cx.notify();
        }
    }

    fn can_save(&self, edit: &Edit, cx: &App) -> bool {
        !edit.written.value(cx).trim().is_empty()
            && !edit.places.is_empty()
            && !edit.saving
            && (!edit.phrase || !edit.spoken.read(cx).value().trim().is_empty())
    }

    fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.edit else {
            return;
        };
        if !self.can_save(edit, cx) {
            return;
        }
        let Some(entry) = self.entries.iter().find(|e| e.id == edit.id) else {
            self.edit = None;
            cx.notify();
            return;
        };
        let written = edit.written.value(cx).trim().to_string();
        let spoken = edit.spoken.read(cx).value().trim().to_string();
        let spoken = (!spoken.is_empty()).then_some(spoken);
        let mut patch = DictionaryEntryUpdate::default();
        if written != entry.written {
            patch.written = Some(written);
        }
        if spoken != entry.spoken {
            patch.spoken = Some(spoken);
        }
        if !same_places(&edit.places, &entry_place_keys(entry)) {
            patch.places = Some(places_from_keys(&edit.places, &self.options));
        }
        if !edit.phrase && edit.match_sound != entry.match_sound {
            patch.match_sound = Some(edit.match_sound);
        }
        if patch == DictionaryEntryUpdate::default() {
            self.edit = None;
            cx.notify();
            return;
        }
        let id = edit.id.clone();
        if let Some(edit) = &mut self.edit {
            edit.saving = true;
            edit.error = None;
        }
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.update_dictionary_entry(&id, &patch),
            |page, result, _, cx| {
                match result {
                    Ok(entry) => {
                        page.edit = None;
                        if let Some(old) = page.entries.iter_mut().find(|e| e.id == entry.id) {
                            *old = entry;
                        }
                        page.rebuild_options();
                        page.refresh(cx);
                    }
                    Err(error) => {
                        if let Some(edit) = &mut page.edit {
                            edit.saving = false;
                            edit.error = Some(error);
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    fn toggle_edit_place(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.places = toggle_place(&edit.places, key);
            edit.error = None;
            cx.notify();
        }
    }

    // Deleting

    fn ask_delete(&mut self, entry: &DictionaryEntry, window: &mut Window, cx: &mut Context<Self>) {
        if entry_place_keys(entry).len() > 1 {
            self.edit = None;
            self.confirming = Some(entry.id.clone());
            cx.notify();
        } else {
            self.delete(entry.id.clone(), window, cx);
        }
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.confirming = None;
        if !self.deleting.insert(id.clone()) {
            return;
        }
        cx.notify();
        let call_id = id.clone();
        self.call(
            window,
            cx,
            move |client| client.delete_dictionary_entry(&call_id),
            move |page, result, window, cx| {
                page.deleting.remove(&id);
                match result {
                    Ok(_) => {
                        page.entries.retain(|e| e.id != id);
                        if page.edit.as_ref().is_some_and(|e| e.id == id) {
                            page.edit = None;
                        }
                        page.rebuild_options();
                        page.refresh(cx);
                    }
                    Err(error) => show_toast(
                        window,
                        cx,
                        Toast::new(tr!("dictionary-list-delete-failed"))
                            .kind(ToastKind::Destructive)
                            .description(error),
                    ),
                }
                cx.notify();
            },
        );
    }
}

impl Render for DictionaryPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fill_heard(window, cx);
        self.update_aim(window, cx);
        let t = theme(cx).clone();

        div()
            .id("dictionary-page")
            .size_full()
            .overflow_y_scroll()
            .bg(t.background)
            .text_color(t.foreground)
            .child(
                div()
                    .px(px(48.))
                    .py(px(28.))
                    .max_w(px(1040.))
                    .child(
                        div()
                            .mb_5()
                            .child(
                                div()
                                    .text_size(metrics::TEXT_LG)
                                    .line_height(metrics::LINE_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr!("dictionary-title")),
                            )
                            .child(
                                div()
                                    .mt_1()
                                    .text_size(metrics::TEXT_XS)
                                    .line_height(metrics::LINE_XS)
                                    .text_color(t.muted_foreground)
                                    .child(tr!("dictionary-description")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .min_h(px(420.))
                            .rounded(metrics::RADIUS_LG)
                            .border_1()
                            .border_color(t.border)
                            .overflow_hidden()
                            .child(self.render_scope_list(cx))
                            .child(self.render_pane(window, cx)),
                    ),
            )
    }
}

// The scope list

impl DictionaryPage {
    fn render_scope_list(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let heading = |text: String, cx: &App| {
            div()
                .px(px(10.))
                .pt_4()
                .pb_1p5()
                .child(mono_label(text, cx))
        };
        let options = self.options.clone();
        div()
            .w(px(220.))
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5()
            .border_r_1()
            .border_color(t.border)
            .bg(t.muted.opacity(0.4))
            .px_2()
            .py_3()
            .child(self.scope_row(&options.everywhere, cx))
            .when(!options.styles.is_empty(), |list| {
                list.child(heading(tr!("dictionary-scope-styles"), cx))
                    .children(options.styles.iter().map(|o| self.scope_row(o, cx)))
            })
            .when(!options.apps.is_empty(), |list| {
                list.child(heading(tr!("dictionary-scope-apps"), cx))
                    .children(options.apps.iter().map(|o| self.scope_row(o, cx)))
            })
    }

    fn scope_row(&self, option: &ScopeOption, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx).clone();
        let selected = option.scope == self.scope;
        let scope = option.scope.clone();
        div()
            .id(SharedString::from(format!("scope-{}", option.key)))
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .h(px(32.))
            .px(px(10.))
            .rounded(metrics::RADIUS_MD)
            .text_size(metrics::TEXT_BASE)
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(t.accent.opacity(0.1))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(px(2.))
                                .rounded_l(metrics::RADIUS_MD)
                                .bg(t.accent),
                        )
                } else {
                    row.text_color(t.muted_foreground)
                        .hover(|s| s.bg(t.muted.opacity(0.6)))
                }
            })
            .child(scope_icon(&option.scope, &option.name, px(14.), cx))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(scope_label(option)),
            )
            .when(option.count > 0, |row| {
                row.child(
                    div()
                        .flex_none()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .font_weight(FontWeight::NORMAL)
                        .text_color(t.muted_foreground)
                        .child(option.count.to_string()),
                )
            })
            .on_click(cx.listener(move |page, _, _, cx| page.choose_scope(scope.clone(), cx)))
    }
}

// The pane for one scope

impl DictionaryPage {
    fn render_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let option = self.option();
        let in_scope: Vec<DictionaryEntry> = entries_in(&self.entries, &self.scope)
            .into_iter()
            .cloned()
            .collect();
        let words = in_scope.iter().filter(|e| !e.phrase).count();
        let phrases = in_scope.len() - words;
        let shown: Vec<DictionaryEntry> = in_scope
            .into_iter()
            .filter(|e| EntryKind::of(e.phrase) == self.kind)
            .collect();

        let mut pane = div()
            .flex_1()
            .min_w_0()
            .px_6()
            .py_5()
            .child(self.render_header(&option, words, phrases, cx));
        if let Some(error) = &self.load_error {
            pane = pane.child(
                div()
                    .mb_3()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.destructive)
                    .child(error.clone()),
            );
        }
        if !self.loaded {
            return pane;
        }
        pane = match self.kind {
            EntryKind::Words => pane.child(self.render_add_word(cx)),
            EntryKind::Phrases => pane.child(self.render_add_phrase(cx)),
        };
        pane = pane.child(self.render_list(&option, shown, window, cx));
        if let Scope::App(_) = &self.scope {
            if let Some((_, resolved)) = &self.resolved {
                let groups = inherited_of_kind(inherited_for_app(&resolved.entries), self.kind);
                let dropped = resolved.dropped_terms.len();
                pane = pane.child(self.render_inherited(groups, &option, cx));
                if dropped > 0 && self.kind == EntryKind::Words {
                    pane = pane.child(
                        div()
                            .mt_3()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(tr!("dictionary-inherited-dropped", count = dropped)),
                    );
                }
            }
        } else if let Scope::Style(_) = &self.scope {
            let groups =
                inherited_of_kind(inherited_for_style(&self.entries, &self.scope), self.kind);
            pane = pane.child(self.render_inherited(groups, &option, cx));
        }
        pane
    }

    fn render_header(
        &self,
        option: &ScopeOption,
        words: usize,
        phrases: usize,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let subtitle = match &option.scope {
            Scope::Global => Some(tr!("dictionary-header-everywhere")),
            Scope::Style(_) => Some(tr!(
                "dictionary-header-style-apps",
                count = option.app_count
            )),
            Scope::App(_) => option
                .style_id
                .as_ref()
                .and_then(|id| {
                    self.options
                        .styles
                        .iter()
                        .find(|s| s.scope == Scope::Style(id.clone()))
                })
                .map(|style| tr!("dictionary-header-app-style", style = style.name.clone())),
        };
        div()
            .mb_5()
            .flex()
            .items_center()
            .gap(px(10.))
            .when(matches!(option.scope, Scope::App(_)), |row| {
                row.child(scope_icon(&option.scope, &option.name, px(28.), cx))
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(15.))
                            .line_height(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(scope_label(option)),
                    )
                    .when_some(subtitle, |col, subtitle| {
                        col.child(
                            div()
                                .text_size(metrics::TEXT_XS)
                                .line_height(metrics::LINE_XS)
                                .text_color(t.muted_foreground)
                                .child(subtitle),
                        )
                    }),
            )
            .child(self.render_kind_switch(words, phrases, cx))
    }

    fn render_kind_switch(&self, words: usize, phrases: usize, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let button = |kind: EntryKind, label: String, count: usize, cx: &mut Context<Self>| {
            let selected = self.kind == kind;
            div()
                .id(match kind {
                    EntryKind::Words => "kind-words",
                    EntryKind::Phrases => "kind-phrases",
                })
                .flex()
                .items_center()
                .gap_1p5()
                .h(px(28.))
                .px_3()
                .rounded(px(5.))
                .text_size(metrics::TEXT_XS)
                .cursor_pointer()
                .map(|b| {
                    if selected {
                        b.bg(t.muted)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                    } else {
                        b.text_color(t.muted_foreground)
                            .hover(|s| s.text_color(t.foreground))
                    }
                })
                .child(label)
                .child(
                    div()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(if selected {
                            t.accent
                        } else {
                            t.muted_foreground
                        })
                        .child(count.to_string()),
                )
                .on_click(cx.listener(move |page, _, _, cx| page.choose_kind(kind, cx)))
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .p(px(2.))
            .rounded(px(7.))
            .border_1()
            .border_color(t.border)
            .bg(t.muted.opacity(0.4))
            .child(button(
                EntryKind::Words,
                tr!("dictionary-kind-words"),
                words,
                cx,
            ))
            .child(button(
                EntryKind::Phrases,
                tr!("dictionary-kind-phrases"),
                phrases,
                cx,
            ))
    }

    fn field_label(text: String, cx: &App) -> Div {
        let t = theme(cx).clone();
        div()
            .h(px(16.))
            .text_size(metrics::TEXT_XS)
            .line_height(px(16.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(t.foreground)
            .child(text)
    }

    /// The limit when the dictionary is full, else the last error.
    fn add_message(&self, cx: &App) -> Option<Div> {
        let t = theme(cx).clone();
        let line = |text: String, color: Hsla| {
            div()
                .mt_2()
                .text_size(metrics::TEXT_XS)
                .text_color(color)
                .child(text)
        };
        if self.full() {
            Some(line(
                tr!("dictionary-add-limit", count = MAX_ENTRIES),
                t.muted_foreground,
            ))
        } else {
            self.add_error
                .clone()
                .map(|error| line(error, t.destructive))
        }
    }

    fn render_add_word(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let can_add =
            !self.write.read(cx).value().trim().is_empty() && !self.full() && !self.adding;
        div()
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(Self::field_label(tr!("dictionary-add-say"), cx))
                            .child(text_input(&self.say, cx).h(px(32.))),
                    )
                    .child(
                        div()
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .child(arrow(true, cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(Self::field_label(tr!("dictionary-add-write"), cx))
                            .child(text_input(&self.write, cx).h(px(32.))),
                    )
                    .child(
                        Button::new("add-word")
                            .small()
                            .label(tr!("dictionary-add-action"))
                            .disabled(!can_add)
                            .loading(self.adding)
                            .on_click(cx.listener(|page, _, window, cx| page.add_word(window, cx))),
                    ),
            )
            .child(
                div()
                    .mt_2()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(tr!("dictionary-add-hint")),
            )
            .children(self.add_message(cx))
    }

    fn render_add_phrase(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let ready = self.phrase_ready(cx).is_some();
        let mut write_col = div().min_w_0().flex().flex_col().gap_1p5();
        write_col.style().flex_grow = Some(1.25);
        write_col.style().flex_shrink = Some(1.);
        write_col.style().flex_basis = Some(relative(0.).into());
        div()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(Self::field_label(tr!("dictionary-add-say"), cx))
                            .child(self.render_phrase_say(cx)),
                    )
                    .child(
                        div()
                            .mt(px(22.))
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .child(arrow(true, cx)),
                    )
                    .child(
                        write_col
                            .child(Self::field_label(tr!("dictionary-phrases-write"), cx))
                            .child(
                                div()
                                    .key_context(PHRASE_WRITE)
                                    .on_action(cx.listener(|page, _: &SubmitPhrase, window, cx| {
                                        page.add_phrase(window, cx)
                                    }))
                                    .child(self.phrase_textarea(&self.phrase_write, cx)),
                            ),
                    )
                    .child(
                        div().mt(px(22.)).child(
                            Button::new("add-phrase")
                                .small()
                                .label(tr!("dictionary-add-action"))
                                .disabled(!ready)
                                .loading(self.adding)
                                .on_click(
                                    cx.listener(|page, _, window, cx| page.add_phrase(window, cx)),
                                ),
                        ),
                    ),
            )
            .child(
                self.render_phrase_hint(cx)
                    .mt_2()
                    .text_color(t.muted_foreground),
            )
            .children(self.add_message(cx))
    }

    fn phrase_textarea(&self, state: &Entity<TextareaState>, cx: &App) -> Textarea {
        let t = theme(cx).clone();
        Textarea::new(state)
            .px(px(10.))
            .py(px(6.))
            .rounded(metrics::RADIUS_MD)
            .bg(t.card)
            .text_size(metrics::TEXT_BASE)
    }

    fn render_phrase_say(&self, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let listening = matches!(self.take, Take::Listening { .. });
        let from_button = self.take == (Take::Listening { from_button: true });
        let cleaning = self.take == Take::Cleaning;
        let input: Input = text_input(&self.phrase_say, cx)
            .h(px(32.))
            .when(self.filled, |input| input.bg(t.accent.opacity(0.1)));
        let field = div()
            .relative()
            .flex_1()
            .min_w_0()
            .when(cleaning, |f| f.opacity(0.6))
            .child(input)
            .when(listening, |f| f.child(listening_overlay(cx)));

        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(field)
            .when_some(self.dictation.as_ref(), |row, _| {
                let button = Button::new("phrase-mic")
                    .size(ButtonSize::Icon)
                    .w(px(32.))
                    .h(px(32.))
                    .disabled(cleaning || (listening && !from_button))
                    .on_click(cx.listener(|page, _, _, cx| page.toggle_mic(cx)));
                let button = if from_button {
                    button
                        .icon(IconName::Square)
                        .tooltip(tr!("dictionary-phrases-stop"))
                } else {
                    button
                        .outline()
                        .icon(IconName::Mic)
                        .tooltip(tr!("dictionary-phrases-dictate"))
                };
                row.child(button)
            })
    }

    fn render_phrase_hint(&self, cx: &App) -> Div {
        let id = if self.dictation.is_none() {
            "dictionary-phrases-hint-typed"
        } else if self.take == (Take::Listening { from_button: false }) {
            "dictionary-phrases-hint-release"
        } else {
            "dictionary-phrases-hint"
        };
        with_keys(&tr!(id), &self.chord, cx)
            .min_h(px(26.))
            .text_size(metrics::TEXT_XS)
            .line_height(px(26.))
    }
}

/// Bars that show the box is listening, over its text.
fn listening_overlay(cx: &App) -> Div {
    let t = theme(cx).clone();
    let accent = t.accent;
    let bars = [6., 14., 10., 16., 8.];
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(10.))
        .rounded(metrics::RADIUS_MD)
        .border_1()
        .border_color(accent)
        .bg(t.card)
        .shadow(vec![BoxShadow {
            color: accent.opacity(0.2),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(3.),
            inset: false,
        }])
        .child(
            div().flex().items_center().gap(px(2.)).h(px(16.)).children(
                bars.into_iter()
                    .enumerate()
                    .map(move |(i, height): (usize, f32)| {
                        div()
                            .w(px(3.))
                            .h(px(height))
                            .rounded(px(1.5))
                            .bg(accent)
                            .with_animation(
                                ("listening-bar", i),
                                Animation::new(Duration::from_millis(900)).repeat(),
                                move |bar, delta| {
                                    let phase = (delta + i as f32 * 0.18).fract();
                                    let scale = 0.45 + 0.55 * (phase * std::f32::consts::PI).sin();
                                    bar.h(px(height * scale))
                                },
                            )
                    }),
            ),
        )
        .child(
            div()
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_CAPTION)
                .text_color(accent)
                .child(tr!("dictionary-phrases-listening")),
        )
}

// The list

impl DictionaryPage {
    fn render_list(
        &mut self,
        option: &ScopeOption,
        shown: Vec<DictionaryEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let title = match self.kind {
            EntryKind::Words => tr!(
                "dictionary-list-title",
                scope = scope_label(option),
                count = shown.len()
            ),
            EntryKind::Phrases => tr!(
                "dictionary-phrases-list-title",
                scope = scope_label(option),
                count = shown.len()
            ),
        };
        let empty = match self.kind {
            EntryKind::Words => tr!("dictionary-list-empty"),
            EntryKind::Phrases => tr!("dictionary-phrases-empty"),
        };
        let now = chrono::Utc::now();
        let mut rows = Vec::new();
        for (i, entry) in shown.iter().enumerate() {
            let editing = self.edit.as_ref().is_some_and(|e| e.id == entry.id);
            let row = if editing {
                self.render_edit_row(entry, option, window, cx)
            } else {
                self.render_row(entry, now, cx)
            };
            rows.push(row.when(i > 0, |r| {
                r.border_t_1().border_color(t.border.opacity(0.7))
            }));
        }

        div()
            .mt_5()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.border)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(t.border)
                    .bg(t.muted.opacity(0.4))
                    .child(mono_label(title, cx))
                    .child(mono_label(tr!("dictionary-list-order"), cx)),
            )
            .map(|list| {
                if rows.is_empty() {
                    list.child(
                        div()
                            .px_3()
                            .py_4()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(empty),
                    )
                } else {
                    list.children(rows)
                }
            })
    }

    fn said(entry_spoken: Option<&str>, cx: &App) -> Div {
        let t = theme(cx).clone();
        match entry_spoken {
            Some(spoken) => div()
                .truncate()
                .text_size(metrics::TEXT_SM)
                .child(spoken.to_string()),
            None => div()
                .truncate()
                .italic()
                .text_size(metrics::TEXT_XS)
                .text_color(t.muted_foreground)
                .child(tr!("dictionary-list-spelling-only")),
        }
    }

    fn render_row(
        &self,
        entry: &DictionaryEntry,
        now: chrono::DateTime<chrono::Utc>,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let deleting = self.deleting.contains(&entry.id);
        let confirming = self.confirming.as_deref() == Some(entry.id.as_str());
        let age = entry
            .created_at
            .as_deref()
            .and_then(|at| entry_age(at, now))
            .map(age_text);

        let written = if entry.phrase {
            div()
                .min_w_0()
                .flex_1()
                .text_size(if looks_like_code(&entry.written) {
                    metrics::TEXT_BASE
                } else {
                    metrics::TEXT_SM
                })
                .when(looks_like_code(&entry.written), |d| {
                    d.font_family(t.mono_font.clone())
                })
                .line_clamp(3)
                .overflow_hidden()
                .child(entry.written.clone())
        } else {
            let note = entry_note(entry.source, entry.match_sound).map(|note| {
                let (label, hint) = match note {
                    EntryNote::SpokenFix => (
                        "dictionary-list-note-spoken-fix",
                        "dictionary-list-note-spoken-fix-hint",
                    ),
                    EntryNote::Correction => (
                        "dictionary-list-note-correction",
                        "dictionary-list-note-correction-hint",
                    ),
                    EntryNote::Exact => (
                        "dictionary-list-note-exact",
                        "dictionary-list-note-exact-hint",
                    ),
                };
                let hint: SharedString = tr!(hint).into();
                div()
                    .id(SharedString::from(format!("note-{}", entry.id)))
                    .flex_none()
                    .italic()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(tr!(label))
                    .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            });
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(metrics::TEXT_SM)
                        .when(looks_like_code(&entry.written), |d| {
                            d.font_family(t.mono_font.clone())
                                .text_size(metrics::TEXT_BASE)
                        })
                        .child(entry.written.clone()),
                )
                .children(note)
        };

        let edit_entry = entry.clone();
        let delete_entry = entry.clone();
        let row = div()
            .flex()
            .gap_3()
            .px_3()
            .py_2()
            .map(|r| {
                if entry.phrase {
                    r.items_start()
                } else {
                    r.items_center()
                }
            })
            .child(
                div()
                    .w(px(200.))
                    .flex_none()
                    .min_w_0()
                    .child(Self::said(entry.spoken.as_deref(), cx)),
            )
            .child(
                div()
                    .flex_none()
                    .when(entry.phrase, |d| d.mt(px(3.)))
                    .child(arrow(false, cx)),
            )
            .child(written)
            .child(
                div()
                    .flex_none()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .text_right()
                    .children(age),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .when(entry.phrase, |d| d.mt(px(-4.)))
                    .child(
                        Button::new(SharedString::from(format!("edit-{}", entry.id)))
                            .ghost()
                            .size(ButtonSize::Icon)
                            .w(px(28.))
                            .h(px(28.))
                            .icon(IconName::Pencil)
                            .tooltip(tr!("dictionary-list-edit", written = entry.written.clone()))
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.begin_edit(&edit_entry, window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("delete-{}", entry.id)))
                            .ghost()
                            .size(ButtonSize::Icon)
                            .w(px(28.))
                            .h(px(28.))
                            .icon(IconName::Trash)
                            .disabled(deleting)
                            .tooltip(tr!(
                                "dictionary-list-delete",
                                written = entry.written.clone()
                            ))
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.ask_delete(&delete_entry, window, cx)
                            })),
                    ),
            );

        let id = entry.id.clone();
        div().child(row).when(confirming, |d| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.destructive.opacity(0.05))
                    .child(
                        div()
                            .flex_1()
                            .text_size(metrics::TEXT_XS)
                            .child(tr!("dictionary-list-confirm-delete")),
                    )
                    .child(
                        Button::new("cancel-delete")
                            .ghost()
                            .small()
                            .h(px(28.))
                            .label(tr!("dictionary-list-cancel"))
                            .on_click(cx.listener(|page, _, _, cx| {
                                page.confirming = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("confirm-delete")
                            .destructive()
                            .small()
                            .h(px(28.))
                            .label(tr!("dictionary-list-remove"))
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.delete(id.clone(), window, cx)
                            })),
                    ),
            )
        })
    }

    fn render_edit_row(
        &self,
        entry: &DictionaryEntry,
        option: &ScopeOption,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = theme(cx).clone();
        let Some(edit) = &self.edit else {
            return div();
        };
        let can_save = self.can_save(edit, cx);
        let written = match &edit.written {
            EditText::Line(state) => div()
                .min_w_0()
                .flex_1()
                .child(text_input(state, cx).h(px(28.))),
            EditText::Area(state) => div()
                .min_w_0()
                .flex_1()
                .key_context(PHRASE_WRITE)
                .on_action(
                    cx.listener(|page, _: &SubmitPhrase, window, cx| page.save_edit(window, cx)),
                )
                .child(self.phrase_textarea(state, cx)),
        };

        let page = cx.weak_entity();
        let selected: Selected = Rc::new(move |cx| {
            page.upgrade()
                .and_then(|page| page.read(cx).edit.as_ref().map(|e| e.places.clone()))
                .unwrap_or_default()
        });
        let page = cx.weak_entity();
        let on_toggle: OnToggle = Rc::new(move |key, _, cx| {
            page.update(cx, |page, cx| page.toggle_edit_place(key, cx))
                .ok();
        });
        let viewed_from = matches!(option.scope, Scope::App(_)).then(|| option.clone());
        let places = places_menu(
            SharedString::from(format!("places-{}", entry.id)),
            selected,
            on_toggle,
            self.options.clone(),
            viewed_from,
            cx,
        );
        let match_sound = edit.match_sound;

        div()
            .on_action(cx.listener(|page, _: &Escape, _, cx| page.cancel_edit(cx)))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .map(|r| {
                        if edit.phrase {
                            r.items_start()
                        } else {
                            r.items_center()
                        }
                    })
                    .child(
                        div()
                            .w(px(200.))
                            .flex_none()
                            .child(text_input(&edit.spoken, cx).h(px(28.))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .when(edit.phrase, |d| d.mt(px(7.)))
                            .child(arrow(false, cx)),
                    )
                    .child(written)
                    .child(div().w(px(56.)).flex_none()),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.accent.opacity(0.05))
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(metrics::TEXT_XS)
                                    .text_color(t.muted_foreground)
                                    .child(tr!("dictionary-list-applies-in")),
                            )
                            .child(places)
                            .child(div().flex_1())
                            .child(
                                Button::new("cancel-edit")
                                    .ghost()
                                    .small()
                                    .h(px(28.))
                                    .label(tr!("dictionary-list-cancel"))
                                    .on_click(cx.listener(|page, _, _, cx| page.cancel_edit(cx))),
                            )
                            .child(
                                Button::new("save-edit")
                                    .small()
                                    .h(px(28.))
                                    .label(tr!("dictionary-list-save"))
                                    .disabled(!can_save)
                                    .loading(edit.saving)
                                    .on_click(cx.listener(|page, _, window, cx| {
                                        page.save_edit(window, cx)
                                    })),
                            ),
                    )
                    .when(!edit.phrase, |strip| {
                        let hint: SharedString = tr!("dictionary-list-match-sound-hint").into();
                        strip.child(
                            div()
                                .id("match-sound")
                                .mt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Toggle::new("match-sound-toggle")
                                        .checked(match_sound)
                                        .on_change(cx.listener(|page, on: &bool, _, cx| {
                                            if let Some(edit) = &mut page.edit {
                                                edit.match_sound = *on;
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(metrics::TEXT_XS)
                                        .child(tr!("dictionary-list-match-sound")),
                                )
                                .tooltip(move |window, cx| {
                                    Tooltip::new(hint.clone()).build(window, cx)
                                }),
                        )
                    })
                    .when_some(edit.error.clone(), |strip, error| {
                        strip.child(
                            div()
                                .mt_2()
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.destructive)
                                .child(error),
                        )
                    }),
            )
    }
}

fn age_text(age: EntryAge) -> String {
    match age {
        EntryAge::Now => tr!("dictionary-list-just-now"),
        EntryAge::Minutes(count) => tr!("dictionary-list-minutes-ago", count = count),
        EntryAge::Hours(count) => tr!("dictionary-list-hours-ago", count = count),
        EntryAge::Date(date, true) => date.format("%b %-d").to_string(),
        EntryAge::Date(date, false) => date.format("%b %-d, %Y").to_string(),
    }
}

// What a scope picks up from elsewhere

impl DictionaryPage {
    fn render_inherited(
        &self,
        groups: Vec<InheritedGroup>,
        option: &ScopeOption,
        cx: &mut Context<Self>,
    ) -> Div {
        if groups.is_empty() {
            return div();
        }
        let t = theme(cx).clone();
        let rows = groups
            .into_iter()
            .enumerate()
            .map(|(i, group)| {
                self.render_group(group, cx).when(i > 0, |g| {
                    g.border_t_1().border_color(t.border.opacity(0.7))
                })
            })
            .collect::<Vec<_>>();
        div()
            .mt_6()
            .child(div().mb_2().child(mono_label(
                tr!("dictionary-inherited-title", scope = scope_label(option)),
                cx,
            )))
            .child(
                div()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(t.border)
                    .overflow_hidden()
                    .children(rows),
            )
    }

    fn render_group(&self, group: InheritedGroup, cx: &mut Context<Self>) -> Div {
        let t = theme(cx).clone();
        let key = group.key();
        let open = self.open_groups.contains(&key);
        let source = match group.source {
            InheritedSource::Global => tr!("dictionary-scope-everywhere"),
            InheritedSource::Style => group
                .scope_id
                .as_ref()
                .and_then(|id| self.options.find(&Scope::Style(id.clone()).key()))
                .map(|o| o.name.clone())
                .unwrap_or_else(|| tr!("dictionary-scope-styles")),
        };
        let toggle_key = key.clone();
        div()
            .child(
                div()
                    .id(SharedString::from(format!("inherited-{key}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.muted.opacity(0.4)))
                    .child(icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        px(14.),
                        t.muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics::TEXT_BASE)
                            .child(tr!("dictionary-inherited-from", source = source)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(group.preview.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_CAPTION)
                            .text_color(t.muted_foreground)
                            .child(group.entries.len().to_string()),
                    )
                    .on_click(cx.listener(move |page, _, _, cx| {
                        if !page.open_groups.remove(&toggle_key) {
                            page.open_groups.insert(toggle_key.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(open, |g| {
                g.child(
                    div()
                        .border_t_1()
                        .border_color(t.border)
                        .bg(t.muted.opacity(0.2))
                        .children(group.entries.iter().map(|e| inherited_row(e, cx))),
                )
            })
    }
}

fn inherited_row(entry: &InheritedEntry, cx: &App) -> Div {
    let t = theme(cx).clone();
    div()
        .flex()
        .items_center()
        .gap_3()
        .pl_8()
        .pr_3()
        .py_1p5()
        .child(
            div()
                .w(px(200.))
                .flex_none()
                .child(DictionaryPage::said(entry.spoken.as_deref(), cx)),
        )
        .child(arrow(false, cx))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_size(metrics::TEXT_SM)
                .when(entry.phrase, |d| d.line_clamp(3).overflow_hidden())
                .when(!entry.phrase, |d| d.truncate())
                .when(looks_like_code(&entry.written), |d| {
                    d.font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_BASE)
                })
                .child(entry.written.clone()),
        )
}
