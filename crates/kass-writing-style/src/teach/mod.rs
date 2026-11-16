//! Teaching a style how the user writes by replying to conversations
//! (docs/plans/TEACH_BY_REPLYING.md). Closing with replies saves them, the
//! same as Finish.

mod conversation;
mod sidebar;
mod summary;

use crate::dictation::TakeState;
use crate::model::MAX_REPLY;
use crate::page::{WritingStylePage, toast_error};
use crate::widgets::spinner;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_api::types::{TeachFinishResult, TeachKind, TeachSession, WritingStyle};
use kass_i18n::tr;
use kass_ui::{Toast, metrics, show_toast, theme};
use std::collections::HashSet;

/// Which request is running, for the buttons it disables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Busy {
    #[default]
    None,
    Starting,
    Adding,
    /// A new theme or wrapping up.
    Switching,
    Sending,
    Finishing,
}

pub struct TeachView {
    client: ApiClient,
    style: WritingStyle,
    page: WeakEntity<WritingStylePage>,
    dictation: bool,
    hold_keys: Vec<String>,
    hands_free_keys: Vec<String>,
    session: Option<TeachSession>,
    selected: Option<String>,
    result: Option<TeachFinishResult>,
    busy: Busy,
    /// The reply just sent and its turn, shown until the answer comes back with it.
    sent: Option<(String, usize)>,
    reply: Entity<TextareaState>,
    /// The conversation and turn the reply box was last emptied for.
    reply_for: Option<(String, usize)>,
    reply_had_text: bool,
    /// Text to put in the reply box on the next render, which has the window.
    dictated: Option<String>,
    expanded: HashSet<String>,
    take: TakeState,
    thread: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

/// Opens the teach dialog for `style` and starts a session.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_teach(
    client: ApiClient,
    style: WritingStyle,
    page: WeakEntity<WritingStylePage>,
    dictation: bool,
    hold_keys: Vec<String>,
    hands_free_keys: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TeachView> {
    let view = cx.new(|cx| {
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder(tr!("writing-style-teach-reply-placeholder"))
        });
        let subscriptions = vec![cx.subscribe_in(&reply, window, TeachView::reply_input)];
        TeachView {
            client,
            style,
            page,
            dictation,
            hold_keys,
            hands_free_keys,
            session: None,
            selected: None,
            result: None,
            busy: Busy::None,
            sent: None,
            reply,
            reply_for: None,
            reply_had_text: false,
            dictated: None,
            expanded: HashSet::new(),
            take: TakeState::Idle,
            thread: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    });
    view.update(cx, |view, cx| view.start(window, cx));

    let shown = view.clone();
    window.open_dialog(cx, move |dialog, window, cx| {
        let summary = shown.read(cx).result.is_some();
        let height = (window.viewport_size().height * 0.9).min(px(760.));
        dialog
            .w(px(if summary { 660. } else { 1040. }))
            .p_0()
            .close_button(false)
            .overlay_closable(false)
            .keyboard(false)
            .child(
                div()
                    .when(!summary, |el| el.h(height))
                    .when(summary, |el| el.max_h(height))
                    .child(shown.clone()),
            )
    });
    view
}

impl TeachView {
    fn call<T: Send + 'static>(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        request: impl FnOnce(&ApiClient) -> kass_api::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let client = self.client.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { request(&client).map_err(|e| e.to_string()) })
                .await;
            view.update_in(cx, |view, window, cx| done(view, result, window, cx))
                .ok();
        })
        .detach();
    }

    /// Runs a request that answers with the session, then shows it.
    /// `select_new` selects the conversation it opened.
    fn update_session(
        &mut self,
        busy: Busy,
        select_new: bool,
        window: &Window,
        cx: &mut Context<Self>,
        request: impl FnOnce(&ApiClient, &str) -> kass_api::Result<TeachSession> + Send + 'static,
    ) {
        let Some(session_id) = self.session.as_ref().map(|s| s.session_id.clone()) else {
            return;
        };
        self.busy = busy;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| request(client, &session_id),
            move |view, result, window, cx| {
                view.busy = Busy::None;
                match result {
                    Ok(next) => view.show(next, select_new),
                    Err(error) => toast_error("writing-style-teach-failed", error, window, cx),
                }
                cx.notify();
            },
        );
    }

    fn show(&mut self, next: TeachSession, select_new: bool) {
        if select_new {
            let before: HashSet<&str> = self
                .session
                .iter()
                .flat_map(|s| s.conversations.iter().map(|c| c.id.as_str()))
                .collect();
            if let Some(new) = next.conversations.iter().find(|c| !before.contains(c.id.as_str())) {
                self.selected = Some(new.id.clone());
            }
        }
        self.session = Some(next);
    }

    fn start(&mut self, window: &Window, cx: &mut Context<Self>) {
        let style = self.style.id.clone();
        self.busy = Busy::Starting;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.start_teach(Some(&style)),
            |view, result, window, cx| {
                view.busy = Busy::None;
                match result {
                    Ok(next) => {
                        view.result = None;
                        view.selected = next.conversations.first().map(|c| c.id.clone());
                        view.reply_for = None;
                        view.session = Some(next);
                    }
                    Err(error) => {
                        toast_error("writing-style-teach-failed", error, window, cx);
                        if view.session.is_none() && view.result.is_none() {
                            view.dismiss(window, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
    }

    fn add(&mut self, kind: TeachKind, window: &mut Window, cx: &mut Context<Self>) {
        self.update_session(Busy::Adding, true, window, cx, move |client, session| {
            client.add_teach_conversation(session, kind)
        });
    }

    fn conversation_id(&self) -> Option<String> {
        self.conversation().map(|c| c.id.clone())
    }

    /// A fresh conversation of the same kind; one with replies is wrapped up and kept.
    fn new_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.conversation_id() else {
            return;
        };
        self.update_session(Busy::Switching, true, window, cx, move |client, session| {
            client.new_teach_theme(session, &id)
        });
    }

    fn wrap_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.conversation_id() else {
            return;
        };
        self.update_session(Busy::Switching, false, window, cx, move |client, session| {
            client.wrap_teach_conversation(session, &id)
        });
    }

    fn can_send(&self, cx: &App) -> bool {
        !self.reply.read(cx).value().trim().is_empty()
            && self.busy != Busy::Sending
            && self.take == TakeState::Idle
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        let (Some(session), Some(conversation)) = (self.session.as_ref(), self.conversation())
        else {
            return;
        };
        let session_id = session.session_id.clone();
        let id = conversation.id.clone();
        let turn = conversation.messages.len();
        let text = self.reply.read(cx).value().trim().to_string();
        self.sent = Some((text.clone(), turn));
        self.reply_had_text = false;
        self.reply.update(cx, |reply, cx| reply.set_value("", window, cx));
        self.busy = Busy::Sending;
        cx.notify();
        let written = text.clone();
        self.call(
            window,
            cx,
            move |client| client.send_teach_reply(&session_id, &id, &written),
            move |view, result, window, cx| {
                view.busy = Busy::None;
                match result {
                    Ok(next) => view.show(next, false),
                    Err(error) => {
                        toast_error("writing-style-teach-failed", error, window, cx);
                        // Not saved: put it back to try again.
                        view.reply_had_text = true;
                        view.reply.update(cx, |reply, cx| reply.set_value(text, window, cx));
                    }
                }
                cx.notify();
            },
        );
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_then(false, window, cx);
    }

    /// Saves the replies. Closing shows how many were saved and closes the
    /// dialog; Finish shows the summary.
    fn finish_then(&mut self, closing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.session.as_ref().map(|s| s.session_id.clone()) else {
            return;
        };
        self.busy = Busy::Finishing;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.finish_teach(&session_id),
            move |view, result, window, cx| {
                view.busy = Busy::None;
                match result {
                    Ok(finished) => {
                        view.session = None;
                        view.page.update(cx, |page, cx| page.teach_done(true, cx)).ok();
                        if closing {
                            show_toast(
                                window,
                                cx,
                                Toast::new(tr!(
                                    "writing-style-teach-saved-on-close",
                                    count = finished.replies
                                )),
                            );
                            view.dismiss(window, cx);
                        } else {
                            view.result = Some(finished);
                        }
                    }
                    Err(error) => toast_error("writing-style-teach-failed", error, window, cx),
                }
                cx.notify();
            },
        );
    }

    /// Closing keeps the replies made so far; with none, the session is thrown away.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.session.as_ref() {
            Some(session) if session.replies > 0 => self.finish_then(true, window, cx),
            Some(session) => {
                let client = self.client.clone();
                let id = session.session_id.clone();
                cx.background_executor()
                    .spawn(async move { client.discard_teach(&id).ok() })
                    .detach();
                self.dismiss(window, cx);
            }
            None => self.dismiss(window, cx),
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.take, TakeState::Listening { from_button: true }) {
            self.page.update(cx, |page, cx| page.teach_take(false, cx)).ok();
        }
        self.page.update(cx, |page, _| page.teach_closed()).ok();
        window.close_dialog(cx);
    }

    fn use_learned(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page
            .update(cx, |page, cx| page.use_learned(window, cx))
            .ok();
        self.dismiss(window, cx);
    }

    fn conversation(&self) -> Option<&kass_api::types::TeachConversation> {
        let session = self.session.as_ref()?;
        session
            .conversations
            .iter()
            .find(|c| Some(&c.id) == self.selected.as_ref())
            .or_else(|| session.conversations.first())
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id);
        cx.notify();
    }

    fn reply_input(
        &mut self,
        _: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                crate::widgets::limit_chars(&self.reply, MAX_REPLY, window, cx);
                let has_text = !self.reply.read(cx).value().trim().is_empty();
                // Cleared out: what was dictated before no longer belongs to this reply.
                if self.reply_had_text && !has_text {
                    self.restart_turn(cx);
                }
                self.reply_had_text = has_text;
                cx.notify();
            }
            InputEvent::PressEnter { secondary: true, .. } => self.send(window, cx),
            _ => {}
        }
    }

    fn restart_turn(&self, cx: &mut Context<Self>) {
        let (Some(session), Some(id)) = (self.session.as_ref(), self.conversation_id()) else {
            return;
        };
        let client = self.client.clone();
        let session = session.session_id.clone();
        cx.background_executor()
            .spawn(async move { client.restart_teach_turn(&session, &id).ok() })
            .detach();
    }

    /// Each step of a take. A take from this dialog's Dictate button ends
    /// with its cleanup fetched into the reply box.
    pub(crate) fn on_take(&mut self, state: TakeState, ours: bool, cx: &mut Context<Self>) {
        if state == TakeState::ButtonDone {
            self.take = TakeState::Idle;
            if ours {
                self.fetch_dictated(cx);
            }
        } else {
            self.take = state;
        }
        cx.notify();
    }

    fn fetch_dictated(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(id)) = (self.session.as_ref(), self.conversation_id()) else {
            return;
        };
        let client = self.client.clone();
        let session = session.session_id.clone();
        cx.spawn(async move |view, cx| {
            let text = cx
                .background_executor()
                .spawn(async move { client.teach_dictated(&session, &id).ok().and_then(|d| d.text) })
                .await;
            if let Some(text) = text {
                view.update(cx, |view, cx| {
                    view.dictated = Some(text);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn dictate(&mut self, start: bool, cx: &mut Context<Self>) {
        self.page.update(cx, |page, cx| page.teach_take(start, cx)).ok();
    }

    /// Empties the reply box for a new conversation or turn, and adds
    /// dictated text; both need the window, so render does them.
    fn sync_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self
            .conversation()
            .map(|c| (c.id.clone(), c.messages.len()))
        else {
            return;
        };
        if self.reply_for.as_ref() != Some(&key) {
            self.reply_for = Some(key);
            self.reply_had_text = false;
            self.reply.update(cx, |reply, cx| {
                reply.set_value("", window, cx);
                reply.focus(window, cx);
            });
            self.thread.scroll_to_bottom();
        }
        if let Some(said) = self.dictated.take() {
            let current = self.reply.read(cx).value().to_string();
            let joined = if current.trim().is_empty() {
                said
            } else {
                format!("{} {said}", current.trim_end())
            };
            let joined: String = joined.chars().take(MAX_REPLY).collect();
            self.reply_had_text = !joined.trim().is_empty();
            self.reply.update(cx, |reply, cx| {
                reply.set_value(joined, window, cx);
                reply.focus(window, cx);
            });
        }
    }
}

impl Render for TeachView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_reply(window, cx);
        let t = theme(cx).clone();

        if let Some(result) = self.result.clone() {
            return self.render_summary(&result, cx).into_any_element();
        }

        let Some(session) = self.session.clone() else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(t.muted_foreground)
                .child(spinner("teach-starting", px(20.), t.muted_foreground))
                .child(
                    div()
                        .text_size(metrics::TEXT_XS)
                        .child(tr!("writing-style-teach-starting")),
                )
                .into_any_element();
        };
        let Some(conversation) = self.conversation().cloned() else {
            return div().into_any_element();
        };

        div()
            .size_full()
            .flex()
            .overflow_hidden()
            .child(self.render_sidebar(&session, &conversation.id, cx))
            .child(self.render_conversation(&session, &conversation, cx))
            .into_any_element()
    }
}
