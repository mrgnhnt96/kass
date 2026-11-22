//! Reports: zip the logs, crash reports and system info into Downloads, to
//! send when something goes wrong. `kass-report.sh` decides what goes in, so
//! the button and the command make the same zip. Nothing is uploaded.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, theme, Button, SettingSection};

use crate::host::{self, AppHandle};
use crate::report;
use crate::settings::toast_error;

/// The shipped script the button runs; Terminal or an AI agent can run it too.
pub const REPORT_COMMAND: &str = "bash /Applications/Kass.app/Contents/Resources/kass-report.sh";

/// How long "Copied" shows after copying the command.
const COPIED_FOR: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct ReportsPage {
    creating: bool,
    /// The last zip made while the window was open.
    report: Option<String>,
    copied: bool,
    /// Bumped on each copy, so only the last copy's timer clears "Copied".
    copies: usize,
}

/// The last part of a path: the zip's name.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl ReportsPage {
    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        self.creating = true;
        cx.notify();
        let create = host::async_runtime::spawn(report::create_report(AppHandle));
        cx.spawn_in(window, async move |this, cx| {
            let result = create.await.unwrap_or_else(|e| Err(e.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.creating = false;
                match result {
                    Ok(path) => this.report = Some(path),
                    Err(e) => toast_error(kass_i18n::t("reports-failed"), &e, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reveal(path: String, window: &mut Window, cx: &mut App) {
        if let Err(e) = report::reveal_report(path) {
            toast_error(kass_i18n::t("reports-reveal-failed"), &e, window, cx);
        }
    }

    fn copy_command(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(REPORT_COMMAND.to_string()));
        self.copied = true;
        self.copies += 1;
        let copy = self.copies;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |this, cx| {
                if this.copies == copy {
                    this.copied = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The create button, and the zip it made.
    fn create_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let made = self.report.clone().map(|path| {
            let shown = file_name(&path).to_string();
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(metrics::RADIUS_MD)
                .bg(t.muted)
                .px(px(12.))
                .py(px(8.))
                .child(
                    svg()
                        .path(IconName::Check.path())
                        .size(px(16.))
                        .flex_none()
                        .text_color(t.accent),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_XS)
                        .child(shown),
                )
                .child(
                    Button::new("reports-reveal")
                        .outline()
                        .small()
                        .icon(IconName::FolderOpen)
                        .label(kass_i18n::t("reports-reveal"))
                        .h(px(28.))
                        .px(px(10.))
                        .text_size(metrics::TEXT_XS)
                        .on_click(move |_, window, cx| Self::reveal(path.clone(), window, cx)),
                )
        });
        div()
            .mb(px(32.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(metrics::RADIUS_LG)
            .border_1()
            .border_color(t.border)
            .p(px(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        Button::new("reports-create")
                            .icon(IconName::FileArchive)
                            .loading(self.creating)
                            .label(if self.creating {
                                kass_i18n::t("reports-creating")
                            } else {
                                kass_i18n::t("reports-create")
                            })
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t.muted_foreground)
                            .child(kass_i18n::t("reports-where")),
                    ),
            )
            .children(made)
    }

    /// The same report from Terminal, with a copy button.
    fn command(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        SettingSection::new(kass_i18n::t("reports-command-title"))
            .description(kass_i18n::t("reports-command-description"))
            .child(
                div()
                    .my(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.sidebar)
                    .py(px(6.))
                    .pr(px(6.))
                    .pl(px(12.))
                    .child(
                        div()
                            .id("reports-command")
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .whitespace_nowrap()
                            .font_family(t.mono_font.clone())
                            .text_size(metrics::TEXT_XS)
                            .child(REPORT_COMMAND),
                    )
                    .child(
                        Button::new("reports-copy")
                            .ghost()
                            .small()
                            .icon(if self.copied {
                                IconName::Check
                            } else {
                                IconName::Copy
                            })
                            .label(if self.copied {
                                kass_i18n::t("reports-command-copied")
                            } else {
                                kass_i18n::t("reports-command-copy")
                            })
                            .h(px(28.))
                            .px(px(8.))
                            .text_size(metrics::TEXT_XS)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_command(cx))),
                    ),
            )
    }
}

impl Render for ReportsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let body = |text: String| {
            div()
                .text_size(metrics::TEXT_BASE)
                .line_height(metrics::LINE_BASE)
                .child(text)
        };
        let contents = SettingSection::new(kass_i18n::t("reports-contents-title")).child(
            div()
                .py(px(14.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(body(kass_i18n::t("reports-contents-logs")))
                .child(body(kass_i18n::t("reports-contents-crashes")))
                .child(body(kass_i18n::t("reports-contents-system"))),
        );
        let private = SettingSection::new(kass_i18n::t("reports-private-title")).child(
            body(kass_i18n::t("reports-private-body"))
                .py(px(14.))
                .text_color(t.muted_foreground),
        );
        let fg = t.foreground;
        let live_logs = div()
            .flex()
            .flex_wrap()
            .gap(px(4.))
            .text_size(metrics::TEXT_XS)
            .text_color(t.muted_foreground)
            .child(kass_i18n::t("reports-live-logs-prefix"))
            .child(
                div()
                    .id("reports-live-logs")
                    .text_color(fg)
                    .underline()
                    .cursor_pointer()
                    .child(kass_i18n::t("reports-live-logs"))
                    .on_click(|_, _, cx| super::navigate("/settings/logs", "", cx)),
            );

        div().id("reports").size_full().overflow_y_scroll().child(
            div()
                .max_w(px(760.))
                .px(px(48.))
                .py(px(28.))
                .child(
                    div()
                        .text_size(metrics::TEXT_LG)
                        .line_height(metrics::LINE_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(kass_i18n::t("reports-title")),
                )
                .child(
                    body(kass_i18n::t("reports-intro"))
                        .mt(px(4.))
                        .mb(px(24.))
                        .text_color(t.muted_foreground),
                )
                .child(self.create_card(cx))
                .child(contents)
                .child(private)
                .child(self.command(cx))
                .child(live_logs),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn shows_the_zip_by_name() {
        assert_eq!(
            file_name("/Users/me/Downloads/Kass-report-2026-10-08-090000.zip"),
            "Kass-report-2026-10-08-090000.zip"
        );
        assert_eq!(file_name("report.zip"), "report.zip");
    }
}
