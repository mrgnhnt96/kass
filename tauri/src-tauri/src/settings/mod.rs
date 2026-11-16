//! Settings in GPUI: the section list on the left and the pages rebuilt
//! natively (General, Transcription & refinement, Features, Logs). The
//! sections still drawn by the page show in the web view to the right of
//! the list.

mod features;
mod general;
mod logs;
mod transcription;

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, show_toast, theme, Toast, ToastKind};

use crate::shell::{self, data::ShellData};

/// The section list's width.
pub const NAV_WIDTH: f32 = 220.;

/// A settings page drawn natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Transcription,
    Features,
    Logs,
}

impl Page {
    /// The native page at `path`, if it's one.
    pub fn of(path: &str) -> Option<Self> {
        match path.trim_end_matches('/') {
            "/settings" => Some(Self::General),
            "/settings/transcription" => Some(Self::Transcription),
            "/settings/features" => Some(Self::Features),
            "/settings/logs" => Some(Self::Logs),
            _ => None,
        }
    }
}

pub fn is_settings(path: &str) -> bool {
    path == "/settings" || path.starts_with("/settings/")
}

/// The sections, in the list's order.
const TABS: [(&str, &str); 8] = [
    ("settings-tabs-general", "/settings"),
    ("settings-tabs-dictation", "/settings/dictation"),
    ("settings-tabs-command-mode", "/settings/command-mode"),
    ("settings-tabs-transcription", "/settings/transcription"),
    ("settings-tabs-writing-style", "/settings/writing-style"),
    ("settings-tabs-dictionary", "/settings/dictionary"),
    ("settings-tabs-features", "/settings/features"),
    ("settings-tabs-logs", "/settings/logs"),
];

/// The native pages, made once with the main window and kept while it's open.
pub struct Settings {
    general: Entity<general::GeneralPage>,
    transcription: Entity<transcription::TranscriptionPage>,
    features: Entity<features::FeaturesPage>,
    logs: Entity<logs::LogsPage>,
}

impl Settings {
    pub fn new<T>(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<T>) -> Self {
        Self {
            general: cx.new(|cx| general::GeneralPage::new(data.clone(), window, cx)),
            transcription: cx
                .new(|cx| transcription::TranscriptionPage::new(data.clone(), window, cx)),
            features: cx.new(|_| features::FeaturesPage),
            logs: cx.new(logs::LogsPage::new),
        }
    }

    pub fn render<T>(&self, path: &str, _: &mut Window, cx: &mut Context<T>) -> impl IntoElement {
        let page = Page::of(path);
        let content: AnyElement = match page {
            // Logs fills the pane; the rest are a readable column that scrolls.
            Some(Page::Logs) => div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(self.logs.clone())
                .into_any_element(),
            Some(page) => {
                let view: AnyView = match page {
                    Page::General => self.general.clone().into(),
                    Page::Transcription => self.transcription.clone().into(),
                    _ => self.features.clone().into(),
                };
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .px(px(48.))
                            .py(px(28.))
                            .max_w(px(760.))
                            .child(view),
                    )
                    .into_any_element()
            }
            // The page's own section: the web view covers this.
            None => div().flex_1().into_any_element(),
        };
        div()
            .flex()
            .size_full()
            .min_h_0()
            .child(nav(path, cx))
            .child(content)
    }
}

fn nav(path: &str, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    let current = path.trim_end_matches('/');
    let (muted, hover_fg, fg, accent, active_bg) =
        (t.muted_foreground, t.foreground, t.foreground, t.accent, t.muted);
    div()
        .w(px(NAV_WIDTH))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(10.))
        .py(px(20.))
        .border_r_1()
        .border_color(t.border)
        .bg(t.sidebar)
        .child(
            div()
                .px(px(10.))
                .pb(px(14.))
                .text_size(metrics::TEXT_LG)
                .line_height(metrics::LINE_LG)
                .font_weight(FontWeight::SEMIBOLD)
                .child(kass_i18n::t("nav-settings")),
        )
        .children(TABS.iter().map(move |&(label, to)| {
            let active = current == to;
            div()
                .id(to)
                .relative()
                .flex()
                .items_center()
                .h(px(34.))
                .px(px(10.))
                .rounded(metrics::RADIUS_MD)
                .text_size(metrics::TEXT_BASE)
                .cursor_pointer()
                .when(active, |tab| {
                    tab.bg(active_bg).text_color(fg).child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .bottom_0()
                            .w(px(2.))
                            .bg(accent),
                    )
                })
                .when(!active, |tab| {
                    tab.text_color(muted).hover(move |s| s.text_color(hover_fg))
                })
                .child(kass_i18n::t(label))
                .on_click(move |_, _, cx| shell::navigate(to, "", cx))
        }))
}

/// A muted line of text beside a control, like "Downloaded".
pub(crate) fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .text_size(metrics::TEXT_BASE)
        .text_color(theme(cx).muted_foreground)
        .whitespace_nowrap()
        .child(text.into())
}

/// Say that `title` failed, with the error under it.
pub(crate) fn toast_error(title: impl Into<SharedString>, error: &str, window: &mut Window, cx: &mut App) {
    show_toast(
        window,
        cx,
        Toast::new(title)
            .description(error.to_string())
            .kind(ToastKind::Destructive),
    );
}

#[cfg(test)]
mod tests {
    use super::{is_settings, Page};

    #[test]
    fn native_pages_by_path() {
        assert_eq!(Page::of("/settings"), Some(Page::General));
        assert_eq!(Page::of("/settings/"), Some(Page::General));
        assert_eq!(Page::of("/settings/transcription"), Some(Page::Transcription));
        assert_eq!(Page::of("/settings/features"), Some(Page::Features));
        assert_eq!(Page::of("/settings/logs"), Some(Page::Logs));
        assert_eq!(Page::of("/settings/dictation"), None);
        assert_eq!(Page::of("/captures"), None);
    }

    #[test]
    fn settings_paths() {
        assert!(is_settings("/settings"));
        assert!(is_settings("/settings/dictionary"));
        assert!(!is_settings("/settingsx"));
        assert!(!is_settings("/models"));
    }
}
