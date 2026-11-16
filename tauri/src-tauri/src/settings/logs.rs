//! Settings › Logs: the server's output since launch, newest at the bottom,
//! following new lines until you scroll up.

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, show_toast, theme, Button, Toast};

use crate::shell::logs::{self, Entry, Level};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Errors,
}

pub struct LogsPage {
    filter: Filter,
    /// Follow new lines; off once you scroll away from the bottom.
    follow: bool,
    /// A scroll happened: check whether it left the bottom.
    scrolled: bool,
    scroll: ScrollHandle,
    _feed: Subscription,
}

/// How close to the bottom still counts as at it.
const NEAR_BOTTOM: f32 = 40.;

impl LogsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let feed = logs::feed(cx);
        Self {
            filter: Filter::All,
            follow: true,
            scrolled: false,
            scroll: ScrollHandle::new(),
            _feed: cx.observe(&feed, |_, _, cx| cx.notify()),
        }
    }

    fn at_bottom(&self) -> bool {
        let offset = -f32::from(self.scroll.offset().y);
        let max = f32::from(self.scroll.max_offset().y);
        max - offset < NEAR_BOTTOM
    }
}

fn time(entry: &Entry) -> String {
    entry.time.format("%H:%M:%S").to_string()
}

fn shown(entries: Vec<Entry>, filter: Filter) -> Vec<Entry> {
    match filter {
        Filter::All => entries,
        Filter::Errors => entries
            .into_iter()
            .filter(|entry| entry.level == Level::Error)
            .collect(),
    }
}

impl Render for LogsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.scrolled) {
            self.follow = self.at_bottom();
        }
        if self.follow {
            self.scroll.scroll_to_bottom();
        }
        let t = theme(cx);
        let all = logs::entries();
        let total = all.len();
        let lines = shown(all, self.filter);
        let count = lines.len();

        let filter_button = |id: &'static str, label: String, filter: Filter| {
            let active = self.filter == filter;
            let (fg, muted, border, input, secondary) =
                (t.foreground, t.muted_foreground, t.border, t.input, t.secondary);
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(metrics::RADIUS_MD)
                .border_1()
                .cursor_pointer()
                .when(active, |b| b.border_color(input).bg(secondary).text_color(fg))
                .when(!active, |b| {
                    b.border_color(border)
                        .text_color(muted)
                        .hover(move |s| s.text_color(fg))
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.filter = filter;
                    this.follow = true;
                    cx.notify();
                }))
        };

        let toolbar = div()
            .h(px(56.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .border_b_1()
            .border_color(t.border)
            .text_size(metrics::TEXT_XS)
            .child(
                div()
                    .font_family(t.mono_font.clone())
                    .text_color(t.muted_foreground)
                    .child(kass_i18n::tr!("settings-logs-line-count", count = total)),
            )
            .child(div().w(px(12.)))
            .child(filter_button(
                "logs-all",
                kass_i18n::t("settings-logs-filter-all"),
                Filter::All,
            ))
            .child(filter_button(
                "logs-errors",
                kass_i18n::t("settings-logs-filter-errors"),
                Filter::Errors,
            ))
            .child(div().flex_1())
            .child(
                Button::new("logs-copy")
                    .outline()
                    .small()
                    .label(kass_i18n::t("settings-logs-copy-all"))
                    .disabled(count == 0)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let text = shown(logs::entries(), this.filter)
                            .iter()
                            .map(|entry| format!("{} {}", time(entry), entry.line))
                            .collect::<Vec<_>>()
                            .join("\n");
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        show_toast(window, cx, Toast::new(kass_i18n::t("settings-logs-copied")));
                    })),
            )
            .child(
                Button::new("logs-latest")
                    .outline()
                    .small()
                    .label(kass_i18n::t("settings-logs-jump-to-latest"))
                    .disabled(self.follow)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.follow = true;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("logs-clear")
                    .ghost()
                    .small()
                    .label(kass_i18n::t("settings-logs-clear"))
                    .text_color(t.destructive)
                    .disabled(total == 0)
                    .on_click(|_, _, cx| logs::clear(cx)),
            );

        let body: AnyElement = if lines.is_empty() {
            div()
                .px(px(20.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_XS)
                .text_color(t.muted_foreground)
                .child(if self.filter == Filter::Errors && total > 0 {
                    kass_i18n::t("settings-logs-no-errors")
                } else {
                    kass_i18n::t("settings-logs-empty")
                })
                .when(total == 0 && cfg!(debug_assertions), |d| {
                    d.child(kass_i18n::t("settings-logs-dev-hint"))
                })
                .into_any_element()
        } else {
            let time_color = t.muted_foreground.opacity(0.6);
            let (error, error_bg, warn, info) = (
                t.destructive,
                t.destructive.opacity(0.07),
                t.warning,
                t.foreground.opacity(0.8),
            );
            div()
                .flex()
                .flex_col()
                .children(lines.into_iter().map(move |entry| {
                    div()
                        .flex()
                        .gap(px(14.))
                        .px(px(20.))
                        .text_size(metrics::TEXT_XS)
                        .line_height(px(21.))
                        .map(|row| match entry.level {
                            Level::Error => row.bg(error_bg).text_color(error),
                            Level::Warn => row.text_color(warn),
                            Level::Info => row.text_color(info),
                        })
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(time_color)
                                .child(time(&entry)),
                        )
                        .child(div().flex_1().min_w_0().child(entry.line))
                }))
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(toolbar)
            .child(
                div()
                    .id("logs")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .bg(t.sidebar)
                    .py(px(12.))
                    .font_family(t.mono_font.clone())
                    .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                        this.scrolled = true;
                        cx.notify();
                    }))
                    .child(body),
            )
    }
}
