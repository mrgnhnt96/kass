//! Settings in GPUI: the section list on the left and the section's page
//! to its right (General, Dictation, Command mode, Transcription &
//! refinement, Writing style, Dictionary, Features, Logs).

mod chord;
mod command_mode;
mod dictation;
pub mod dictionary;
mod features;
pub(crate) mod general;
mod hud_preview;
mod logs;
mod transcription;
mod writing_style;

use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_dictionary::DictionaryPage;
use kass_ui::{metrics, show_toast, theme, Toast, ToastKind};
use kass_writing_style::WritingStylePage;

use crate::shell::{self, data::ShellData};

/// The section list's width.
pub const NAV_WIDTH: f32 = 220.;

/// A settings page drawn natively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Dictation,
    CommandMode,
    Transcription,
    WritingStyle,
    Dictionary,
    Features,
    Logs,
}

impl Page {
    /// The native page at `path`, if it's one.
    pub fn of(path: &str) -> Option<Self> {
        match path.trim_end_matches('/') {
            "/settings" => Some(Self::General),
            "/settings/dictation" => Some(Self::Dictation),
            "/settings/command-mode" => Some(Self::CommandMode),
            "/settings/transcription" => Some(Self::Transcription),
            "/settings/writing-style" => Some(Self::WritingStyle),
            dictionary::PATH => Some(Self::Dictionary),
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
    dictation: Entity<dictation::DictationPage>,
    command_mode: Entity<command_mode::CommandModePage>,
    transcription: Entity<transcription::TranscriptionPage>,
    writing_style: Entity<WritingStylePage>,
    dictionary: Entity<DictionaryPage>,
    features: Entity<features::FeaturesPage>,
    logs: Entity<logs::LogsPage>,
    /// The page and search last drawn, to tell when Writing style or the
    /// Dictionary is shown.
    shown: Option<(Page, String)>,
}

impl Settings {
    pub fn new<T>(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<T>) -> Self {
        Self {
            general: cx.new(|cx| general::GeneralPage::new(data.clone(), window, cx)),
            dictation: cx.new(|cx| dictation::DictationPage::new(data.clone(), window, cx)),
            command_mode: cx.new(|cx| command_mode::CommandModePage::new(data.clone(), window, cx)),
            transcription: cx
                .new(|cx| transcription::TranscriptionPage::new(data.clone(), window, cx)),
            writing_style: cx.new(|cx| {
                WritingStylePage::new(data.read(cx).api.clone(), window, cx)
                    .with_dictation(Rc::new(writing_style::HostDictation), cx)
            }),
            dictionary: cx.new(|cx| {
                DictionaryPage::new(data.read(cx).api.clone(), window, cx)
                    .with_dictation(Rc::new(dictionary::HostDictation::default()), cx)
            }),
            features: cx.new(|_| features::FeaturesPage),
            logs: cx.new(logs::LogsPage::new),
            shown: None,
        }
    }

    pub fn render<T>(
        &mut self,
        path: &str,
        search: &str,
        _: &mut Window,
        cx: &mut Context<T>,
    ) -> impl IntoElement {
        let page = Page::of(path);
        let shown = page.map(|page| (page, search.to_string()));
        if shown != self.shown {
            // Writing style fetches again each time it's shown (styles and
            // their apps change as captures come in), and opens the style
            // a link asks for.
            if page == Some(Page::WritingStyle) {
                let style = writing_style::style_in(search);
                self.writing_style.update(cx, |page, cx| {
                    page.refresh(cx);
                    page.show(style, cx);
                });
            }
            // So does the Dictionary (Captures adds to it), on the scope and
            // kind a link asks for.
            if page == Some(Page::Dictionary) {
                let (scope, phrases) = dictionary::link(search);
                self.dictionary.update(cx, |page, cx| {
                    page.refresh(cx);
                    page.show(scope, phrases, cx);
                });
            }
            self.shown = shown;
        }
        let content: AnyElement = match page {
            // Logs, the Dictionary and Writing style fill the pane (the
            // Dictionary scrolls its own wider column, Writing style its
            // own, to pin the chosen style's header); the rest are a
            // readable column that scrolls.
            Some(page @ (Page::Logs | Page::Dictionary | Page::WritingStyle)) => {
                let view: AnyView = match page {
                    Page::Logs => self.logs.clone().into(),
                    Page::Dictionary => self.dictionary.clone().into(),
                    _ => self.writing_style.clone().into(),
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(view)
                    .into_any_element()
            }
            Some(page) => {
                let view: AnyView = match page {
                    Page::General => self.general.clone().into(),
                    Page::Dictation => self.dictation.clone().into(),
                    Page::CommandMode => self.command_mode.clone().into(),
                    Page::Transcription => self.transcription.clone().into(),
                    _ => self.features.clone().into(),
                };
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .child(div().px(px(48.)).py(px(28.)).max_w(px(760.)).child(view))
                    .into_any_element()
            }
            // No section by that path.
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
    let (muted, hover_fg, fg, accent, active_bg) = (
        t.muted_foreground,
        t.foreground,
        t.foreground,
        t.accent,
        t.muted,
    );
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
pub(crate) fn toast_error(
    title: impl Into<SharedString>,
    error: &str,
    window: &mut Window,
    cx: &mut App,
) {
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
        assert_eq!(
            Page::of("/settings/transcription"),
            Some(Page::Transcription)
        );
        assert_eq!(
            Page::of("/settings/writing-style"),
            Some(Page::WritingStyle)
        );
        assert_eq!(Page::of("/settings/dictionary"), Some(Page::Dictionary));
        assert_eq!(Page::of("/settings/features"), Some(Page::Features));
        assert_eq!(Page::of("/settings/logs"), Some(Page::Logs));
        assert_eq!(Page::of("/settings/dictation"), Some(Page::Dictation));
        assert_eq!(Page::of("/settings/command-mode"), Some(Page::CommandMode));
        assert_eq!(Page::of("/captures"), None);
    }

    #[test]
    fn settings_paths() {
        assert!(is_settings("/settings"));
        assert!(is_settings("/settings/dictionary"));
        assert!(is_settings("/settings/writing-style"));
        assert!(!is_settings("/settingsx"));
        assert!(!is_settings("/models"));
    }
}
