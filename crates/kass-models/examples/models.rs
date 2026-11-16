//! The Models page on its own, against the local Kass server.
//!
//! `cargo run -p kass-models --example models`

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_models::ModelsPage;

actions!(models_example, [Quit]);

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
                        let mut page = ModelsPage::new(ApiClient::local(None), window, cx);
                        page.refresh(cx);
                        page
                    })
                },
            )
            .expect("open the models window");
            cx.activate(true);
        });
}
