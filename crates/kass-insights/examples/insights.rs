//! The Insights page on its own, against a running Kass server.
//!
//! `cargo run -p kass-insights --example insights`

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_insights::{InsightsPage, NoHost};
use std::rc::Rc;

actions!(insights_example, [Quit]);

fn main() {
    gpui_kit::application()
        .with_assets(kass_ui::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            kass_ui::init(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            let bounds = Bounds::centered(None, size(px(1180.), px(820.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        cx.observe_window_appearance(window, |_, window, cx| {
                            kass_ui::system_appearance_changed(window.appearance(), cx);
                        })
                        .detach();
                        InsightsPage::new(ApiClient::local(None), Rc::new(NoHost), cx)
                    })
                },
            )
            .expect("open the insights window");
            cx.activate(true);
        });
}
