//! Beta `network`: once the other Mac has stayed away a while, a line at
//! the top of the window offers to use this Mac for good. Kass already
//! dictates on this Mac while it's away; this makes that stick, or hides
//! until the other Mac comes back and goes away again.

use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::{t, tr};
use kass_ui::{metrics, show_toast, theme, Button, Toast, ToastKind};

use super::data::ShellData;
use super::status::mac_name;
use crate::host;
use crate::prefs;

/// How long the other Mac stays away before Kass offers this one.
pub const OFFER_AFTER: Duration = Duration::from_secs(120);

/// The other Mac, away since `since`, has been away long enough to offer
/// this Mac instead.
pub fn long_away(since: Option<Instant>, now: Instant) -> bool {
    since.is_some_and(|since| now.saturating_duration_since(since) >= OFFER_AFTER)
}

/// The line, while there's something to offer.
pub fn banner(data: &Entity<ShellData>, cx: &App) -> Option<AnyElement> {
    let shell = data.read(cx);
    let remote = shell.remote()?;
    if shell.away_dismissed || !long_away(shell.away_since, Instant::now()) {
        return None;
    }
    let switching = shell.switching_to_this_mac;
    let t_ = theme(cx);
    let use_this = data.clone();
    let keep_waiting = data.clone();
    Some(
        div()
            .id("away-banner")
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px(px(16.))
            .py(px(8.))
            .border_b_1()
            .border_color(t_.accent.opacity(0.3))
            .bg(t_.accent.opacity(0.08))
            .text_size(metrics::TEXT_SM)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(tr!("nav-away-title", mac = mac_name(&remote)))
                    .child(
                        div()
                            .text_size(metrics::TEXT_XS)
                            .text_color(t_.muted_foreground)
                            .child(t("nav-away-description")),
                    ),
            )
            .child(
                Button::new("away-keep-waiting")
                    .ghost()
                    .small()
                    .label(t("nav-away-keep-waiting"))
                    .disabled(switching)
                    .on_click(move |_, _, cx| {
                        keep_waiting.update(cx, |data, cx| {
                            data.away_dismissed = true;
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new("away-use-this-mac")
                    .small()
                    .label(t("nav-away-use-this-mac"))
                    .loading(switching)
                    .disabled(switching)
                    .on_click(move |_, window, cx| use_this_mac(&use_this, window, cx)),
            )
            .into_any_element(),
    )
}

/// Stop using the other Mac and restart on this one, as Settings does.
fn use_this_mac(data: &Entity<ShellData>, window: &mut Window, cx: &mut App) {
    data.update(cx, |data, cx| {
        data.switching_to_this_mac = true;
        cx.notify();
    });
    let switched = host::async_runtime::spawn(prefs::use_remote(None));
    let data = data.clone();
    window
        .spawn(cx, async move |cx| {
            let result = switched.await.unwrap_or_else(|e| Err(e.to_string()));
            let Err(error) = result else {
                return;
            };
            let _ = cx.update(|window, cx| {
                data.update(cx, |data, cx| {
                    data.switching_to_this_mac = false;
                    cx.notify();
                });
                show_toast(
                    window,
                    cx,
                    Toast::new(t("settings-general-restart-failed"))
                        .description(error)
                        .kind(ToastKind::Destructive),
                );
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::{long_away, OFFER_AFTER};
    use std::time::{Duration, Instant};

    #[test]
    fn offers_this_mac_once_the_other_has_been_away_two_minutes() {
        let now = Instant::now();
        assert!(!long_away(None, now));
        assert!(!long_away(Some(now), now));
        assert!(!long_away(
            now.checked_sub(OFFER_AFTER - Duration::from_secs(1)),
            now
        ));
        assert!(long_away(now.checked_sub(OFFER_AFTER), now));
    }
}
