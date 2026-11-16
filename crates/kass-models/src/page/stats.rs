//! The model's facts in cards: size on disk and what Hugging Face says
//! about it, and its siblings' sizes to compare with.

use super::Info;
use crate::catalog::{format_count, format_license, format_pipeline_tag, format_size_mb};
use crate::downloads::Downloads;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{HuggingFaceModelInfo, ModelStatus};
use kass_i18n::{t, tr};
use kass_ui::{KassTheme, metrics};
use std::time::Duration;

/// A fact's value: shown, still loading, or unknown.
enum Value {
    Text(String),
    Loading,
    Unknown,
}

fn license(info: &HuggingFaceModelInfo) -> Option<String> {
    info.card_data
        .as_ref()
        .and_then(|card| card.license.clone())
        .or_else(|| {
            info.tags
                .iter()
                .find_map(|tag| tag.strip_prefix("license:").map(str::to_string))
        })
        .map(|license| format_license(&license))
}

fn languages(info: &HuggingFaceModelInfo) -> Option<String> {
    let languages = info.card_data.as_ref()?.language.as_ref()?;
    match languages.len() {
        0 => None,
        n if n > 4 => Some(tr!("models-stats-languages-count", count = n)),
        _ => Some(languages.join(", ")),
    }
}

pub(super) fn stats(model: &ModelStatus, info: Option<&Info>, t_: &KassTheme) -> Div {
    let hf = |get: &dyn Fn(&HuggingFaceModelInfo) -> Option<String>| match info {
        Some(Info::Loaded(info, _)) => get(info).map_or(Value::Unknown, Value::Text),
        Some(Info::Loading) => Value::Loading,
        _ => Value::Unknown,
    };
    let disk = match (model.downloaded, model.size_mb) {
        (true, Some(size)) => Value::Text(format_size_mb(size)),
        _ => Value::Unknown,
    };
    let cards = vec![
        (t("models-stats-on-disk"), disk),
        (t("models-detail-license"), hf(&license)),
        (t("models-stats-languages"), hf(&languages)),
        (
            t("models-detail-downloads"),
            hf(&|info| Some(format_count(info.downloads))),
        ),
        (
            t("models-detail-likes"),
            hf(&|info| Some(format_count(info.likes))),
        ),
        (
            t("models-stats-task"),
            hf(&|info| info.pipeline_tag.as_deref().map(format_pipeline_tag)),
        ),
        (
            t("models-stats-library"),
            hf(&|info| info.library_name.clone()),
        ),
        (
            t("models-stats-author"),
            hf(&|info| Some(info.author.clone()).filter(|a| !a.is_empty())),
        ),
    ];
    grid(
        cards
            .into_iter()
            .enumerate()
            .map(|(ix, (label, value))| stat_card(ix, label, value, t_).into_any_element())
            .collect(),
    )
}

fn stat_card(ix: usize, label: String, value: Value, t_: &KassTheme) -> Div {
    let value = match value {
        Value::Text(text) => div()
            .text_size(px(15.))
            .truncate()
            .child(text)
            .into_any_element(),
        Value::Unknown => div()
            .text_size(px(15.))
            .text_color(t_.muted_foreground)
            .child("—")
            .into_any_element(),
        Value::Loading => div()
            .h(px(18.))
            .w(px(64.))
            .rounded(metrics::RADIUS_SM)
            .bg(t_.muted)
            .with_animation(
                ("stat-loading", ix),
                Animation::new(Duration::from_millis(1400))
                    .repeat()
                    .with_easing(pulsating_between(0.4, 1.)),
                |skeleton, delta| skeleton.opacity(delta),
            )
            .into_any_element(),
    };
    card(t_)
        .child(label_text(label, t_))
        .child(div().mt(px(6.)).child(value))
}

/// The family's sizes side by side, `current` outlined.
pub(super) fn compare(
    siblings: &[ModelStatus],
    current: &str,
    downloads: &Downloads,
    t_: &KassTheme,
) -> Div {
    let cards = siblings
        .iter()
        .map(|model| {
            let state = downloads.state_of(model);
            let detail = if state.downloading {
                t("models-compare-downloading")
            } else if model.downloaded {
                model.size_mb.map(format_size_mb).unwrap_or_default()
            } else {
                t("models-compare-not-downloaded")
            };
            let is_current = model.model_name == current;
            card(t_)
                .when(is_current, |card| card.border_color(t_.accent))
                .child(
                    div()
                        .text_size(metrics::TEXT_BASE)
                        .font_weight(FontWeight::MEDIUM)
                        .truncate()
                        .child(model.display_name.clone()),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .font_family(t_.mono_font.clone())
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(t_.muted_foreground)
                        .child(detail),
                )
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_size(metrics::TEXT_BASE)
                .font_weight(FontWeight::SEMIBOLD)
                .child(t("models-detail-compare")),
        )
        .child(grid(cards))
}

/// Cards three to a row.
fn grid(mut cards: Vec<AnyElement>) -> Div {
    let mut rows = Vec::new();
    while !cards.is_empty() {
        let rest = cards.split_off(cards.len().min(3));
        let mut row: Vec<AnyElement> = std::mem::replace(&mut cards, rest)
            .into_iter()
            .map(|card| div().flex_1().min_w_0().child(card).into_any_element())
            .collect();
        while row.len() < 3 {
            row.push(div().flex_1().into_any_element());
        }
        rows.push(div().flex().gap_3().children(row));
    }
    div().flex().flex_col().gap_3().children(rows)
}

fn card(t_: &KassTheme) -> Div {
    div()
        .h_full()
        .p(px(14.))
        .rounded(metrics::RADIUS_LG)
        .border_1()
        .border_color(t_.border)
        .bg(t_.card)
}

fn label_text(text: String, t_: &KassTheme) -> Div {
    div()
        .font_family(t_.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .text_color(t_.muted_foreground)
        .child(text.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::{languages, license};
    use kass_api::types::{HuggingFaceCardData, HuggingFaceModelInfo};

    fn info(card: Option<HuggingFaceCardData>, tags: &[&str]) -> HuggingFaceModelInfo {
        HuggingFaceModelInfo {
            id: "openai/whisper-base".into(),
            author: "openai".into(),
            last_modified: String::new(),
            pipeline_tag: None,
            library_name: None,
            downloads: 0,
            likes: 0,
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            card_data: card,
        }
    }

    #[test]
    fn license_from_card_or_tags() {
        let card = HuggingFaceCardData {
            license: Some("mit".into()),
            ..Default::default()
        };
        assert_eq!(license(&info(Some(card), &[])).as_deref(), Some("MIT"));
        assert_eq!(
            license(&info(None, &["license:apache-2.0"])).as_deref(),
            Some("Apache 2.0")
        );
        assert_eq!(license(&info(None, &["audio"])), None);
    }

    #[test]
    fn languages_listed_or_counted() {
        let with = |langs: &[&str]| {
            info(
                Some(HuggingFaceCardData {
                    language: Some(langs.iter().map(|l| l.to_string()).collect()),
                    ..Default::default()
                }),
                &[],
            )
        };
        assert_eq!(languages(&with(&["en", "fr"])).as_deref(), Some("en, fr"));
        assert!(
            languages(&with(&["a", "b", "c", "d", "e"]))
                .unwrap()
                .contains('5')
        );
        assert_eq!(languages(&with(&[])), None);
    }
}
