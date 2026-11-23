//! The chosen style's name and apps, pinned to the top of the page once the
//! style cards scroll away, so every setting below says whose it is.

use super::WritingStylePage;
use crate::model::{app_label, apps_in};
use crate::widgets::{app_tile, default_badge};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{WritingStyle, WritingStylesResponse};
use kass_i18n::tr;
use kass_ui::{metrics, theme};
use std::time::Duration;

/// App icons the pinned header shows before "+N more".
const PINNED_APPS: usize = 8;

/// The block that starts at `block_top` has scrolled above the view: the
/// view's top is at `view_top` and its content is moved by `offset` (zero
/// or less, as GPUI keeps it). The block's top is where it's laid out
/// before scrolling.
pub(super) fn scrolled_past(view_top: Pixels, offset: Pixels, block_top: Pixels) -> bool {
    block_top < view_top - offset
}

impl WritingStylePage {
    /// Whether the chosen style's settings, the scroll view's child at
    /// `index`, have scrolled above the top, as of the last layout.
    pub(super) fn settings_scrolled_past(&self, index: usize) -> bool {
        self.scroll.bounds_for_item(index).is_some_and(|block| {
            scrolled_past(
                self.scroll.bounds().top(),
                self.scroll.offset().y,
                block.top(),
            )
        })
    }

    /// The header pinned over the top of the page, as wide as its column.
    pub(super) fn render_pinned(
        &self,
        data: &WritingStylesResponse,
        style: &WritingStyle,
        width: Pixels,
        cx: &App,
    ) -> AnyElement {
        let t = theme(cx);
        let apps = apps_in(data, &style.id);
        let rest = apps.len().saturating_sub(PINNED_APPS);
        let tiles = apps.iter().take(PINNED_APPS).map(|app| {
            app_tile(
                &app.bundle_id,
                &app_label(app),
                px(24.),
                px(11.),
                px(6.),
                cx,
            )
        });
        let note = |text: String| {
            div()
                .text_size(metrics::TEXT_XS)
                .text_color(t.muted_foreground)
                .child(text)
        };

        div()
            .id("pinned-style")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .max_w(width)
            .flex()
            .items_center()
            .gap_3()
            .px(px(48.))
            .py(px(10.))
            .border_b_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.background)
            .occlude()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.foreground)
                    .child(style.name.clone()),
            )
            .when(style.is_default, |el| el.child(default_badge(cx)))
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .children(tiles)
                    .when(rest > 0, |el| {
                        el.child(note(tr!("writing-style-styles-more", count = rest)))
                    })
                    .when(apps.is_empty(), |el| {
                        el.child(note(tr!("writing-style-styles-count", count = 0)))
                    }),
            )
            .with_animation(
                "pinned-style-in",
                Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()),
                |el, delta| el.opacity(delta).mt(px(-8.) * (1. - delta)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::scrolled_past;
    use gpui_kit::px;

    #[test]
    fn pins_once_the_settings_scroll_above_the_view() {
        // The view starts 40px down the window; the settings 300px further.
        assert!(!scrolled_past(px(40.), px(0.), px(340.)));
        assert!(!scrolled_past(px(40.), px(-300.), px(340.)));
        assert!(scrolled_past(px(40.), px(-301.), px(340.)));
    }
}
