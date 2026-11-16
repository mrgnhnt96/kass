//! The dictate pill in each state, side by side, for checking its look.
//!
//! `cargo run -p kass-pill --example pill`

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_pill::{DictatePill, LevelEvent, NoHost, PILL_SPACE, Phase, StateEvent, StyleEvent};
use std::rc::Rc;
use std::time::Duration;

actions!(pill_example, [Quit]);

fn pill(phase: Phase, cx: &mut App) -> Entity<DictatePill> {
    cx.new(|cx| {
        let mut pill = DictatePill::new(Rc::new(NoHost));
        // A take starts with `preparing`; later states follow for its number.
        for phase in [Phase::Preparing, phase] {
            pill.on_state(StateEvent { take: 1, phase }, cx);
        }
        pill
    })
}

struct Gallery {
    pills: Vec<(&'static str, Entity<DictatePill>)>,
    _levels: Task<()>,
}

impl Gallery {
    fn new(cx: &mut Context<Self>) -> Self {
        let recording = pill(Phase::Recording, cx);
        let styled = pill(Phase::Recording, cx);
        styled.update(cx, |pill, cx| {
            pill.on_style(
                StyleEvent {
                    take: 1,
                    from: Some("Casual".into()),
                    to: "Formal".into(),
                },
                cx,
            )
        });
        let pills = vec![
            ("preparing", pill(Phase::Preparing, cx)),
            ("recording", recording.clone()),
            ("style", styled.clone()),
            ("transcribing", pill(Phase::Transcribing { elapsed_ms: 0 }, cx)),
            (
                "error",
                pill(
                    Phase::Error {
                        message: "The server didn't answer. Copy the text from Captures.".into(),
                        visible_ms: 600_000,
                    },
                    cx,
                ),
            ),
        ];
        // A voice-like level so the bars move.
        let levels = cx.spawn(async move |_, cx| {
            let mut t = 0f32;
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                t += 0.05;
                let db = -32. + 18. * (t * 3.1).sin().abs() + 6. * (t * 11.).sin();
                for pill in [&recording, &styled] {
                    pill.update(cx, |pill, _| pill.on_level(LevelEvent { take: 1, db }));
                }
            }
        });
        Self {
            pills,
            _levels: levels,
        }
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .p(px(16.))
            .bg(rgb(0x3a3f4b))
            .children(self.pills.iter().map(|(name, pill)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(white().opacity(0.6))
                    .child(*name)
                    .child(div().w(px(420.)).h(px(PILL_SPACE + 40.)).child(pill.clone()))
            }))
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(kass_ui::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            kass_ui::init(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            let bounds = Bounds::centered(None, size(px(900.), px(420.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                |_, cx| cx.new(Gallery::new),
            )
            .expect("open the pill gallery");
            cx.activate(true);
        });
}
