//! The Writing Style page: the styles and which apps use each, the chosen
//! style's settings, teaching it, what it learns from, and its recent
//! corrections.

mod board;
mod learn;
mod style;

use crate::dictation::{StyleDictation, TakeState};
use crate::model::{AppFilter, MAX_NAME, chosen_style};
use crate::teach::TeachView;
use crate::widgets::spinner;
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_api::types::{
    CaptureAppFilter, CorrectionNote, PersonalExample, PunctuationStyle, WritingStyle,
    WritingStyleStatus, WritingStylesResponse,
};
use kass_i18n::tr;
use kass_ui::{SelectOptions, Toast, ToastKind, show_toast, theme};
use std::rc::Rc;

/// Whose Dictate button started the current take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TakeOwner {
    Description,
    Teach,
}

/// What the chosen style learned and learns from, as last fetched.
struct StyleData {
    status: Option<WritingStyleStatus>,
    examples: Vec<PersonalExample>,
    notes: Vec<CorrectionNote>,
}

pub struct WritingStylePage {
    client: ApiClient,
    dictation: Option<Rc<dyn StyleDictation>>,
    following: Option<Subscription>,
    take: TakeState,
    take_owner: Option<TakeOwner>,
    data: Option<WritingStylesResponse>,
    load_error: Option<String>,
    status: Option<WritingStyleStatus>,
    examples: Vec<PersonalExample>,
    notes: Vec<CorrectionNote>,
    hold_keys: Vec<String>,
    hands_free_keys: Vec<String>,
    /// The style asked for; the default style when it's `None` or gone.
    wanted: Option<String>,
    /// The style `status`, `examples` and `notes` belong to.
    loaded_for: Option<String>,
    filter: AppFilter,
    search: Entity<InputState>,
    /// The new style's name while one is being added.
    draft: Option<(Entity<InputState>, Subscription)>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    /// Dictated text for the description, set in from the next draw.
    dictated_description: Option<String>,
    punctuation: Entity<SelectState<SelectOptions<PunctuationStyle>>>,
    /// The style the name, description and punctuation show.
    form_for: Option<style::FormFor>,
    teach: Option<WeakEntity<TeachView>>,
    loading: Option<Task<()>>,
    style_loading: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl WritingStylePage {
    pub fn new(client: ApiClient, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(tr!("writing-style-styles-search"))
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("writing-style-styles-name-placeholder"))
                .validate(|text, _| text.chars().count() <= MAX_NAME)
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder(tr!(
                    "writing-style-styles-settings-description-placeholder"
                ))
        });
        let punctuation = cx.new(|cx| style::punctuation_state(None, false, window, cx));

        let subscriptions = vec![
            cx.subscribe_in(&search, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&name, window, |page, _, event, window, cx| {
                page.name_input(event, window, cx)
            }),
            cx.subscribe_in(&description, window, |page, _, event, window, cx| {
                page.description_input(event, window, cx)
            }),
            cx.subscribe_in(
                &punctuation,
                window,
                |page, _, event: &SelectEvent<SelectOptions<PunctuationStyle>>, window, cx| {
                    if let SelectEvent::Confirm(Some(choice)) = event {
                        page.choose_punctuation(*choice, window, cx);
                    }
                },
            ),
        ];

        let mut page = Self {
            client,
            dictation: None,
            following: None,
            take: TakeState::Idle,
            take_owner: None,
            data: None,
            load_error: None,
            status: None,
            examples: Vec::new(),
            notes: Vec::new(),
            hold_keys: Vec::new(),
            hands_free_keys: Vec::new(),
            wanted: None,
            loaded_for: None,
            filter: AppFilter::All,
            search,
            draft: None,
            name,
            description,
            dictated_description: None,
            punctuation,
            form_for: None,
            teach: None,
            loading: None,
            style_loading: None,
            _subscriptions: subscriptions,
        };
        page.refresh(cx);
        page
    }

    /// Lets the page dictate a description or a teach reply.
    pub fn with_dictation(mut self, dictation: Rc<dyn StyleDictation>, cx: &mut Context<Self>) -> Self {
        let page = cx.entity().downgrade();
        self.following = Some(dictation.follow(
            Rc::new(move |state, cx| {
                page.update(cx, |page, cx| page.on_take(state, cx)).ok();
            }),
            cx,
        ));
        self.dictation = Some(dictation);
        self
    }

    /// Fetches everything again: after a change made elsewhere, or when the page is shown.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.loading = Some(cx.spawn(async move |page, cx| {
            let (styles, keys) = cx
                .background_executor()
                .spawn(async move {
                    let styles = client.list_writing_styles().map_err(|e| e.to_string());
                    let keys = client.capture_settings().ok().map(|s| {
                        (s.chord_push_to_talk_keys, s.chord_toggle_to_talk_keys)
                    });
                    (styles, keys)
                })
                .await;
            page.update(cx, |page, cx| {
                match styles {
                    Ok(data) => {
                        page.data = Some(data);
                        page.load_error = None;
                    }
                    Err(error) if page.data.is_none() => page.load_error = Some(error),
                    Err(_) => {}
                }
                if let Some((hold, hands_free)) = keys {
                    page.hold_keys = hold;
                    page.hands_free_keys = hands_free;
                }
                page.loaded_for = None;
                page.load_style(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Opens a style by its id; `None` keeps the one shown.
    pub fn show(&mut self, style: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = style {
            self.choose_style(Some(id), cx);
        }
    }

    fn style(&self) -> Option<&WritingStyle> {
        self.data
            .as_ref()
            .and_then(|data| chosen_style(data, self.wanted.as_deref()))
    }

    fn style_id(&self) -> Option<String> {
        self.style().map(|style| style.id.clone())
    }

    fn style_name(&self) -> String {
        self.style().map(|style| style.name.clone()).unwrap_or_default()
    }

    fn choose_style(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.wanted = id;
        self.load_style(cx);
        cx.notify();
    }

    /// Fetches what the chosen style learned, unless it's already here.
    fn load_style(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.style_id() else {
            return;
        };
        if self.loaded_for.as_deref() == Some(id.as_str()) {
            return;
        }
        if self.loaded_for.is_some() {
            self.status = None;
            self.examples.clear();
            self.notes.clear();
        }
        self.loaded_for = Some(id.clone());
        let client = self.client.clone();
        self.style_loading = Some(cx.spawn(async move |page, cx| {
            let style = id.clone();
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    StyleData {
                        status: client.writing_style(Some(&style)).ok(),
                        examples: client
                            .list_personal_examples(Some(&style))
                            .unwrap_or_default(),
                        notes: client
                            .correction_notes(Some(&style))
                            .map(|notes| notes.notes)
                            .unwrap_or_default(),
                    }
                })
                .await;
            page.update(cx, |page, cx| {
                if page.loaded_for.as_deref() == Some(id.as_str()) {
                    page.status = fetched.status;
                    page.examples = fetched.examples;
                    page.notes = fetched.notes;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Fetches the styles and the chosen style's data again after a change.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
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

    // Dictation

    fn start_take(&mut self, owner: TakeOwner, cx: &mut Context<Self>) {
        if let Some(dictation) = self.dictation.clone() {
            self.take_owner = Some(owner);
            dictation.start(cx);
        }
    }

    fn stop_take(&mut self, cx: &mut Context<Self>) {
        if let Some(dictation) = self.dictation.clone() {
            dictation.stop(cx);
        }
    }

    fn on_take(&mut self, state: TakeState, cx: &mut Context<Self>) {
        let owner = match state {
            TakeState::Listening { from_button: true } | TakeState::Cleaning => self.take_owner,
            TakeState::ButtonDone => self.take_owner.take(),
            _ => {
                self.take_owner = None;
                None
            }
        };
        self.take = state;
        if let Some(teach) = self.teach.as_ref().and_then(|t| t.upgrade()) {
            let ours = owner == Some(TakeOwner::Teach);
            teach.update(cx, |teach, cx| teach.on_take(state, ours, cx));
        }
        if state == TakeState::ButtonDone && owner == Some(TakeOwner::Description) {
            self.take = TakeState::Idle;
            self.fetch_dictated_description(cx);
        }
        cx.notify();
    }

    /// What the take just said, from its capture: refined, else as heard.
    fn fetch_dictated_description(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |page, cx| {
            let text = cx
                .background_executor()
                .spawn(async move {
                    client
                        .list_captures(1, 0, &CaptureAppFilter::All)
                        .ok()
                        .and_then(|list| list.items.into_iter().next())
                        .map(|capture| {
                            capture
                                .transcript_refined
                                .filter(|text| !text.trim().is_empty())
                                .unwrap_or(capture.transcript_raw)
                                .trim()
                                .to_string()
                        })
                })
                .await;
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                page.update(cx, |page, cx| {
                    page.dictated_description = Some(text);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn open_teach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(style) = self.style().cloned() else {
            return;
        };
        let page = cx.entity().downgrade();
        let view = crate::teach::open_teach(
            self.client.clone(),
            style,
            page,
            self.dictation.is_some(),
            self.hold_keys.clone(),
            self.hands_free_keys.clone(),
            window,
            cx,
        );
        self.teach = Some(view.downgrade());
    }

    /// Called by the teach dialog: start or stop its take, or that it closed or saved.
    pub(crate) fn teach_take(&mut self, start: bool, cx: &mut Context<Self>) {
        if start {
            self.start_take(TakeOwner::Teach, cx);
        } else {
            self.stop_take(cx);
        }
    }

    pub(crate) fn teach_done(&mut self, saved: bool, cx: &mut Context<Self>) {
        if saved {
            self.loaded_for = None;
            self.reload(cx);
        }
    }

    pub(crate) fn teach_closed(&mut self) {
        self.teach = None;
    }

    pub(crate) fn use_learned(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_punctuation(PunctuationStyle::Learned, window, cx);
    }

    pub(crate) fn learned_on(&self) -> bool {
        self.style()
            .is_some_and(|style| style.punctuation_style == PunctuationStyle::Learned)
    }
}

pub(crate) fn toast_error(
    title: &'static str,
    error: String,
    window: &mut Window,
    cx: &mut App,
) {
    show_toast(
        window,
        cx,
        Toast::new(kass_i18n::t(title))
            .description(error)
            .kind(ToastKind::Destructive),
    );
}

impl Render for WritingStylePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_form(window, cx);
        if let Some(text) = self.dictated_description.take() {
            self.append_description(text, window, cx);
        }
        let t = theme(cx).clone();

        let Some(data) = self.data.clone() else {
            return div()
                .id("writing-style")
                .flex()
                .items_center()
                .justify_center()
                .py(px(64.))
                .gap_2()
                .text_color(t.muted_foreground)
                .child(spinner("writing-style-loading", px(16.), t.muted_foreground))
                .when_some(self.load_error.clone(), |el, error| el.child(error))
                .into_any_element();
        };

        div()
            .id("writing-style")
            .flex()
            .flex_col()
            .child(self.render_board(&data, window, cx))
            .when(!data.apps.is_empty(), |el| {
                el.child(self.render_app_list(&data, window, cx))
            })
            .when(self.style().is_some(), |el| {
                el.child(self.render_style_settings(&data, window, cx))
                    .child(self.render_calibration(&data, cx))
                    .child(self.render_learns_from(cx))
                    .child(self.render_recent(&data, cx))
            })
            .into_any_element()
    }
}
