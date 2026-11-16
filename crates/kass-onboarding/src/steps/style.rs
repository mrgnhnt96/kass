//! Teaching Kass how the user writes: they answer a text in their own words,
//! check Kass's version and edit it until it's how they'd send it, then see
//! their way beside standard cleanup. It runs on a teach-by-replying session,
//! so the reply and every edit are saved as examples when they go on.

use crate::flow::{heard_parts, standard_cleanup};
use crate::motion::{self, Frame};
use crate::page::{Onboarding, StylePhase, StyleState, StyleTake, TakePhase};
use crate::poster::{
    self, HeadlineSize, Kind, Poster, actions, button, headline, lead, speak_row, status_line,
};
use crate::steps::spoken::typed_parts;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_api::types::{CaptureResponse, CaptureSource, TeachConversation, TeachKind, TeachSession};
use kass_i18n::{t, tr};
use std::time::Instant;

/// Up to this many characters (quotes included), the incoming message fits
/// two lines at the biggest size, or three at the middle one; longer ones
/// get the smallest.
const SHORT_MESSAGE: usize = 56;
const MEDIUM_MESSAGE: usize = 110;
const EDIT_INK: u32 = 0x1D1B19;

fn kind_key(kind: TeachKind) -> &'static str {
    match kind {
        TeachKind::CodingAgent => "coding-agent",
        TeachKind::DesignFeedback => "design-feedback",
        TeachKind::Notes => "notes",
        TeachKind::Writeup => "writeup",
        TeachKind::TeamChat => "team-chat",
        TeachKind::IssueComment => "issue-comment",
        TeachKind::Email => "email",
        TeachKind::TextMessage | TeachKind::Unknown => "text-message",
    }
}

/// The conversation of `kind` that hasn't been answered yet, adding one if
/// needed; only dictation from here on belongs to its reply.
fn open_conversation(
    api: &ApiClient,
    session: TeachSession,
    kind: TeachKind,
) -> Result<TeachSession, String> {
    let fresh = |s: &TeachSession| {
        s.conversations
            .iter()
            .find(|c| c.kind == kind && c.reply_count == 0 && !c.wrapped)
            .map(|c| c.id.clone())
    };
    let mut session = session;
    let mut conversation = fresh(&session);
    if conversation.is_none() {
        session = api
            .add_teach_conversation(&session.session_id, kind)
            .map_err(|e| e.to_string())?;
        conversation = fresh(&session);
    }
    let Some(conversation) = conversation else {
        return Err(format!("No {kind:?} conversation to answer."));
    };
    api.restart_teach_turn(&session.session_id, &conversation)
        .map_err(|e| e.to_string())?;
    // The opened one goes first, so the page knows which it is.
    session.conversations.sort_by_key(|c| c.id != conversation);
    Ok(session)
}

fn label(text: impl Into<String>) -> Div {
    div()
        .font_family(kass_ui::metrics::FONT_MONO)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into().to_uppercase())
}

impl Onboarding {
    fn conversation(&self) -> Option<&TeachConversation> {
        self.style.session.as_ref()?.conversations.first()
    }

    fn style_ids(&self) -> Option<(String, String)> {
        let session = self.style.session.as_ref()?;
        let conversation = session.conversations.first()?;
        Some((session.session_id.clone(), conversation.id.clone()))
    }

    pub(crate) fn enter_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.style = StyleState::new();
        self.reply_field
            .update(cx, |field, cx| field.set_value("", window, cx));
        let api = self.api.clone();
        cx.spawn_in(window, async move |page, cx| {
            let opened = cx
                .background_executor()
                .spawn({
                    let api = api.clone();
                    async move {
                        let started = api.start_teach(None).map_err(|e| e.to_string())?;
                        open_conversation(&api, started, TeachKind::TextMessage)
                    }
                })
                .await;
            let kept = page.update_in(cx, |page, window, cx| {
                match opened {
                    Ok(session) if page.progress.step == crate::flow::Step::Style => {
                        page.style.open = Some((session.session_id.clone(), 0));
                        page.style.session = Some(session);
                        page.reply_field.update(cx, |f, cx| f.focus(window, cx));
                    }
                    Ok(session) => return Some(session.session_id),
                    Err(error) => page.style.error = Some(error),
                }
                cx.notify();
                None
            });
            // Left the step before it opened: nothing is kept.
            if let Ok(Some(left)) = kept {
                cx.background_executor()
                    .spawn(async move {
                        let _ = api.discard_teach(&left);
                    })
                    .await;
            }
        })
        .detach();
    }

    /// Leaving without going on: nothing is saved.
    pub(crate) fn leave_style(&mut self, cx: &mut Context<Self>) {
        if let Some((session, _)) = self.style.open.take() {
            let api = self.api.clone();
            cx.background_executor()
                .spawn(async move {
                    let _ = api.discard_teach(&session);
                })
                .detach();
        }
    }

    /// On to the next step: the replies are saved once there's one.
    fn finish_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((session, replies)) = self.style.open.take() {
            let api = self.api.clone();
            cx.background_executor()
                .spawn(async move {
                    if replies > 0 {
                        let _ = api.finish_teach(&session);
                    } else {
                        let _ = api.discard_teach(&session);
                    }
                })
                .detach();
        }
        self.next(window, cx);
    }

    fn style_phase(&mut self, phase: StylePhase, window: &mut Window, cx: &mut Context<Self>) {
        self.style.phase = phase;
        self.style.at = Instant::now();
        match phase {
            StylePhase::Reply => self.reply_field.update(cx, |field, cx| {
                field.set_value("", window, cx);
                field.focus(window, cx);
            }),
            StylePhase::Edit => {
                let written = self.style.take.written.clone();
                self.edit_field.update(cx, |field, cx| {
                    field.set_value(written, window, cx);
                    field.focus(window, cx);
                });
            }
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn style_captured(
        &mut self,
        capture: &CaptureResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.style.phase != StylePhase::Reply || capture.source != CaptureSource::Dictation {
            return;
        }
        let Some((session, conversation)) = self.style_ids() else {
            return;
        };
        let heard = [
            self.style.take.heard.as_str(),
            capture.transcript_raw.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
        let fallback = capture
            .transcript_refined
            .clone()
            .unwrap_or_else(|| capture.transcript_raw.clone());
        let api = self.api.clone();
        cx.spawn_in(window, async move |page, cx| {
            // Every take this turn, cleaned up together, is what Kass shows.
            let dictated = cx
                .background_executor()
                .spawn(async move { api.teach_dictated(&session, &conversation) })
                .await;
            page.update_in(cx, |page, window, cx| {
                match dictated {
                    Ok(dictated) => {
                        let shown = dictated.text.unwrap_or(fallback).trim().to_string();
                        if !shown.is_empty() && page.style.phase == StylePhase::Reply {
                            page.style.take = StyleTake {
                                heard,
                                written: shown.clone(),
                                shown,
                            };
                            page.style_phase(StylePhase::Check, window, cx);
                        }
                    }
                    Err(error) => page.style.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The typed reply, checked like a dictated one.
    pub(crate) fn use_typed_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.reply_field.read(cx).value().trim().to_string();
        if typed.is_empty() || self.style.phase != StylePhase::Reply {
            return;
        }
        self.style.take = StyleTake {
            heard: String::new(),
            shown: typed.clone(),
            written: typed,
        };
        self.style_phase(StylePhase::Check, window, cx);
    }

    pub(crate) fn send_edited(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let written = self.edit_field.read(cx).value().to_string();
        self.send_reply(written, window, cx);
    }

    fn send_reply(&mut self, written: String, window: &mut Window, cx: &mut Context<Self>) {
        let written = written.trim().to_string();
        let Some((session, conversation)) = self.style_ids() else {
            return;
        };
        if written.is_empty() || self.style.busy {
            return;
        }
        self.style.busy = true;
        self.style.error = None;
        let api = self.api.clone();
        let sent = written.clone();
        cx.spawn_in(window, async move |page, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api.send_teach_reply(&session, &conversation, &sent) })
                .await;
            page.update_in(cx, |page, window, cx| {
                page.style.busy = false;
                match result {
                    Ok(next) => {
                        page.keep_opened(next);
                        page.style.take.written = written;
                        if let Some((_, replies)) = page.style.open.as_mut() {
                            *replies += 1;
                        }
                        page.style_phase(StylePhase::Reveal, window, cx);
                    }
                    Err(error) => page.style.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps a session the server sent back, the open conversation still first.
    fn keep_opened(&mut self, mut next: TeachSession) {
        if let Some((_, id)) = self.style_ids() {
            next.conversations.sort_by_key(|c| c.id != id);
        }
        self.style.session = Some(next);
    }

    fn say_again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((session, conversation)) = self.style_ids() {
            let api = self.api.clone();
            cx.background_executor()
                .spawn(async move {
                    let _ = api.restart_teach_turn(&session, &conversation);
                })
                .detach();
        }
        self.style.take = StyleTake::default();
        self.style_phase(StylePhase::Reply, window, cx);
    }

    fn try_work(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.style.session.clone() else {
            return;
        };
        self.style.busy = true;
        self.style.error = None;
        let api = self.api.clone();
        cx.spawn_in(window, async move |page, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move { open_conversation(&api, session, TeachKind::TeamChat) })
                .await;
            page.update_in(cx, |page, window, cx| {
                page.style.busy = false;
                match opened {
                    Ok(session) => {
                        page.style.session = Some(session);
                        page.style.tried_work = true;
                        page.style.take = StyleTake::default();
                        page.style_phase(StylePhase::Reply, window, cx);
                    }
                    Err(error) => page.style.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn style_status(&self) -> Option<SharedString> {
        if self.style.busy || self.take == TakePhase::Working {
            Some(t("onboarding-working").into())
        } else {
            self.style
                .error
                .clone()
                .or_else(|| self.take_error.clone())
                .map(Into::into)
        }
    }

    pub(crate) fn render_style(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match self.style.phase {
            StylePhase::Reply => self.render_style_reply(poster, frame, cx),
            StylePhase::Check | StylePhase::Edit => self.render_style_check(poster, frame, cx),
            StylePhase::Reveal => self.render_style_reveal(poster, frame, cx),
        }
    }

    fn render_style_reply(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let conversation = self.conversation().cloned();
        let persona = conversation
            .as_ref()
            .map(|c| c.persona.clone())
            .unwrap_or_default();
        let kind_line = conversation
            .as_ref()
            .map(|c| {
                format!(
                    "{persona} · {}",
                    t(&format!(
                        "writing-style-teach-kinds-{}-short",
                        kind_key(c.kind)
                    ))
                )
            })
            .unwrap_or_else(|| " ".into());
        let incoming = conversation
            .as_ref()
            .and_then(|c| c.messages.iter().rev().find(|m| !m.from_you))
            .map(|m| format!("“{}”", m.text));
        let length = incoming.as_ref().map_or(0, |m| m.chars().count());
        // Room for three lines at the smaller sizes; a short message gets the biggest.
        let (size, line) = if length <= SHORT_MESSAGE {
            (px(44.), px(46.))
        } else if length <= MEDIUM_MESSAGE {
            (px(34.), px(36.))
        } else {
            (px(28.), px(34.))
        };
        let message = poster::display()
            .flex_none()
            .h(px(108.))
            .max_w(px(720.))
            .overflow_hidden()
            .text_size(size)
            .line_height(line)
            .children(incoming);
        let note = conversation.as_ref().and_then(|c| c.note.clone());
        let body = match note {
            Some(note) if note.facts.is_some() => tr!(
                "onboarding-style-tell",
                persona = persona.clone(),
                facts = note.facts.unwrap_or_default()
            ),
            Some(note) if !note.answers.is_empty() => note
                .answers
                .iter()
                .map(|a| format!("{} → {}", a.ask, a.answer))
                .collect::<Vec<_>>()
                .join(" · "),
            _ => tr!(
                "onboarding-style-body",
                persona = if persona.is_empty() {
                    t("onboarding-style-them")
                } else {
                    persona.clone()
                }
            ),
        };
        let typed = !self.reply_field.read(cx).value().trim().is_empty();
        let use_typed = button(
            "use-typed",
            t("onboarding-style-use-typed"),
            Kind::Primary,
            poster,
            cx.listener(|page, _, window, cx| page.use_typed_reply(window, cx)),
        );
        // Held in place while empty, so the field keeps its width.
        let field_row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .max_w(px(760.))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.reply_field)
                        .h(px(48.))
                        .px_4()
                        .rounded_full()
                        .border(px(1.5))
                        .border_color(white().opacity(0.6))
                        .bg(white().opacity(0.1))
                        .text_color(white())
                        .text_size(px(15.)),
                ),
            )
            .child(if typed {
                div().child(use_typed)
            } else {
                poster::hidden(use_typed)
            });
        let replies = self.style.open.as_ref().map_or(0, |(_, n)| *n);
        let speak = speak_row(
            "dictate",
            &self.push_keys(),
            self.take == TakePhase::Listening,
            conversation.is_none(),
            poster,
            frame,
            self.take_at,
            cx.listener(|page, _, _, cx| page.toggle_take(cx)),
        );
        vec![
            label(kind_line).opacity(0.75).into_any_element(),
            message.into_any_element(),
            lead(body).into_any_element(),
            field_row.into_any_element(),
            speak.into_any_element(),
            status_line(self.style_status()).into_any_element(),
            div()
                .ml(px(-14.))
                .child(actions().child(button(
                    "skip",
                    if replies > 0 {
                        t("onboarding-next")
                    } else {
                        t("onboarding-skip")
                    },
                    Kind::Ghost,
                    poster,
                    cx.listener(|page, _, window, cx| page.finish_style(window, cx)),
                )))
                .into_any_element(),
        ]
    }

    fn render_style_check(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let editing = self.style.phase == StylePhase::Edit;
        let persona = self
            .conversation()
            .map(|c| c.persona.clone())
            .unwrap_or_default();
        let shown = self.style.take.shown.clone();
        let busy = self.style.busy;
        let reply = if editing {
            div()
                .flex_none()
                .max_w(px(700.))
                .child(
                    Textarea::new(&self.edit_field)
                        .w_full()
                        .px(px(18.))
                        .py_4()
                        .rounded(px(18.))
                        .bg(white())
                        .text_color(rgb(EDIT_INK))
                        .text_size(px(28.))
                        .font_weight(FontWeight::BOLD),
                )
                .into_any_element()
        } else {
            motion::pop(
                poster::display()
                    .max_w(px(700.))
                    .max_h(px(134.))
                    .overflow_hidden()
                    .rounded(px(18.))
                    .bg(white().opacity(0.12))
                    .px(px(18.))
                    .py_4()
                    .text_size(px(28.))
                    .line_height(px(34.))
                    .child(shown.clone()),
                frame,
                self.style.at,
                0.,
            )
            .into_any_element()
        };
        let mut pieces = vec![
            label(tr!("onboarding-style-your-reply-to", persona = persona))
                .opacity(0.75)
                .into_any_element(),
            headline(t("onboarding-style-check-title"), HeadlineSize::Md).into_any_element(),
            lead(if editing {
                t("onboarding-style-edit-body")
            } else {
                t("onboarding-style-check-body")
            })
            .into_any_element(),
            reply,
        ];
        if editing {
            let written = self.edit_field.read(cx).value().to_string();
            let changed = motion::words_row().gap_x(px(4.)).children(
                heard_parts(&written, &shown).into_iter().map(|part| {
                    if part.dropped {
                        div()
                            .flex_none()
                            .rounded(px(4.))
                            .bg(white())
                            .px(px(3.))
                            .text_color(poster.bg)
                            .child(part.text)
                    } else {
                        div().flex_none().child(part.text)
                    }
                }),
            );
            pieces.push(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .max_w(px(700.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(14.))
                            .child(
                                label(t("onboarding-style-kass-wrote"))
                                    .w(px(130.))
                                    .flex_none()
                                    .opacity(0.75),
                            )
                            .child(
                                div()
                                    .max_h(px(42.))
                                    .overflow_hidden()
                                    .text_size(px(15.))
                                    .opacity(0.75)
                                    .child(shown),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(14.))
                            .child(
                                label(t("onboarding-style-you-changed"))
                                    .w(px(130.))
                                    .flex_none(),
                            )
                            .child(
                                div()
                                    .max_h(px(48.))
                                    .overflow_hidden()
                                    .text_size(px(15.))
                                    .line_height(px(24.))
                                    .child(changed),
                            ),
                    )
                    .into_any_element(),
            );
        }
        pieces.push(status_line(self.style_status()).into_any_element());
        let row = if editing {
            actions()
                .child(button(
                    "done",
                    t("onboarding-style-done"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.send_edited(window, cx)),
                ))
                .child(button(
                    "cancel",
                    t("onboarding-style-cancel"),
                    Kind::Ghost,
                    poster,
                    cx.listener(|page, _, window, cx| {
                        page.style.take.written = page.style.take.shown.clone();
                        page.style_phase(StylePhase::Check, window, cx);
                    }),
                ))
        } else {
            let disabled = |b: Stateful<Div>| if busy { b.opacity(0.6) } else { b };
            actions()
                .child(disabled(button(
                    "looks-right",
                    t("onboarding-style-looks-right"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| {
                        let shown = page.style.take.shown.clone();
                        page.send_reply(shown, window, cx);
                    }),
                )))
                .child(disabled(
                    button(
                        "edit",
                        t("onboarding-style-edit"),
                        Kind::Outline,
                        poster,
                        cx.listener(|page, _, window, cx| {
                            if !page.style.busy {
                                page.style_phase(StylePhase::Edit, window, cx);
                            }
                        }),
                    )
                    .child(poster::icon(IconName::Pencil, px(16.), poster.fg)),
                ))
                .child(disabled(button(
                    "say-again",
                    t("onboarding-style-say-again"),
                    Kind::Ghost,
                    poster,
                    cx.listener(|page, _, window, cx| {
                        if !page.style.busy {
                            page.say_again(window, cx);
                        }
                    }),
                )))
        };
        pieces.push(row.into_any_element());
        pieces
    }

    fn render_style_reveal(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let take = self.style.take.clone();
        let standard = standard_cleanup(&take.shown);
        let at = self.style.at;
        let row = |name: Div, value: AnyElement| {
            div()
                .flex()
                .items_baseline()
                .gap(px(14.))
                .child(name.w(px(150.)).flex_none())
                .child(div().flex_1().min_w_0().child(value))
        };
        let mut grid = div().flex().flex_col().gap(px(10.)).max_w(px(740.));
        if !take.heard.is_empty() {
            grid = grid.child(row(
                label(t("onboarding-style-said")).opacity(0.75),
                div()
                    .max_h(px(42.))
                    .overflow_hidden()
                    .text_size(px(15.))
                    .line_height(px(21.))
                    .child(typed_parts(
                        &heard_parts(&take.heard, &take.shown),
                        poster.fg,
                        frame,
                        at,
                        |_| 250.,
                        0.75,
                    ))
                    .into_any_element(),
            ));
        }
        if standard != take.written {
            grid = grid.child(row(
                label(t("onboarding-style-standard")).opacity(0.75),
                div()
                    .max_h(px(42.))
                    .overflow_hidden()
                    .text_size(px(15.))
                    .line_height(px(21.))
                    .opacity(0.75)
                    .child(standard)
                    .into_any_element(),
            ));
        }
        grid = grid.child(
            div()
                .border_t(px(1.5))
                .border_color(poster.fg)
                .pt(px(14.))
                .child(row(
                    label(t("onboarding-style-your-way")),
                    motion::rise_late(
                        poster::display()
                            .max_h(px(104.))
                            .overflow_hidden()
                            .text_size(px(30.))
                            .line_height(px(34.))
                            .child(take.written.clone()),
                        frame,
                        at,
                        900.,
                        false,
                    )
                    .into_any_element(),
                )),
        );
        let size = poster::headline_size(HeadlineSize::Lg);
        let tried = self.style.tried_work;
        let try_work = button(
            "try-work",
            t("onboarding-style-try-work"),
            Kind::Outline,
            poster,
            cx.listener(|page, _, window, cx| {
                if !page.style.busy {
                    page.try_work(window, cx);
                }
            }),
        );
        vec![
            poster::display()
                .text_size(size)
                .line_height(size * 1.04)
                .child(motion::rise_letters(
                    &t("onboarding-style-reveal-title"),
                    frame,
                    at,
                    0.,
                    size,
                ))
                .into_any_element(),
            grid.into_any_element(),
            status_line(self.style_status()).into_any_element(),
            actions()
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.finish_style(window, cx)),
                ))
                .child(if tried {
                    poster::hidden(try_work)
                } else {
                    div().child(try_work)
                })
                .into_any_element(),
            div()
                .max_w(px(520.))
                .text_size(px(13.))
                .opacity(0.75)
                .child(if tried {
                    t("onboarding-style-more-later")
                } else {
                    t("onboarding-style-more")
                })
                .into_any_element(),
        ]
    }
}
