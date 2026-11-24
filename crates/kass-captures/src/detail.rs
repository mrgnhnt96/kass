//! The open capture: its header (app, time, recording, actions), the
//! transcript you can correct in place, what changed and what Kass heard.
//! A command shows its rewrite, instruction and original instead.

use crate::format::{CaptureTag, capture_tag, format_detail_stamp};
use crate::host::Route;
use crate::page::{CapturesPage, Detail, Load, OpenPhrase};
use crate::phrases::{
    FiredPhrase, dictionary_word, fired_phrases, mark_phrases, phrase_from_hunk, phrase_key, pick,
};
use crate::widgets::{
    app_tile, disclosure, hunk_action, hunk_list, icon, icon_button, kbd, mono_label, symbol_tile,
};
use crate::word_diff::{MergedKind, count_words, diff_words, summarize_changes};
use chrono::Local;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureResponse, CaptureSource, RefinementReview, ReviewOutcome, ReviewReason,
};
use kass_i18n::tr;
use kass_ui::keys::{key_label, key_side, sort_chord};
use kass_ui::{Button, KassTheme, metrics, theme};

/// A voice edit: a command that rewrote text by voice.
pub(crate) fn is_voice_edit(capture: &CaptureResponse) -> bool {
    capture.command_transform.as_deref() == Some("Voice edit")
}

/// The transcript's size: big for a few words, smaller as it grows.
fn text_size(words: usize) -> (Pixels, Pixels) {
    if words <= 6 {
        (px(26.), px(32.5))
    } else if words <= 25 {
        (px(20.), px(31.))
    } else {
        (px(16.), px(25.6))
    }
}

fn source_label(source: CaptureSource) -> String {
    tr!(match source {
        CaptureSource::Recording => "captures-source-recording",
        CaptureSource::File => "captures-source-file",
        CaptureSource::Command => "captures-source-command",
        _ => "captures-source-dictation",
    })
}

fn pill(text: String, fg: Hsla, bg: Hsla, t: &KassTheme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .h(px(18.))
        .px(px(6.))
        .rounded_full()
        .bg(bg)
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_2XS)
        .text_color(fg)
        .whitespace_nowrap()
        .child(text)
}

impl CapturesPage {
    pub(crate) fn render_detail_column(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = match &self.detail {
            None => self.render_empty(cx).into_any_element(),
            Some(detail) if detail.capture.source == CaptureSource::Command => {
                self.render_command(cx).into_any_element()
            }
            Some(_) => self.render_dictation(window, cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(div().flex_1().min_h_0().flex().child(body))
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let mut header = div()
            .h(px(64.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .pl(px(24.))
            .pr(px(12.))
            .border_b_1()
            .border_color(t.border);
        let Some(detail) = &self.detail else {
            return header.child(div().flex_1());
        };
        let capture = &detail.capture;
        let name = capture
            .app_name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| source_label(capture.source));
        let tile = match &capture.app_bundle_id {
            Some(bundle) => app_tile(self.host.as_ref(), bundle, &name, px(24.), cx),
            None => symbol_tile(IconName::Captions, px(24.), cx).into_any_element(),
        };
        let stamp = format_detail_stamp(
            &capture.created_at,
            &tr!("captures-detail-today"),
            &tr!("captures-detail-yesterday"),
            Local::now(),
        );
        header = header.child(tile).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(metrics::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(t.muted_foreground)
                        .child(stamp),
                ),
        );
        if let Some(player) = &detail.player {
            header = header.child(player.clone());
        } else if capture.audio_deleted {
            header = header.child(
                div()
                    .flex_none()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(tr!("captures-detail-audio-deleted")),
            );
        }

        let command = capture.source == CaptureSource::Command;
        let style = capture
            .app_bundle_id
            .as_ref()
            .and_then(|b| {
                self.apps
                    .as_ref()?
                    .apps
                    .iter()
                    .find(|a| &a.app_bundle_id == b)
            })
            .and_then(|a| a.style_id.clone());
        let settings_label = tr!(if command {
            "captures-actions-configure-command"
        } else {
            "captures-actions-configure"
        });
        header = header.child(
            icon_button("detail-settings", IconName::Settings2, cx)
                .tooltip(move |window, cx| Tooltip::new(settings_label.clone()).build(window, cx))
                .on_click(cx.listener(move |page, _, window, cx| {
                    let route = if command {
                        Route::CommandMode
                    } else {
                        Route::WritingStyle(style.clone())
                    };
                    page.navigate(route, window, cx)
                })),
        );
        let deleting = self.deleting;
        header = header.child(
            icon_button("detail-delete", IconName::Trash, cx)
                .when(deleting, |b| b.opacity(0.5))
                .tooltip(|window, cx| {
                    Tooltip::new(tr!("captures-actions-delete"))
                        .action(&crate::page::DeleteCapture, Some(crate::page::CONTEXT))
                        .build(window, cx)
                })
                .on_click(cx.listener(|page, _, window, cx| page.ask_delete(window, cx))),
        );
        let open = self.details_open;
        let label = tr!(if open {
            "captures-details-hide"
        } else {
            "captures-details-show"
        });
        header.child(
            icon_button(
                "detail-toggle",
                if open {
                    IconName::PanelRightClose
                } else {
                    IconName::PanelRightOpen
                },
                cx,
            )
            .when(open, |b| b.bg(t.muted))
            .tooltip(move |window, cx| {
                Tooltip::new(label.clone())
                    .action(&crate::page::ToggleDetails, Some(crate::page::CONTEXT))
                    .build(window, cx)
            })
            .on_click(cx.listener(|page, _, _, cx| {
                page.details_open = !page.details_open;
                cx.notify();
            })),
        )
    }

    /// No capture open: loading, pick one, or how to make the first.
    fn render_empty(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let centered = |inner: Div| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .text_color(t.muted_foreground)
                .child(
                    inner
                        .max_w(px(384.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(16.))
                        .text_center(),
                )
        };
        let note = |text: String| div().text_size(metrics::TEXT_BASE).child(text);
        let hint = |text: String| {
            div()
                .text_size(metrics::TEXT_XS)
                .line_height(px(19.5))
                .child(text)
        };
        let loading = self.captures_load == Load::Loading;
        if loading || !self.captures.is_empty() {
            return centered(
                div()
                    .child(icon(
                        IconName::Captions,
                        px(32.),
                        t.muted_foreground.opacity(0.4),
                    ))
                    .child(note(tr!(if loading {
                        "captures-empty-loading"
                    } else {
                        "captures-empty-pick-one"
                    }))),
            );
        }
        let settings = self.capture_settings.as_ref();
        let enabled = settings.is_some_and(|s| s.hotkey_enabled);
        if enabled && self.not_ready {
            return centered(
                div()
                    .child(note(tr!("captures-not-ready-title")).text_color(t.foreground))
                    .child(hint(tr!("captures-not-ready-description")))
                    .child(
                        Button::new("empty-setup")
                            .small()
                            .label(tr!("captures-not-ready-action"))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.navigate(Route::Setup, window, cx)
                            })),
                    ),
            );
        }
        let (push, toggle) = settings.map_or((Vec::new(), Vec::new()), |s| {
            (
                s.chord_push_to_talk_keys.clone(),
                s.chord_toggle_to_talk_keys.clone(),
            )
        });
        if enabled && (!push.is_empty() || !toggle.is_empty()) {
            let chord_row = |keys: &[String], label: String| {
                let mut caps = div().flex().items_center().gap(px(4.));
                for key in sort_chord(keys) {
                    let mut cap = kbd(key_label(&key), cx)
                        .h(px(24.))
                        .min_w(px(24.))
                        .text_color(t.foreground);
                    if let Some(side) = key_side(&key) {
                        cap = cap.child(
                            div()
                                .ml(px(2.))
                                .text_size(px(9.))
                                .text_color(t.accent)
                                .child(side),
                        );
                    }
                    caps = caps.child(cap);
                }
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(caps)
                    .child(mono_label(label, t.muted_foreground, cx))
            };
            let mut chords = div().flex().flex_col().gap(px(10.));
            if !push.is_empty() {
                chords = chords.child(chord_row(&push, tr!("captures-empty-hold-to-record")));
            }
            if !toggle.is_empty() {
                chords = chords.child(chord_row(&toggle, tr!("captures-empty-toggle-hands-free")));
            }
            return centered(
                div()
                    .child(chords)
                    .child(note(tr!("captures-empty-press-shortcut"))),
            );
        }
        centered(
            div()
                .child(icon(
                    IconName::Captions,
                    px(32.),
                    t.muted_foreground.opacity(0.4),
                ))
                .child(note(tr!("captures-empty-none")).text_color(t.foreground))
                .child(hint(tr!("captures-empty-turn-on-shortcut")))
                .child(
                    Button::new("empty-settings")
                        .small()
                        .outline()
                        .label(tr!("captures-empty-open-settings"))
                        .on_click(cx.listener(|page, _, window, cx| {
                            page.navigate(Route::DictationSettings, window, cx)
                        })),
                ),
        )
    }

    fn render_dictation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let detail = self.detail.as_ref().expect("a capture is open");
        let capture = detail.capture.clone();
        let raw = capture.transcript_raw.clone();
        let refined = capture.transcript_refined.clone().filter(|r| !r.is_empty());
        let delivered = refined.clone().unwrap_or_else(|| raw.clone());
        let phrases = fired_phrases(&raw, &delivered, &self.dictionary);

        let mut main = div()
            .id("capture-main")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(24.))
            .pt(px(24.))
            .pb(px(12.))
            .overflow_y_scroll();
        if let Some(review) = &capture.refinement_review {
            main = main.child(review_notice(review, theme(cx)));
        }
        main = main.child(self.render_transcript(refined.is_some(), &phrases, window, cx));
        if let Some(refined) = &refined {
            main = main.child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(4.))
                    .child(self.render_changes(&raw, refined, &phrases, cx))
                    .child(self.render_heard(&raw, cx)),
            );
        }
        div()
            .size_full()
            .flex()
            .child(main)
            .when(self.details_open, |d| d.child(self.render_inspector(cx)))
    }

    fn render_transcript(
        &self,
        refined: bool,
        phrases: &[FiredPhrase],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let detail = self.detail.as_ref().expect("a capture is open");
        let teach = &detail.teach;
        let editing = teach.draft.is_some();
        let saved = teach.saved(&detail.reports);
        let shown = saved
            .map(|s| s.expected_text.clone())
            .unwrap_or_else(|| teach.original.clone());
        let (size, line) = text_size(count_words(&shown));

        let mut top = div().flex().items_center().gap(px(8.)).child(mono_label(
            tr!(if refined {
                "captures-transcript-refined"
            } else {
                "captures-transcript-raw"
            }),
            t.accent,
            cx,
        ));
        if saved.is_some() {
            top = top.child(pill(
                tr!("captures-teach-corrected-by-you"),
                t.success,
                t.success.opacity(0.15),
                &t,
            ));
        }
        top = top.child(div().flex_1());
        if !editing {
            if saved.is_none() {
                let help = detail.help_open;
                top = top.child(
                    icon_button("transcript-help", IconName::CircleQuestionMark, cx)
                        .size(px(28.))
                        .when(help, |b| b.text_color(t.foreground))
                        .tooltip(|window, cx| {
                            Tooltip::new(tr!("captures-feedback-about")).build(window, cx)
                        })
                        .on_click(cx.listener(|page, _, _, cx| {
                            if let Some(detail) = &mut page.detail {
                                detail.help_open = !detail.help_open;
                                cx.notify();
                            }
                        })),
                );
            }
            top = top.child(
                div()
                    .id("transcript-alter")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(t.accent.opacity(0.45))
                    .bg(t.accent.opacity(0.1))
                    .hover(|s| s.bg(t.accent.opacity(0.15)))
                    .cursor_pointer()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.accent)
                    .child(icon(IconName::Pencil, px(12.), t.accent))
                    .child(tr!("captures-teach-alter"))
                    .on_click(cx.listener(|page, _, window, cx| page.begin_edit(window, cx))),
            );
        }
        let copied = detail.copied;
        let copy_text = shown.clone();
        top = top.child(
            icon_button(
                "transcript-copy",
                if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                },
                cx,
            )
            .size(px(28.))
            .tooltip(|window, cx| Tooltip::new(tr!("captures-panel-copy")).build(window, cx))
            .on_click(
                cx.listener(move |page, _, window, cx| page.copy(copy_text.clone(), window, cx)),
            ),
        );

        let mut card = div()
            .id("transcript-card")
            .flex_1()
            .min_h(px(200.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .rounded(metrics::RADIUS_XL)
            .border_1()
            .border_color(t.accent.opacity(0.3))
            .bg(t.accent.opacity(0.045))
            .px(px(20.))
            .pt(px(16.))
            .pb(px(24.))
            .child(top);
        if detail.help_open && !editing && saved.is_none() {
            card = card.child(
                div()
                    .mt(px(-4.))
                    .text_size(metrics::TEXT_XS)
                    .line_height(px(19.5))
                    .text_color(t.muted_foreground)
                    .child(tr!("captures-feedback-description")),
            );
        }

        // The text sits in the same box whether it is shown or being edited,
        // down to the 1px border, so tapping in never moves a word.
        let surface = |d: Div| {
            d.mx(px(-6.))
                .my(px(-4.))
                .px(px(6.))
                .py(px(4.))
                .rounded(metrics::RADIUS_MD)
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .text_size(size)
                .line_height(line)
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
        };
        if editing {
            card = card.child(
                surface(div())
                    .bg(t.foreground.opacity(0.04))
                    .border_color(t.ring)
                    .child(
                        // No frame of its own: the surface is the frame, and
                        // the type matches the shown text exactly.
                        Textarea::new(&detail.editor)
                            .appearance(false)
                            .text_size(size)
                            .line_height(line)
                            .font_weight(FontWeight::MEDIUM),
                    ),
            );
            if let Some(add) = self.render_pick(cx) {
                card = card.child(add);
            }
            card = card.child(self.render_teach_actions(window, cx));
        } else {
            let mut hit: Option<TextLayout> = None;
            // Where each phrase wrote into the text, to open it on a click.
            let mut written: Vec<(std::ops::Range<usize>, FiredPhrase)> = Vec::new();
            let text = if let Some(saved) = saved {
                let after = diff_words(&teach.original, &saved.expected_text).after;
                let mut joined = String::new();
                let mut marks = Vec::new();
                for segment in after {
                    let start = joined.len();
                    joined.push_str(&segment.text);
                    if segment.changed {
                        marks.push((
                            start..joined.len(),
                            HighlightStyle {
                                background_color: Some(t.success.opacity(0.15)),
                                ..Default::default()
                            },
                        ));
                    }
                }
                let text = StyledText::new(joined).with_highlights(marks);
                hit = Some(text.layout().clone());
                text.into_any_element()
            } else if shown.is_empty() {
                div()
                    .text_size(px(16.))
                    .text_color(t.muted_foreground)
                    .child(tr!("captures-snippet-empty"))
                    .into_any_element()
            } else if !phrases.is_empty() {
                let mut joined = String::new();
                let mut marks = Vec::new();
                for run in mark_phrases(&shown, phrases) {
                    let start = joined.len();
                    joined.push_str(&run.text);
                    if let Some(phrase) = run.phrase {
                        let hovered = detail.phrase_hover == Some(written.len());
                        marks.push((
                            start..joined.len(),
                            HighlightStyle {
                                underline: Some(UnderlineStyle {
                                    thickness: px(2.),
                                    color: Some(t.accent),
                                    wavy: false,
                                }),
                                background_color: hovered.then(|| t.accent.opacity(0.1)),
                                ..Default::default()
                            },
                        ));
                        written.push((start..joined.len(), phrase));
                    }
                }
                let text = StyledText::new(joined).with_highlights(marks);
                hit = Some(text.layout().clone());
                text.into_any_element()
            } else {
                let text = StyledText::new(shown.clone());
                hit = Some(text.layout().clone());
                text.into_any_element()
            };
            let written = std::rc::Rc::new(written);
            let hovered = detail
                .phrase_hover
                .and_then(|i| written.get(i))
                .map(|(_, phrase)| phrase.spoken.clone());
            let on_phrase = {
                let (hit, written) = (hit.clone(), written.clone());
                move |position: Point<Pixels>| {
                    let at = hit.as_ref()?.index_for_position(position).ok()?;
                    written.iter().position(|(range, _)| range.contains(&at))
                }
            };
            let over_phrase = on_phrase.clone();
            card = card.child(
                surface(div())
                    .id("transcript-text")
                    .cursor(if hovered.is_some() {
                        CursorStyle::PointingHand
                    } else {
                        CursorStyle::IBeam
                    })
                    .hover(|s| s.bg(t.foreground.opacity(0.04)))
                    .tooltip(move |window, cx| {
                        let tip = match &hovered {
                            Some(spoken) => tr!("captures-phrase-wrote-hint", spoken = spoken),
                            None => tr!("captures-teach-edit-hint"),
                        };
                        Tooltip::new(tip).build(window, cx)
                    })
                    .on_mouse_move(cx.listener(move |page, event: &MouseMoveEvent, _, cx| {
                        let over = over_phrase(event.position);
                        if let Some(detail) = &mut page.detail
                            && detail.phrase_hover != over
                        {
                            detail.phrase_hover = over;
                            cx.notify();
                        }
                    }))
                    .on_hover(cx.listener(|page, hovered: &bool, _, cx| {
                        if let Some(detail) = &mut page.detail
                            && !hovered
                            && detail.phrase_hover.take().is_some()
                        {
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |page, event: &ClickEvent, window, cx| {
                        // Text a phrase wrote opens the phrase; anywhere
                        // else starts editing with the cursor there.
                        if let Some(i) = on_phrase(event.position()) {
                            page.open_phrase(written[i].1.clone(), event.position(), cx);
                            return;
                        }
                        let at = hit.as_ref().map(|layout| {
                            match layout.index_for_position(event.position()) {
                                Ok(i) | Err(i) => i,
                            }
                        });
                        page.begin_edit_at(at, window, cx);
                    }))
                    .child(text),
            );
            if let Some(open) = &detail.phrase_open {
                card = card.child(self.render_phrase_popover(open, cx));
            }
            if let Some(notice) = self.render_learned(cx) {
                card = card.child(notice);
            }
        }
        card
    }

    /// What wrote the clicked text: the phrase said and the text it writes,
    /// and a way to edit it in the dictionary.
    fn render_phrase_popover(&self, open: &OpenPhrase, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let phrase = &open.phrase;
        let card = div()
            .id("phrase-popover")
            .w(px(280.))
            .flex()
            .flex_col()
            .p_1()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .text_color(t.popover_foreground)
            .shadow_md()
            .on_mouse_down_out(cx.listener(|page, _, _, cx| {
                page.close_phrase(cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .child(pill(
                        tr!("captures-phrase-chip"),
                        t.accent,
                        t.accent.opacity(0.1),
                        &t,
                    ))
                    .child(
                        div()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(tr!("captures-phrase-from")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .pb_2()
                    .text_size(px(13.))
                    .child(
                        div()
                            .flex_none()
                            .child(format!("\u{201c}{}\u{201d}", phrase.spoken)),
                    )
                    .child(div().mt(px(2.)).child(icon(
                        IconName::ArrowRight,
                        px(14.),
                        t.muted_foreground,
                    )))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_color(t.muted_foreground)
                            .child(phrase.written.clone()),
                    ),
            )
            .child(div().h(px(1.)).mx(px(-4.)).my_1().bg(t.border))
            .child(
                div()
                    .id("phrase-edit")
                    .flex()
                    .items_center()
                    .h(px(30.))
                    .px_2()
                    .rounded(metrics::RADIUS_SM)
                    .text_size(metrics::TEXT_BASE)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.secondary))
                    .child(tr!("captures-phrase-edit"))
                    .on_click(cx.listener(|page, _, window, cx| {
                        page.close_phrase(cx);
                        page.host.navigate(Route::DictionaryPhrases, window, cx);
                    })),
            );
        deferred(
            anchored()
                .anchor(Anchor::TopLeft)
                .position(open.at + point(px(0.), px(12.)))
                .snap_to_window_with_margin(px(8.))
                .child(card),
        )
        .with_priority(1)
        .into_any_element()
    }

    /// "Add to dictionary" over a word or phrase selected in the edit box:
    /// above the selection, or below it near the top of the window.
    fn render_pick(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let detail = self.detail.as_ref()?;
        if detail.teach.saving || self.adding_entry {
            return None;
        }
        let state = detail.editor.read(cx);
        let range = state.selected_range();
        let picked = pick(&state.value(), range.start, range.end)?;
        let bounds = state.range_to_bounds(&range)?;
        let t = theme(cx).clone();
        let x = bounds.origin.x + bounds.size.width.max(px(0.)) / 2.;
        let (anchor, y) = if bounds.origin.y > px(44.) {
            (Anchor::BottomCenter, bounds.origin.y - px(8.))
        } else {
            (Anchor::TopCenter, bounds.bottom() + px(8.))
        };
        let button = div()
            .id("pick-add")
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(10.))
            .rounded(metrics::RADIUS_MD)
            .bg(t.primary)
            .text_color(t.primary_foreground)
            .text_size(metrics::TEXT_XS)
            .font_weight(FontWeight::MEDIUM)
            .shadow_md()
            .cursor_pointer()
            .child(icon(IconName::BookPlus, px(14.), t.primary_foreground))
            .child(tr!("captures-dictionary-add"))
            // On press, so the edit box keeps its selection and focus until
            // the dialog takes it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |page, _, window, cx| {
                    cx.stop_propagation();
                    window.prevent_default();
                    page.pick_word(picked.clone(), window, cx);
                }),
            );
        Some(
            deferred(
                anchored()
                    .anchor(anchor)
                    .position(point(x, y))
                    .snap_to_window_with_margin(px(8.))
                    .child(button),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Under the text while editing: what changed, with one-click dictionary
    /// and phrase actions, an optional note, and Cancel / Save.
    fn render_teach_actions(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx).clone();
        let detail = self.detail.as_ref().expect("a capture is open");
        let teach = &detail.teach;
        let draft = teach.draft.clone().unwrap_or_default();
        if draft.is_empty() {
            return div()
                .text_size(metrics::TEXT_XS)
                .text_color(t.muted_foreground)
                .child(tr!("captures-feedback-empty-hint"))
                .into_any_element();
        }
        if !teach.changed(&detail.reports) {
            return div().into_any_element();
        }
        let hunks = diff_words(&teach.base(&detail.reports), &draft).hunks;
        let busy = teach.saving || self.adding_entry;
        let mut actions = div().flex().flex_col().gap(px(14.));
        if !hunks.is_empty() {
            let handle = cx.entity().downgrade();
            actions = actions.child(hunk_list(&hunks, cx, |i, hunk| {
                let mut extras = div().flex().items_center();
                let mut any = false;
                if let Some(word) = dictionary_word(hunk) {
                    any = true;
                    let handle = handle.clone();
                    extras = extras.child(
                        hunk_action(
                            ("hunk-add", i),
                            IconName::BookPlus,
                            tr!("captures-dictionary-add"),
                            cx,
                        )
                        .on_click(move |_, window, cx| {
                            handle
                                .update(cx, |page, cx| page.add_word(word.clone(), window, cx))
                                .ok();
                        }),
                    );
                }
                if let Some(phrase) = phrase_from_hunk(hunk) {
                    any = true;
                    let handle = handle.clone();
                    extras = extras.child(
                        hunk_action(
                            ("hunk-phrase", i),
                            IconName::MessageSquareQuote,
                            tr!("captures-phrase-make"),
                            cx,
                        )
                        .on_click(move |_, window, cx| {
                            handle
                                .update(cx, |page, cx| page.make_phrase(phrase.clone(), window, cx))
                                .ok();
                        }),
                    );
                }
                any.then(|| extras.into_any_element())
            }));
        }
        actions = actions.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(metrics::TEXT_XS)
                        .text_color(t.muted_foreground)
                        .child(tr!("captures-feedback-notes")),
                )
                .child(
                    Input::new(&detail.notes)
                        .h(px(36.))
                        .px_3()
                        .rounded(metrics::RADIUS_MD)
                        .border_1()
                        .border_color(t.input)
                        .bg(t.background)
                        .text_size(metrics::TEXT_BASE),
                ),
        );
        actions
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("teach-cancel")
                            .outline()
                            .small()
                            .disabled(teach.saving)
                            .label(format!("{}  esc", tr!("common-cancel")))
                            .on_click(
                                cx.listener(|page, _, window, cx| page.cancel_edit(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("teach-save")
                            .small()
                            .loading(busy)
                            .disabled(busy)
                            .label(format!("{}  ⏎", tr!("captures-teach-save")))
                            .on_click(
                                cx.listener(|page, _, window, cx| page.save_edit(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// After a round is saved: what it changed and how learning is going.
    fn render_learned(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx).clone();
        let detail = self.detail.as_ref()?;
        let report = detail.teach.learned_round(&detail.reports)?;
        let before = detail.teach.before(&detail.reports, &report.id);
        let hunks = diff_words(&before, &report.expected_text).hunks;
        let model = self
            .learning
            .as_ref()
            .and_then(|l| l.model.as_ref())
            .filter(|m| m.running);
        let mut saved = tr!("captures-feedback-saved");
        if let Some(model) = model {
            saved.push(' ');
            saved.push_str(&learning_phase(&model.phase));
        }
        let mut head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .text_size(metrics::TEXT_BASE)
            .child(icon(IconName::Check, px(14.), t.success))
            .child(tr!("captures-teach-learned"));
        if !hunks.is_empty() {
            head = head.child(hunk_list(&hunks, cx, |_, _| None).text_color(t.muted_foreground));
        }
        let mut card = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .p(px(12.))
            .child(head)
            .child(
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(saved),
            );
        if model.is_some() {
            card = card.child(
                div()
                    .h(px(3.))
                    .rounded(px(2.))
                    .overflow_hidden()
                    .bg(t.border)
                    .child(
                        div()
                            .h_full()
                            .w(relative(1. / 3.))
                            .rounded(px(2.))
                            .bg(t.accent)
                            .with_animation(
                                "learning-pulse",
                                Animation::new(std::time::Duration::from_millis(2000)).repeat(),
                                |bar, delta| {
                                    bar.opacity(
                                        0.5 + 0.5 * (delta * std::f32::consts::TAU).cos().abs(),
                                    )
                                },
                            ),
                    ),
            );
        }
        let mut notice = div().flex().flex_col().gap(px(14.)).child(card);
        if self.examples > 0 {
            notice = notice.child(
                div()
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_CAPTION)
                    .text_color(t.muted_foreground)
                    .child(tr!(
                        "captures-teach-examples-learned",
                        count = self.examples
                    )),
            );
        }
        Some(notice.into_any_element())
    }

    fn render_changes(
        &self,
        raw: &str,
        refined: &str,
        phrases: &[FiredPhrase],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let diff = diff_words(raw, refined);
        let mut summary = summarize_changes(&diff);
        let written: Vec<String> = phrases.iter().map(|p| phrase_key(&p.written)).collect();
        let mut phrase_count = 0usize;
        for hunk in &diff.hunks {
            if hunk.added.is_empty() || !written.contains(&phrase_key(&hunk.added)) {
                continue;
            }
            let times = hunk.count.unwrap_or(1);
            phrase_count += times;
            if !hunk.removed.is_empty() {
                summary.reworded = summary.reworded.saturating_sub(times);
            } else {
                summary.added = summary
                    .added
                    .saturating_sub(count_words(&hunk.added) * times);
            }
        }
        let chips: Vec<String> = [
            ("captures-changes-phrase", phrase_count),
            ("captures-changes-removed", summary.removed),
            ("captures-changes-added", summary.added),
            ("captures-changes-reworded", summary.reworded),
            ("captures-changes-case", summary.case),
            ("captures-changes-punctuation", summary.punctuation),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(id, count)| tr!(id, count = count))
        .collect();
        if chips.is_empty() {
            return div()
                .h(px(28.))
                .my(px(10.))
                .pl(px(22.))
                .flex()
                .items_center()
                .text_size(metrics::TEXT_BASE)
                .text_color(t.muted_foreground)
                .child(tr!("captures-changes-none"))
                .into_any_element();
        }
        let open = self.detail.as_ref().is_some_and(|d| d.changes_open);
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .py(px(10.))
            .child(
                disclosure("changes", tr!("captures-changes-title"), open, cx).on_click(
                    cx.listener(|page, _, _, cx| {
                        if let Some(detail) = &mut page.detail {
                            detail.changes_open = !detail.changes_open;
                            cx.notify();
                        }
                    }),
                ),
            )
            .child(
                div().flex().flex_wrap().gap(px(4.)).pl(px(22.)).children(
                    chips
                        .into_iter()
                        .map(|c| pill(c, t.accent, t.accent.opacity(0.1), &t)),
                ),
            );
        if open {
            let mut joined = String::new();
            let mut marks = Vec::new();
            for segment in &diff.merged {
                let start = joined.len();
                joined.push_str(&segment.text);
                let style = match segment.kind {
                    MergedKind::Same => continue,
                    MergedKind::Removed => HighlightStyle {
                        color: Some(t.destructive),
                        background_color: Some(t.destructive.opacity(0.15)),
                        strikethrough: Some(StrikethroughStyle {
                            thickness: px(1.),
                            color: Some(t.destructive),
                        }),
                        ..Default::default()
                    },
                    MergedKind::Added => HighlightStyle {
                        color: Some(t.foreground),
                        background_color: Some(t.accent.opacity(0.2)),
                        ..Default::default()
                    },
                };
                marks.push((start..joined.len(), style));
            }
            section = section.child(
                div()
                    .ml(px(22.))
                    .mt(px(4.))
                    .rounded(metrics::RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .px(px(14.))
                    .py(px(12.))
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_BASE)
                    .line_height(px(23.4))
                    .text_color(t.foreground.opacity(0.8))
                    .child(StyledText::new(joined).with_highlights(marks)),
            );
        }
        section.into_any_element()
    }

    fn render_heard(&self, raw: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let open = self.detail.as_ref().is_some_and(|d| d.heard_open);
        let copy_text = raw.to_string();
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_t_1()
            .border_color(t.border)
            .py(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        disclosure("heard", tr!("captures-transcript-heard"), open, cx)
                            .flex_1()
                            .on_click(cx.listener(|page, _, _, cx| {
                                if let Some(detail) = &mut page.detail {
                                    detail.heard_open = !detail.heard_open;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        icon_button("heard-copy", IconName::Copy, cx)
                            .size(px(28.))
                            .tooltip(|window, cx| {
                                Tooltip::new(tr!("captures-panel-copy-raw")).build(window, cx)
                            })
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.copy(copy_text.clone(), window, cx)
                            })),
                    ),
            );
        if open {
            section = section.child(
                div()
                    .ml(px(22.))
                    .mt(px(4.))
                    .rounded(metrics::RADIUS_LG)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .px(px(14.))
                    .py(px(12.))
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_BASE)
                    .line_height(px(23.4))
                    .text_color(t.foreground.opacity(0.8))
                    .child(raw.to_string()),
            );
        }
        section
    }

    /// A command: its rewrite, the instruction and the original selection.
    fn render_command(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let detail: &Detail = self.detail.as_ref().expect("a capture is open");
        let capture = detail.capture.clone();
        let result = capture.transcript_refined.clone().filter(|r| !r.is_empty());
        let original = capture.command_selection.clone().unwrap_or_default();
        let said = capture.transcript_raw.trim().to_string();
        let instruction = capture
            .command_instruction
            .clone()
            .unwrap_or_else(|| said.clone());
        let section = |border: Hsla| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .rounded(metrics::RADIUS_XL)
                .border_1()
                .border_color(border)
                .px(px(20.))
                .py(px(16.))
        };
        let copy = |id: &'static str, text: String, label: String, cx: &mut Context<Self>| {
            icon_button(id, IconName::Copy, cx)
                .size(px(28.))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(
                    cx.listener(move |page, _, window, cx| page.copy(text.clone(), window, cx)),
                )
        };

        let mut rewrite_head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(mono_label(tr!("captures-command-result"), t.accent, cx))
            .child(div().flex_1());
        if let Some(result) = &result {
            rewrite_head = rewrite_head.child(copy(
                "command-copy-result",
                result.clone(),
                tr!("captures-command-copy-result"),
                cx,
            ));
        }
        let rewrite = section(t.accent.opacity(0.3))
            .gap(px(12.))
            .bg(t.accent.opacity(0.045))
            .pb(px(24.))
            .child(rewrite_head)
            .child(match &result {
                Some(result) => div()
                    .text_size(px(16.))
                    .line_height(px(25.6))
                    .font_weight(FontWeight::MEDIUM)
                    .child(result.clone()),
                None => div()
                    .text_size(metrics::TEXT_SM)
                    .text_color(t.muted_foreground)
                    .child(tr!("captures-command-failed")),
            });

        let heading = match &capture.command_transform {
            Some(transform) => format!("{} · {transform}", tr!("captures-command-transform")),
            None => tr!("captures-command-instruction"),
        };
        let mut instruction_head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(mono_label(heading, t.muted_foreground, cx))
            .child(div().flex_1());
        if is_voice_edit(&capture) {
            instruction_head = instruction_head.child(
                icon_button("command-delete", IconName::Trash, cx)
                    .size(px(28.))
                    .tooltip(|window, cx| {
                        Tooltip::new(tr!("captures-actions-delete")).build(window, cx)
                    })
                    .on_click(cx.listener(|page, _, window, cx| page.ask_delete(window, cx))),
            );
        }
        let mut instruction_section = section(t.border).gap(px(6.)).child(instruction_head).child(
            div()
                .text_size(metrics::TEXT_SM)
                .child(if instruction.is_empty() {
                    "∅".to_string()
                } else {
                    instruction
                }),
        );
        if capture.command_transform.is_some() && !said.is_empty() {
            instruction_section = instruction_section.child(
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .child(format!("{}: “{said}”", tr!("captures-command-said"))),
            );
        }

        let mut original_head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(mono_label(
                tr!("captures-command-original"),
                t.muted_foreground,
                cx,
            ))
            .child(div().flex_1());
        if !original.is_empty() {
            original_head = original_head.child(copy(
                "command-copy-original",
                original.clone(),
                tr!("captures-command-copy-original"),
                cx,
            ));
        }
        let original_section = section(t.border)
            .child(original_head)
            .child(
                div()
                    .text_size(metrics::TEXT_SM)
                    .text_color(t.muted_foreground)
                    .child(if original.is_empty() {
                        "∅".to_string()
                    } else {
                        original
                    }),
            )
            .child(
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground.opacity(0.8))
                    .child(tr!("captures-command-undo-hint")),
            );

        let main = div()
            .id("command-main")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(24.))
            .pt(px(24.))
            .pb(px(12.))
            .overflow_y_scroll()
            .child(rewrite)
            .child(instruction_section)
            .child(original_section);
        let _ = capture_tag(&capture) == CaptureTag::Command;
        div()
            .size_full()
            .flex()
            .child(main)
            .when(self.details_open, |d| {
                d.child(self.render_command_inspector(&capture, cx))
            })
    }
}

/// The phrase a model learning phase reads as.
pub(crate) fn learning_phase(phase: &str) -> String {
    let id = format!(
        "captures-feedback-learning-model-phases-{}",
        phase.replace('_', "-")
    );
    kass_i18n::t(&id)
}

/// The warning on a cleanup Kass wasn't sure of, or one it threw away.
fn review_notice(review: &RefinementReview, t: &KassTheme) -> impl IntoElement + use<> {
    let rejected = review.outcome == ReviewOutcome::Reject;
    let muted = |text: String| div().text_color(t.muted_foreground).child(text);
    let mut body =
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(tr!(if rejected {
                        "captures-review-rejected-title"
                    } else {
                        "captures-review-title"
                    })),
            );
    if rejected && !review.reasons.is_empty() {
        let reasons: Vec<String> = review
            .reasons
            .iter()
            .filter_map(|reason| {
                let id = match reason {
                    ReviewReason::Answered => "captures-review-reasons-answered",
                    ReviewReason::Dropped => "captures-review-reasons-dropped",
                    ReviewReason::Negation => "captures-review-reasons-negation",
                    ReviewReason::Number => "captures-review-reasons-number",
                    ReviewReason::Person => "captures-review-reasons-person",
                    ReviewReason::Technical => "captures-review-reasons-technical",
                    _ => return None,
                };
                Some(kass_i18n::t(id))
            })
            .collect();
        body = body.child(muted(reasons.join(" ")));
    }
    if !review.added.is_empty() {
        body = body.child(muted(tr!(
            "captures-review-added",
            words = review.added.join(", ")
        )));
    }
    if !review.missing.is_empty() {
        body = body.child(muted(tr!(
            "captures-review-missing",
            words = review.missing.join(", ")
        )));
    }
    body = body.child(
        div()
            .text_size(metrics::TEXT_XS)
            .text_color(t.muted_foreground)
            .child(tr!("captures-review-hint")),
    );
    div()
        .flex_none()
        .flex()
        .gap(px(10.))
        .rounded(metrics::RADIUS_LG)
        .border_1()
        .border_color(t.warning.opacity(0.3))
        .bg(t.warning.opacity(0.1))
        .p(px(12.))
        .text_size(metrics::TEXT_BASE)
        .child(
            div()
                .mt(px(2.))
                .child(icon(IconName::TriangleAlert, px(16.), t.warning)),
        )
        .child(body)
}
