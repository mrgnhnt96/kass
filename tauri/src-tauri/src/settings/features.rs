//! Settings › Features: everything Kass does besides hold and talk, each
//! with a way to try it, and onboarding again.

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{metrics, theme, Button, SettingRow, SettingSection};

use super::toast_error;
use crate::shell::{self, bridge};

/// A feature, and the screen where it's set up or used (`None`: ⌘K).
const FEATURES: [(&str, Option<&str>); 9] = [
    ("styles", Some("/settings/writing-style")),
    ("teach", Some("/settings/writing-style")),
    ("dictionary", Some("/settings/dictionary")),
    ("fix-capture", Some("/captures")),
    ("formatting", Some("/settings/transcription")),
    ("hands-free", Some("/settings/dictation")),
    ("command-mode", Some("/settings/command-mode")),
    ("palette", None),
    ("insights", Some("/insights")),
];

pub struct FeaturesPage;

impl Render for FeaturesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let features = SettingSection::new(kass_i18n::t("settings-features-title"))
            .description(kass_i18n::t("settings-features-description"))
            .children(FEATURES.iter().map(|&(key, to)| {
                let action = match to {
                    Some(to) => Button::new(SharedString::from(format!("try-{key}")))
                        .outline()
                        .small()
                        .label(kass_i18n::t("settings-features-try-it"))
                        .on_click(move |_, _, cx| shell::navigate(to, "", cx))
                        .into_any_element(),
                    None => div()
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(metrics::RADIUS_SM)
                        .border_1()
                        .border_color(t.input)
                        .bg(t.secondary)
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_XS)
                        .child("⌘K")
                        .into_any_element(),
                };
                SettingRow::new(kass_i18n::t(&format!("settings-features-items-{key}-title")))
                    .description(kass_i18n::t(&format!(
                        "settings-features-items-{key}-description"
                    )))
                    .action(action)
            }));
        let onboarding =
            SettingSection::new(kass_i18n::t("settings-features-onboarding-section-title")).child(
                SettingRow::new(kass_i18n::t("settings-features-onboarding-title"))
                    .description(kass_i18n::t("settings-features-onboarding-description"))
                    .action(
                        Button::new("open-onboarding")
                            .outline()
                            .small()
                            .label(kass_i18n::t("settings-features-onboarding-action"))
                            .on_click(|_, window, cx| {
                                window
                                    .spawn(cx, async move |cx| {
                                        if let Err(e) =
                                            bridge::send(bridge::Command::OpenOnboarding).await
                                        {
                                            let _ = cx.update(|window, cx| {
                                                toast_error(
                                                    kass_i18n::t(
                                                        "settings-features-onboarding-title",
                                                    ),
                                                    &e,
                                                    window,
                                                    cx,
                                                )
                                            });
                                        }
                                    })
                                    .detach();
                            }),
                    ),
            );
        div().flex().flex_col().child(features).child(onboarding)
    }
}
