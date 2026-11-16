//! The Captures page on its own, against the local Kass server.
//!
//! `cargo run -p kass-captures --example captures`

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_captures::{CapturesPage, NoHost};
use std::rc::Rc;

actions!(captures_example, [Quit]);

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
                        CapturesPage::new(ApiClient::local(None), Rc::new(NoHost), window, cx)
                    })
                },
            )
            .expect("open the captures window");
            cx.activate(true);
        });
}
