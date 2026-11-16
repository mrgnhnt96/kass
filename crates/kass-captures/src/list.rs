//! The capture list: the last 7 days' card, the filtered app's card,
//! search, the not-set-up banner and one row per capture.

use crate::format::{
    CaptureTag, capture_tag, delivered_text, format_count, format_day, format_duration,
    format_row_time, format_saved, snippet_parts,
};
use crate::host::Route;
use crate::page::{CapturesPage, Load, SEARCH_CONTEXT};
use crate::widgets::{app_tile, dot, icon, kbd, mono_label};
use chrono::{Local, NaiveDate};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Input;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{CaptureResponse, UsagePoint};
use kass_i18n::tr;
use kass_ui::{KassTheme, metrics, select, theme};

impl CapturesPage {
    /// The filtered app's name, for labels.
    pub(crate) fn filter_app_name(&self) -> Option<String> {
        let app = self.filter_app()?;
        Some(
            app.app_name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| app.app_bundle_id.clone()),
        )
    }

    pub(crate) fn search_placeholder(&self) -> String {
        match self.filter_app_name() {
            Some(app) => tr!("captures-apps-search-placeholder", app = app),
            None => tr!("captures-search-placeholder"),
        }
    }

    pub(crate) fn render_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let mut top = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(16.))
            .border_b_1()
            .border_color(t.border)
            .child(self.render_week(cx));
        if let Some(header) = self.render_app_header(cx) {
            top = top.child(header);
        }
        let search_focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        top = top.child(
            div()
                .key_context(SEARCH_CONTEXT)
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(40.))
                .px(px(12.))
                .rounded(metrics::RADIUS_LG)
                .border_1()
                .border_color(if search_focused { t.ring } else { t.input })
                .bg(t.popover)
                .child(icon(IconName::Search, px(15.), t.muted_foreground))
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.search)
                            .text_size(metrics::TEXT_BASE)
                            .bg(t.popover),
                    ),
                )
                .child(kbd("⌘K", cx)),
        );

        let mut body = div()
            .id("capture-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll);
        if self.not_ready {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.border)
                    .bg(t.warning.opacity(0.1))
                    .text_size(metrics::TEXT_BASE)
                    .child(tr!("captures-not-ready-title"))
                    .child(
                        div()
                            .id("not-ready-setup")
                            .flex_none()
                            .cursor_pointer()
                            .text_color(t.accent)
                            .hover(|s| s.underline())
                            .child(tr!("captures-not-ready-action"))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.navigate(Route::Setup, window, cx)
                            })),
                    ),
            );
        }

        let visible: Vec<CaptureResponse> = self.visible().into_iter().cloned().collect();
        if self.captures_load == Load::Loading && self.captures.is_empty() {
            body = body.child(div().py(px(48.)).flex().justify_center().child(icon(
                IconName::LoaderCircle,
                px(16.),
                t.muted_foreground,
            )));
        } else if visible.is_empty() {
            let query = self.query.trim();
            let text = if !query.is_empty() {
                tr!("captures-empty-no-matches", query = query.to_string())
            } else if self.captures_load == Load::Failed {
                tr!("captures-empty-none")
            } else if !self.captures.is_empty() {
                tr!("captures-empty-none-in-filter")
            } else {
                tr!("captures-empty-none")
            };
            body = body.child(
                div()
                    .px(px(16.))
                    .py(px(48.))
                    .text_center()
                    .text_size(metrics::TEXT_BASE)
                    .text_color(t.muted_foreground)
                    .child(text),
            );
        } else {
            let show_app = self.filter_app().is_none();
            let mut rows = div().flex().flex_col().gap(px(2.)).p(px(8.));
            for capture in &visible {
                rows = rows.child(self.render_row(capture, show_app, cx));
            }
            body = body.child(rows);
        }

        div().size_full().flex().flex_col().child(top).child(body)
    }

    /// The style a capture was cleaned up with; "Work?" while its app's
    /// style is unconfirmed.
    fn row_style(&self, capture: &CaptureResponse) -> Option<String> {
        let name = self.style_name(capture.style_id.as_deref()?)?;
        let confirmed = self
            .apps
            .as_ref()
            .and_then(|a| {
                a.apps
                    .iter()
                    .find(|app| Some(&app.app_bundle_id) == capture.app_bundle_id.as_ref())
            })
            .is_none_or(|app| app.confirmed);
        Some(if confirmed { name } else { format!("{name}?") })
    }

    fn render_row(
        &self,
        capture: &CaptureResponse,
        show_app: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let active = self.selected.as_ref() == Some(&capture.id);
        let tag = capture_tag(capture);
        let text = delivered_text(capture).trim().to_string();
        let added = capture
            .refinement_review
            .as_ref()
            .map(|r| r.added.clone())
            .unwrap_or_default();

        let snippet = if text.is_empty() {
            StyledText::new(tr!("captures-snippet-empty"))
        } else {
            let parts = snippet_parts(&text, &added);
            let mut joined = String::new();
            let mut highlights = Vec::new();
            for part in parts {
                let start = joined.len();
                joined.push_str(&part.text);
                if part.changed {
                    highlights.push((
                        start..joined.len(),
                        HighlightStyle {
                            color: Some(t.warning),
                            background_color: Some(t.warning.opacity(0.1)),
                            underline: Some(UnderlineStyle {
                                thickness: px(1.5),
                                color: Some(t.warning.opacity(0.7)),
                                wavy: false,
                            }),
                            ..Default::default()
                        },
                    ));
                }
            }
            StyledText::new(joined).with_highlights(highlights)
        };

        let muted_dot = || {
            div()
                .flex_none()
                .text_color(t.muted_foreground.opacity(0.5))
                .child("·")
        };
        let mut meta = div()
            .flex()
            .items_center()
            .gap(px(7.))
            .min_w_0()
            .text_size(px(11.5))
            .text_color(t.muted_foreground);
        if show_app && let Some(bundle) = &capture.app_bundle_id {
            let name = capture.app_name.clone().unwrap_or_else(|| bundle.clone());
            meta = meta.child(app_tile(self.host.as_ref(), bundle, &name, px(14.), cx));
        }
        if show_app && let Some(name) = capture.app_name.as_ref().filter(|n| !n.is_empty()) {
            meta = meta
                .child(div().min_w_0().truncate().child(name.clone()))
                .child(muted_dot());
        }
        let now = Local::now();
        meta = meta
            .child(div().flex_none().whitespace_nowrap().child(format_row_time(
                &capture.created_at,
                &tr!("captures-list-yesterday"),
                now,
            )))
            .child(muted_dot())
            .child(
                div()
                    .flex_none()
                    .child(format_duration(capture.duration_ms)),
            )
            .child(div().flex_1());
        let chip = |color: Hsla, mark: Div, label: String| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(color)
                .child(mark)
                .child(label)
        };
        if let Some(style) = self.row_style(capture) {
            meta = meta.child(chip(t.accent, dot(t.accent, px(6.), false), style));
        }
        match tag {
            CaptureTag::Review => {
                meta = meta.child(chip(
                    t.warning,
                    dot(t.warning, px(6.), false),
                    tr!("captures-tag-review"),
                ))
            }
            CaptureTag::Command | CaptureTag::CommandFailed => {
                let label = capture.command_transform.clone().unwrap_or_else(|| {
                    tr!(if tag == CaptureTag::Command {
                        "captures-tag-command"
                    } else {
                        "captures-tag-command-failed"
                    })
                });
                meta = meta.child(chip(
                    t.accent,
                    dot(t.accent, px(6.), tag == CaptureTag::CommandFailed),
                    label,
                ));
            }
            CaptureTag::Raw => {
                meta = meta.child(
                    chip(
                        t.muted_foreground,
                        dot(t.muted_foreground.opacity(0.7), px(6.), true),
                        tr!("captures-tag-raw"),
                    )
                    .font_weight(FontWeight::NORMAL),
                )
            }
            CaptureTag::Refined => {}
        }

        let id = capture.id.clone();
        div()
            .id(SharedString::from(format!("capture-{}", capture.id)))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(metrics::RADIUS_LG)
            .cursor_pointer()
            .when(active, |r| r.bg(t.muted))
            .when(!active, |r| r.hover(|s| s.bg(t.muted.opacity(0.5))))
            .on_click(cx.listener(move |page, _, window, cx| {
                page.select(Some(id.clone()), window, cx);
                window.focus(&page.focus, cx);
            }))
            .child(
                div()
                    .text_size(metrics::TEXT_SM)
                    .line_height(px(21.))
                    .text_color(t.foreground)
                    .line_clamp(2)
                    .child(snippet),
            )
            .child(meta)
    }

    /// The filtered app's card: its icon, name, dictation count and style.
    fn render_app_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx).clone();
        let app = self.filter_app()?.clone();
        let name = self.filter_app_name()?;
        let formatted = format_count(app.count);
        let style = app.style_id.as_deref().and_then(|id| self.style_name(id));
        let line = match &style {
            Some(style) => tr!(
                "captures-apps-dictations-teach",
                count = app.count,
                formatted = formatted,
                style = style.clone()
            ),
            None => tr!(
                "captures-apps-dictations",
                count = app.count,
                formatted = formatted
            ),
        };
        let mut card = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .p(px(12.))
            .rounded(metrics::RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .child(app_tile(
                self.host.as_ref(),
                &app.app_bundle_id,
                &name,
                px(32.),
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(t.muted_foreground)
                            .child(line),
                    ),
            );
        if let Some(picker) = &self.style_select {
            let placeholder = style
                .filter(|_| !app.confirmed)
                .map(|style| tr!("captures-apps-suggested", style = style));
            let mut control = select(&picker.state, cx).h(px(30.)).text_size(px(12.5));
            if let Some(placeholder) = placeholder {
                control = control.placeholder(placeholder);
            }
            card = card.child(
                div()
                    .flex_none()
                    .w(px(140.))
                    .id("app-style")
                    .tooltip(move |window, cx| {
                        Tooltip::new(tr!("captures-apps-style-for", app = name.clone()))
                            .build(window, cx)
                    })
                    .child(control),
            );
        }
        Some(card.into_any_element())
    }

    /// The last 7 days: words, pace and time saved, and words per day.
    fn render_week(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let app = self.filter_app_name();
        let label = match &app {
            Some(app) => tr!("captures-week-label-app", app = app.clone()),
            None => tr!("captures-week-label"),
        };
        let totals = self.week.as_ref().map(|w| &w.current);
        let stat = |value: String, label: String| {
            div()
                .flex_none()
                .whitespace_nowrap()
                .child(
                    div()
                        .text_size(px(20.))
                        .line_height(px(25.))
                        .font_weight(FontWeight::BOLD)
                        .child(value),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(t.muted_foreground)
                        .child(label),
                )
        };
        let mut row = div()
            .flex()
            .items_end()
            .gap(px(16.))
            .child(stat(
                totals.map_or("—".into(), |c| format_count(c.words)),
                tr!("captures-week-words-label"),
            ))
            .child(stat(
                totals
                    .and_then(|c| c.pace_wpm)
                    .map_or("—".into(), |p| p.to_string()),
                tr!("captures-week-wpm"),
            ))
            .child(stat(
                totals.map_or("—".into(), |c| format_saved(c.time_saved_ms)),
                tr!("captures-week-saved"),
            ));
        if let Some(week) = &self.week {
            row = row.child(week_bars(&week.series, &t));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(14.))
            .pt(px(8.))
            .pb(px(12.))
            .rounded(metrics::RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .child(
                div()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        mono_label(label, t.muted_foreground, cx)
                            .min_w_0()
                            .truncate(),
                    )
                    .child(
                        div()
                            .id("week-open")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .mr(px(-6.))
                            .rounded(metrics::RADIUS_MD)
                            .cursor_pointer()
                            .hover(|s| s.bg(t.muted))
                            .tooltip(|window, cx| {
                                Tooltip::new(tr!("captures-week-open")).build(window, cx)
                            })
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.navigate(Route::Insights, window, cx)
                            }))
                            .child(icon(IconName::ArrowRight, px(15.), t.accent)),
                    ),
            )
            .child(row)
    }
}

fn week_bars(series: &[UsagePoint], t: &KassTheme) -> Div {
    let days: Vec<&UsagePoint> = series.iter().rev().take(7).rev().collect();
    let max = days
        .iter()
        .map(|d| d.words.unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1) as f32
        * 1.05;
    let mut bars = div().h(px(30.)).flex().items_end().gap(px(4.));
    let mut letters = div().flex().gap(px(4.));
    for (i, day) in days.iter().enumerate() {
        let words = day.words.unwrap_or(0) as f32;
        let today = i + 1 == days.len();
        bars = bars.child(
            div().h_full().w(px(14.)).flex().items_end().child(
                div()
                    .w_full()
                    .min_h(px(2.))
                    .h(relative(words / max))
                    .rounded_t(px(3.))
                    .bg(if today {
                        t.accent
                    } else {
                        t.accent.opacity(0.45)
                    }),
            ),
        );
        let letter = day_of(&day.start)
            .map(|d| format_day(d).chars().next().unwrap_or(' ').to_string())
            .unwrap_or_default();
        letters = letters.child(
            div()
                .w(px(14.))
                .text_center()
                .text_size(px(9.5))
                .text_color(t.muted_foreground)
                .child(letter),
        );
    }
    div()
        .ml_auto()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(bars)
        .child(letters)
}

/// The day a series point starts on ("2026-11-10" or a timestamp).
fn day_of(start: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(start.get(..10)?, "%Y-%m-%d").ok()
}
