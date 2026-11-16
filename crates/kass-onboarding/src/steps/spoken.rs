//! The steps the user talks through: their name, a messy line, and a
//! rewrite of text already written.

use crate::flow::{HeardPart, heard_parts, without_end_punctuation};
use crate::motion::{self, Frame};
use crate::page::{NamePhase, Onboarding, TakePhase, sent_text};
use crate::poster::{
    self, HeadlineSize, Kind, Poster, actions, button, headline, keycaps, lead, speak_row,
    status_line,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureResponse, CaptureSource, DictionaryEntryCreate, DictionaryPlaceInput,
};
use kass_i18n::{t, tr};
use std::time::Instant;

/// After what was said is typed out, each dropped piece is struck in turn.
const STRIKE_START: f32 = 250.;
const STRIKE_EACH: f32 = 320.;

const INSTRUCTIONS: [&str; 3] = ["shorter", "friendlier", "bullets"];
const REWRITE_INK: u32 = 0x1D1B19;

/// What was heard typing out, its dropped pieces struck once they're typed.
pub(crate) fn typed_parts(
    parts: &[HeardPart],
    color: Hsla,
    frame: &Frame,
    start: Instant,
    strike_delay: impl Fn(usize) -> f32,
    kept_opacity: f32,
) -> Div {
    let total: usize = parts
        .iter()
        .map(|p| p.text.chars().count() + 1)
        .sum::<usize>()
        .saturating_sub(1);
    let shown = motion::typed(frame, start, 0., total);
    let typed_ms = motion::typing_ms(total);
    let mut row = motion::words_row().gap_x(px(5.));
    let mut offset = 0usize;
    let mut nth = 0usize;
    for part in parts {
        let len = part.text.chars().count();
        if part.dropped {
            let typed_out = shown >= offset + len;
            row = row.child(if typed_out {
                motion::strike(
                    part.text.clone(),
                    color,
                    px(2.),
                    frame,
                    start,
                    typed_ms + strike_delay(nth),
                )
            } else {
                motion::typed_word(&part.text, shown.saturating_sub(offset))
            });
            nth += 1;
            offset += len + 1;
        } else {
            for word in part.text.split_whitespace() {
                let len = word.chars().count();
                row = row.child(
                    motion::typed_word(word, shown.saturating_sub(offset)).opacity(kept_opacity),
                );
                offset += len + 1;
            }
        }
    }
    row
}

impl Onboarding {
    pub(crate) fn name_captured(
        &mut self,
        capture: &CaptureResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.name.phase != NamePhase::Ask || capture.source != CaptureSource::Dictation {
            return;
        }
        let said = without_end_punctuation(&sent_text(capture));
        if said.is_empty() {
            return;
        }
        self.name.heard = said.clone();
        self.name.name = said.clone();
        self.name.phase = NamePhase::Confirm;
        self.name.at = Instant::now();
        self.name_field
            .update(cx, |field, cx| field.set_value(said, window, cx));
    }

    /// Enter in the name field: in Edit, saves the new spelling.
    pub(crate) fn name_entered(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.name.phase == NamePhase::Edit {
            let spelling = self.name_field.read(cx).value().to_string();
            self.save_name(spelling, true, cx);
        }
    }

    fn save_name(&mut self, spelling: String, fixed: bool, cx: &mut Context<Self>) {
        let written = spelling.trim().to_string();
        if written.is_empty() {
            return;
        }
        let heard = self.name.heard.clone();
        let spoken = (fixed && !heard.is_empty() && heard != written).then_some(heard);
        self.name.name = written.clone();
        self.name.fixed = fixed;
        self.name.phase = NamePhase::Saved;
        self.name.at = Instant::now();
        let api = self.api.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = api.create_dictionary_entry(&DictionaryEntryCreate {
                    written,
                    spoken,
                    places: vec![DictionaryPlaceInput::global()],
                    match_sound: None,
                    phrase: None,
                });
            })
            .detach();
        cx.notify();
    }

    fn name_phase(&mut self, phase: NamePhase, window: &mut Window, cx: &mut Context<Self>) {
        self.name.phase = phase;
        self.name.at = Instant::now();
        if phase == NamePhase::Ask {
            self.name.name.clear();
            self.name_field
                .update(cx, |field, cx| field.set_value("", window, cx));
        }
        if matches!(phase, NamePhase::Ask | NamePhase::Edit) {
            self.name_field
                .update(cx, |field, cx| field.focus(window, cx));
        }
        cx.notify();
    }

    fn speak(
        &self,
        id: &'static str,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Div {
        speak_row(
            id,
            &self.push_keys(),
            self.take == TakePhase::Listening,
            false,
            poster,
            frame,
            self.take_at,
            cx.listener(|page, _, _, cx| page.toggle_take(cx)),
        )
    }

    pub(crate) fn render_name(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let phase = self.name.phase;
        if matches!(phase, NamePhase::Ask | NamePhase::Edit) {
            let field = Input::new(&self.name_field)
                .h(px(80.))
                .w(px(440.))
                .px_4()
                .rounded(px(16.))
                .border(px(3.))
                .border_color(white().opacity(0.7))
                .bg(white().opacity(0.12))
                .text_color(white())
                .text_size(px(52.))
                .font_weight(FontWeight::BOLD);
            let row = div()
                .flex()
                .items_center()
                .gap_4()
                .when(phase == NamePhase::Edit, |row| {
                    row.child(
                        poster::display()
                            .text_size(px(84.))
                            .line_height(px(84.))
                            .child(t("onboarding-name-hi")),
                    )
                })
                .child(field);
            let mut pieces = Vec::new();
            if phase == NamePhase::Ask {
                pieces.push(
                    headline(t("onboarding-name-title"), HeadlineSize::Md).into_any_element(),
                );
                pieces.push(lead(t("onboarding-name-body")).into_any_element());
            } else {
                pieces.push(lead(t("onboarding-name-edit-body")).into_any_element());
            }
            pieces.push(row.into_any_element());
            if phase == NamePhase::Ask {
                pieces.push(self.speak("dictate", poster, frame, cx).into_any_element());
                pieces.push(status_line(self.take_status()).into_any_element());
            } else {
                pieces.push(
                    actions()
                        .child(button(
                            "save-spelling",
                            t("onboarding-name-save-spelling"),
                            Kind::Primary,
                            poster,
                            cx.listener(|page, _, _, cx| {
                                let spelling = page.name_field.read(cx).value().to_string();
                                page.save_name(spelling, true, cx);
                            }),
                        ))
                        .child(button(
                            "say-again",
                            t("onboarding-name-say-again"),
                            Kind::Ghost,
                            poster,
                            cx.listener(|page, _, window, cx| {
                                page.name_phase(NamePhase::Ask, window, cx)
                            }),
                        ))
                        .into_any_element(),
                );
            }
            return pieces;
        }

        // The greeting's letters land first; what follows waits for them.
        let name = self.name.name.clone();
        let greeting = tr!("onboarding-name-greeting", name = name.clone());
        let size = poster::headline_size(HeadlineSize::Xl);
        let after = 200. + greeting.chars().count() as f32 * 30.;
        let title = poster::display()
            .max_w(px(760.))
            .text_size(size)
            .line_height(size * 1.04)
            .child(motion::rise_letters(
                &greeting,
                frame,
                self.name.at,
                0.,
                size,
            ))
            .into_any_element();
        if phase == NamePhase::Confirm {
            let rest = div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_size(px(17.))
                        .child(t("onboarding-name-spelled-right")),
                )
                .child(
                    actions()
                        .child(button(
                            "yes",
                            t("onboarding-name-yes"),
                            Kind::Primary,
                            poster,
                            cx.listener(|page, _, _, cx| {
                                let name = page.name.name.clone();
                                page.save_name(name, false, cx);
                            }),
                        ))
                        .child(button(
                            "fix",
                            t("onboarding-name-fix"),
                            Kind::Outline,
                            poster,
                            cx.listener(|page, _, window, cx| {
                                page.name_phase(NamePhase::Edit, window, cx)
                            }),
                        )),
                );
            return vec![
                title,
                motion::rise_late(rest, frame, self.name.at, after, false).into_any_element(),
            ];
        }
        let saved = if self.name.fixed {
            tr!("onboarding-name-saved-fixed", name = name)
        } else {
            t("onboarding-name-saved")
        };
        let green = Hsla::from(rgb(poster::GREEN));
        let chip = motion::pop(
            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded_full()
                .border(px(1.5))
                .border_color(green)
                .bg(green.opacity(0.2))
                .px(px(14.))
                .py_2()
                .text_size(px(13.))
                .child(poster::icon(IconName::Check, px(14.), poster.fg))
                .child(saved),
            frame,
            self.name.at,
            0.,
        );
        vec![
            title,
            div().flex().child(chip).into_any_element(),
            actions()
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
                .into_any_element(),
        ]
    }

    pub(crate) fn messy_captured(&mut self, capture: &CaptureResponse) {
        if capture.source != CaptureSource::Dictation || self.messy.result.is_some() {
            return;
        }
        // Once the line is said, it stays until "Say it again".
        self.messy.result = Some((capture.transcript_raw.clone(), sent_text(capture)));
        self.messy.at = Instant::now();
    }

    pub(crate) fn render_messy(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // Two lines of what was said and two of what was sent, reserved.
        let said = match &self.messy.result {
            Some((heard, sent)) => {
                let parts = heard_parts(heard, sent);
                let strikes = parts.iter().filter(|p| p.dropped).count();
                let chars = parts
                    .iter()
                    .map(|p| p.text.chars().count() + 1)
                    .sum::<usize>();
                let landed = motion::typing_ms(chars.saturating_sub(1))
                    + STRIKE_START
                    + strikes as f32 * STRIKE_EACH
                    + 150.;
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .max_w(px(700.))
                            .max_h(px(48.))
                            .overflow_hidden()
                            .text_size(px(17.))
                            .line_height(px(23.))
                            .child(typed_parts(
                                &parts,
                                poster.fg,
                                frame,
                                self.messy.at,
                                |nth| STRIKE_START + nth as f32 * STRIKE_EACH,
                                0.85,
                            )),
                    )
                    .child(motion::rise_late(
                        poster::display()
                            .max_w(px(720.))
                            .max_h(px(80.))
                            .overflow_hidden()
                            .text_size(px(32.))
                            .line_height(px(40.))
                            .child(sent.clone()),
                        frame,
                        self.messy.at,
                        landed,
                        false,
                    ))
            }
            None => div().child(
                poster::display()
                    .max_w(px(700.))
                    .text_size(px(26.))
                    .line_height(px(32.))
                    .font_weight(FontWeight::MEDIUM)
                    .child(t("onboarding-messy-line")),
            ),
        };
        let said = div()
            .flex()
            .flex_col()
            .flex_none()
            .h(px(140.))
            .overflow_hidden()
            .child(said);
        // Where to speak until the line is said; then where to go next, in the same space.
        let below = if self.messy.result.is_some() {
            actions()
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
                .child(button(
                    "say-again",
                    t("onboarding-messy-say-again"),
                    Kind::Ghost,
                    poster,
                    cx.listener(|page, _, window, cx| {
                        page.messy.result = None;
                        page.messy_field.update(cx, |field, cx| {
                            field.set_value("", window, cx);
                            field.focus(window, cx);
                        });
                        cx.notify();
                    }),
                ))
        } else {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div().max_w(px(560.)).child(
                        Textarea::new(&self.messy_field)
                            .w_full()
                            .px(px(14.))
                            .py(px(10.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(white().opacity(0.4))
                            .bg(white().opacity(0.1))
                            .text_color(white())
                            .text_size(px(14.)),
                    ),
                )
                .child(self.speak("dictate", poster, frame, cx))
        };
        vec![
            headline(t("onboarding-messy-title"), HeadlineSize::Md).into_any_element(),
            lead(t("onboarding-messy-body")).into_any_element(),
            said.into_any_element(),
            div()
                .flex()
                .flex_col()
                .flex_none()
                .h(px(130.))
                .child(below)
                .into_any_element(),
            status_line(self.take_status()).into_any_element(),
        ]
    }

    pub(crate) fn rewrite_captured(
        &mut self,
        capture: &CaptureResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rewritten = capture
            .transcript_refined
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if capture.source != CaptureSource::Command
            || rewritten.is_empty()
            || self.rewrite.done.is_some()
        {
            return;
        }
        let selection = capture.command_selection.clone().unwrap_or_default();
        let instruction = capture.command_instruction.clone().unwrap_or_default();
        self.finish_rewrite(&selection, &rewritten, instruction, window, cx);
    }

    fn finish_rewrite(
        &mut self,
        selection: &str,
        rewritten: &str,
        instruction: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self.rewrite_field.read(cx).value().to_string();
        let text = if !selection.is_empty() && current.contains(selection) {
            current.replacen(selection, rewritten, 1)
        } else {
            rewritten.to_string()
        };
        self.rewrite_field
            .update(cx, |field, cx| field.set_value(text, window, cx));
        self.rewrite.done = Some(instruction);
        self.rewrite.at = Instant::now();
        cx.notify();
    }

    fn run_rewrite(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewrite.busy {
            return;
        }
        let field = self.rewrite_field.read(cx);
        let selected = field.selected_text().to_string();
        let selection = if selected.is_empty() {
            field.value().to_string()
        } else {
            selected
        };
        let instruction = t(&format!("onboarding-rewrite-instructions-{key}"));
        self.rewrite.busy = true;
        self.rewrite.error = None;
        let api = self.api.clone();
        let (sent, asked) = (selection.clone(), instruction.clone());
        cx.spawn_in(window, async move |page, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { api.run_command(&sent, &asked) })
                .await;
            page.update_in(cx, |page, window, cx| {
                page.rewrite.busy = false;
                match result {
                    Ok(capture) => {
                        let rewritten = capture
                            .transcript_refined
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !rewritten.is_empty() && page.rewrite.done.is_none() {
                            page.finish_rewrite(&selection, &rewritten, instruction, window, cx);
                        }
                    }
                    Err(error) => page.rewrite.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_rewrite(
        &mut self,
        poster: Poster,
        frame: &Frame,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let command_keys = self.chords().command;
        let done = self.rewrite.done.clone();
        let body = if command_keys.is_empty() {
            t("onboarding-rewrite-body-no-keys")
        } else {
            t("onboarding-rewrite-body")
        };
        // The field and the rewritten text in its place: the same size.
        let ink = Hsla::from(rgb(REWRITE_INK));
        let field = Textarea::new(&self.rewrite_field)
            .w_full()
            .h(px(126.))
            .px_4()
            .py(px(14.))
            .rounded(px(16.))
            .bg(white())
            .text_color(ink)
            .text_size(px(15.))
            .disabled(done.is_some());
        let sweep = done.is_some().then(|| {
            // A light swept across the text once it's rewritten.
            let t = frame.progress(self.rewrite.at, 0., 900.);
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(t * 1.4 - 0.4))
                .w(relative(0.4))
                .bg(white().opacity(0.55 * (1. - t)))
        });
        let box_ = div()
            .relative()
            .flex_none()
            .max_w(px(700.))
            .rounded(px(16.))
            .overflow_hidden()
            .child(field)
            .children(sweep);

        let row = if let Some(instruction) = done {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .h(px(32.))
                .child(motion::pop(
                    div().child(poster::icon(IconName::Check, px(16.), poster.fg)),
                    frame,
                    self.rewrite.at,
                    0.,
                ))
                .child(div().text_size(px(13.)).child(if instruction.is_empty() {
                    t("onboarding-rewrite-did-any")
                } else {
                    tr!("onboarding-rewrite-did", instruction = instruction)
                }))
                .child(
                    div()
                        .id("try-another")
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .px(px(10.))
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .underline()
                        .cursor_pointer()
                        .child(t("onboarding-rewrite-try-another"))
                        .on_click(cx.listener(|page, _, window, cx| {
                            page.rewrite.done = None;
                            page.rewrite_field.update(cx, |field, cx| {
                                field.set_value(t("onboarding-rewrite-paragraph"), window, cx);
                                field.focus(window, cx);
                                field.select_all(window, cx);
                            });
                            cx.notify();
                        })),
                )
        } else {
            let busy = self.rewrite.busy;
            let held = self.held.as_deref() == Some("command");
            let mut row = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .min_h(px(32.));
            if !command_keys.is_empty() {
                row = row
                    .child(
                        div()
                            .text_size(px(13.))
                            .opacity(0.8)
                            .child(t("onboarding-hold")),
                    )
                    .child(keycaps(
                        &command_keys,
                        held,
                        poster::CapSize::Sm,
                        false,
                        poster,
                        frame,
                        self.held_at,
                    ))
                    .child(
                        div()
                            .text_size(px(13.))
                            .opacity(0.8)
                            .child(t("onboarding-rewrite-and-say")),
                    );
            }
            row.children(INSTRUCTIONS.map(|key| {
                div()
                    .id(key)
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .rounded_full()
                    .border_1()
                    .border_color(white().opacity(0.4))
                    .px(px(14.))
                    .text_size(px(13.))
                    .when(busy, |b| b.opacity(0.6))
                    .when(!busy, |b| {
                        b.cursor_pointer()
                            .hover(|s| s.border_color(white()))
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.run_rewrite(key, window, cx)
                            }))
                    })
                    .child(format!(
                        "“{}”",
                        t(&format!("onboarding-rewrite-instructions-{key}"))
                    ))
            }))
        };
        let status = if self.rewrite.busy {
            Some(t("onboarding-working").into())
        } else {
            self.rewrite.error.clone().map(Into::into)
        };
        let skip = button(
            "skip",
            t("onboarding-skip"),
            Kind::Ghost,
            poster,
            cx.listener(|page, _, window, cx| page.next(window, cx)),
        );
        vec![
            headline(t("onboarding-rewrite-title"), HeadlineSize::Md).into_any_element(),
            lead(body).into_any_element(),
            box_.into_any_element(),
            row.into_any_element(),
            status_line(status).into_any_element(),
            actions()
                .child(button(
                    "next",
                    t("onboarding-next"),
                    Kind::Primary,
                    poster,
                    cx.listener(|page, _, window, cx| page.next(window, cx)),
                ))
                .child(if self.rewrite.done.is_some() {
                    poster::hidden(skip)
                } else {
                    div().child(skip)
                })
                .into_any_element(),
        ]
    }
}
