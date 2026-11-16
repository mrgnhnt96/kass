//! The dictate window's content: the pill at the bottom, and the style chip
//! above it while one plays. The window is transparent; everything outside
//! the pill and chip shows what's under it.

use crate::chip::{self, style_chip};
use crate::ease::{self, keyframes};
use crate::level::{self, Wave, bar_heights, is_clipping, level_from_db, push_level, silent_wave};
use crate::session::{Later, LevelEvent, Outcome, PillState, Session, StateEvent, StyleEvent};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::tr;
use kass_ui::{metrics, theme};
use std::rc::Rc;
use std::time::Duration;

/// What the pill asks of the app around it.
pub trait PillHost: 'static {
    /// The recording pill was clicked: finish the take.
    fn stop(&self, cx: &mut App);
    /// The pill is gone: tuck its window away.
    fn hide(&self, cx: &mut App);
    /// Grow the window up from the pill so the style chip has room to rise
    /// into, or give the room back.
    fn chip_space(&self, show: bool, cx: &mut App);
}

/// A host that does nothing, for previews.
pub struct NoHost;

impl PillHost for NoHost {
    fn stop(&self, _: &mut App) {}
    fn hide(&self, _: &mut App) {}
    fn chip_space(&self, _: bool, _: &mut App) {}
}

/// The pill's window holds this much at the bottom for it.
pub const PILL_SPACE: f32 = 64.;

/// The rest dot shows this long before the fade starts.
const REST_HOLD_S: f32 = 0.6;
const REST_FADE_S: f32 = 0.3;

/// How the capsule's opacity moves, as the React pill's did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fade {
    Shown,
    /// Resting after a take: holds, then fades out.
    Out(u64),
    /// Came straight from the recording button to rest: no fade to play.
    Gone,
    /// Back from rest.
    In(u64),
}

fn emerald_400() -> Hsla {
    rgb(0x34d399).into()
}

fn orange_400() -> Hsla {
    rgb(0xfb923c).into()
}

fn red_400() -> Hsla {
    rgb(0xf87171).into()
}

fn red_300() -> Hsla {
    rgb(0xfca5a5).into()
}

/// `ring-1 <ring>` with `shadow-lg shadow-black/40`.
fn capsule_shadow(ring: Hsla) -> Vec<BoxShadow> {
    let drop = |y: f32, blur: f32, spread: f32| BoxShadow {
        color: black().opacity(0.4),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
        inset: false,
    };
    vec![
        drop(10., 15., -3.),
        drop(4., 6., -4.),
        BoxShadow {
            color: ring,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        },
    ]
}

fn capsule() -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(3.))
        .h(px(28.))
        .min_w(px(64.))
        .px(px(10.))
        .rounded_full()
        .bg(black().opacity(0.9))
        .shadow(capsule_shadow(white().opacity(0.1)))
}

fn dot(size: f32, color: Hsla) -> Div {
    div().size(px(size)).flex_none().rounded_full().bg(color)
}

/// Three dots pulsing one after another while Kass works.
fn pulsing_dots(id: &'static str, color: Hsla) -> impl IntoElement {
    div().flex().items_center().gap(px(3.)).with_animation(
        id,
        Animation::new(Duration::from_millis(1200)).repeat(),
        move |el, delta| {
            el.children((0..3).map(|i| {
                let phase = (delta - i as f32 * 0.2 / 1.2).rem_euclid(1.);
                let opacity = keyframes(
                    phase,
                    &[0., 0.5, 1.],
                    &[0.25, 1., 0.25],
                    &[&ease::ease_in_out],
                );
                dot(4., color).opacity(opacity)
            }))
        },
    )
}

pub struct DictatePill {
    session: Session,
    host: Rc<dyn PillHost>,
    later: Option<Task<()>>,
    chip_done: Option<Task<()>>,
    wave: Wave,
    /// The bar heights each bar grows or shrinks from over a step.
    heights_from: Wave,
    step: u64,
    ticker: Option<Task<()>>,
    fade: Fade,
    fades: u64,
}

impl DictatePill {
    pub fn new(host: Rc<dyn PillHost>) -> Self {
        Self {
            session: Session::default(),
            host,
            later: None,
            chip_done: None,
            wave: silent_wave(),
            heights_from: bar_heights(&silent_wave()),
            step: 0,
            ticker: None,
            fade: Fade::Shown,
            fades: 0,
        }
    }

    pub fn state(&self) -> PillState {
        self.session.state()
    }

    pub fn on_state(&mut self, event: StateEvent, cx: &mut Context<Self>) {
        let before = self.session.state();
        let Outcome::Applied(later) = self.session.apply(&event) else {
            return;
        };
        self.later = later.map(|(delay, later)| self.run_later(delay, later, cx));
        self.changed(before, cx);
    }

    pub fn on_level(&mut self, event: LevelEvent) {
        self.session.level(event);
    }

    pub fn on_style(&mut self, event: StyleEvent, cx: &mut Context<Self>) {
        if !self.session.wants_style(&event) {
            return;
        }
        self.host.chip_space(true, cx);
        self.session.show_style(event);
        self.chip_done = Some(cx.spawn(async move |pill, cx| {
            cx.background_executor().timer(chip::DURATION).await;
            pill.update(cx, |pill, cx| pill.finish_style(cx)).ok();
        }));
        cx.notify();
    }

    /// The error was clicked: it's copied, so it can go.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        let before = self.session.state();
        self.later = None;
        self.session.hide();
        self.changed(before, cx);
    }

    fn finish_style(&mut self, cx: &mut Context<Self>) {
        self.session.finish_style();
        // The room above the pill would otherwise take clicks meant for
        // what's under it.
        self.host.chip_space(false, cx);
        cx.notify();
    }

    fn run_later(&mut self, delay: Duration, later: Later, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |pill, cx| {
            cx.background_executor().timer(delay).await;
            pill.update(cx, |pill, cx| {
                let before = pill.session.state();
                pill.session.run(later);
                pill.changed(before, cx);
            })
            .ok();
        })
    }

    fn changed(&mut self, before: PillState, cx: &mut Context<Self>) {
        let now = self.session.state();
        if self.session.style().is_none() && self.chip_done.take().is_some() {
            self.host.chip_space(false, cx);
        }
        if now == before {
            cx.notify();
            return;
        }
        let shown_fading = |state| {
            matches!(
                state,
                PillState::Preparing | PillState::Transcribing | PillState::Refining
            )
        };
        self.fade = match now {
            PillState::Rest if shown_fading(before) => {
                self.fades += 1;
                Fade::Out(self.fades)
            }
            PillState::Rest => Fade::Gone,
            _ if before == PillState::Rest && self.fade != Fade::Shown => {
                self.fades += 1;
                Fade::In(self.fades)
            }
            _ => Fade::Shown,
        };
        if now == PillState::Recording {
            self.wave = silent_wave();
            self.heights_from = bar_heights(&self.wave);
            self.start_ticker(cx);
        } else {
            self.ticker = None;
        }
        if now == PillState::Hidden {
            self.host.hide(cx);
        }
        cx.notify();
    }

    /// Move the level bars along every step while recording.
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        self.ticker = Some(cx.spawn(async move |pill, cx| {
            loop {
                cx.background_executor().timer(level::STEP).await;
                let stepped = pill.update(cx, |pill, cx| {
                    pill.heights_from = bar_heights(&pill.wave);
                    let level = level_from_db(pill.session.input_db());
                    pill.wave = push_level(&pill.wave, level);
                    pill.step += 1;
                    cx.notify();
                });
                if stepped.is_err() {
                    break;
                }
            }
        }));
    }

    fn level_bars(&self, accent: Hsla) -> impl IntoElement {
        let from = self.heights_from;
        let to = bar_heights(&self.wave);
        let color = if is_clipping(self.session.input_db()) {
            orange_400()
        } else {
            accent
        };
        div().flex().items_center().gap(px(3.)).with_animation(
            ElementId::NamedInteger("level".into(), self.step),
            Animation::new(level::STEP),
            move |el, delta| {
                el.children((0..level::COUNT).map(|i| {
                    let height = from[i] + (to[i] - from[i]) * delta;
                    div().w(px(3.)).h(px(height)).rounded_full().bg(color)
                }))
            },
        )
    }

    fn error_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let message: SharedString = match self.session.error() {
            Some(message) => message.to_string().into(),
            None => tr!("captures-pill-error-fallback").into(),
        };
        let copied = message.clone();
        div()
            .id("pill-error")
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(28.))
            .max_w(px(380.))
            .px(px(10.))
            .rounded_full()
            .bg(black().opacity(0.9))
            .hover(|style| style.bg(black()))
            .shadow(capsule_shadow(red_400().opacity(0.4)))
            .child(dot(4., red_400()))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(red_300())
                    .child(message),
            )
            .on_click(cx.listener(move |pill, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.to_string()));
                pill.dismiss(cx);
            }))
            .into_any_element()
    }

    fn pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let accent = theme(cx).accent;
        let state = self.session.state();
        let marks = match state {
            PillState::Error => return self.error_pill(cx),
            PillState::Recording => {
                let host = self.host.clone();
                return capsule()
                    .id("pill-stop")
                    .child(self.level_bars(accent))
                    .on_click(move |_, _, cx| host.stop(cx))
                    .into_any_element();
            }
            PillState::Preparing => div()
                .flex()
                .items_center()
                .gap(px(3.))
                .children((0..5).map(|_| dot(3., white().opacity(0.25))))
                .into_any_element(),
            PillState::Transcribing => {
                pulsing_dots("transcribing", white().opacity(0.7)).into_any_element()
            }
            PillState::Refining => pulsing_dots("refining", accent).into_any_element(),
            PillState::Rest | PillState::Hidden => dot(4., emerald_400()).into_any_element(),
        };
        let pill = capsule().child(marks);
        match self.fade {
            Fade::Shown => pill.into_any_element(),
            Fade::Gone => pill.opacity(0.).into_any_element(),
            Fade::Out(n) => pill
                .with_animation(
                    ElementId::NamedInteger("pill-rest".into(), n),
                    Animation::new(Duration::from_secs_f32(REST_HOLD_S + REST_FADE_S)),
                    |el, delta| {
                        let t = delta * (REST_HOLD_S + REST_FADE_S);
                        let faded = ease::tween(t, REST_HOLD_S, REST_FADE_S, ease::ease_in);
                        el.opacity(1. - faded)
                    },
                )
                .into_any_element(),
            Fade::In(n) => pill
                .with_animation(
                    ElementId::NamedInteger("pill-back".into(), n),
                    Animation::new(Duration::from_millis(150)).with_easing(ease::ease_out),
                    |el, delta| el.opacity(delta),
                )
                .into_any_element(),
        }
    }
}

impl Render for DictatePill {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().relative().font_family(metrics::FONT_SANS);
        if self.session.state() == PillState::Hidden {
            return root;
        }
        let accent = theme(cx).accent;
        let chip = self
            .session
            .style()
            .cloned()
            .map(|change| style_chip(change, accent));
        root.children(chip).child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(PILL_SPACE))
                .flex()
                .items_center()
                .justify_center()
                .px(px(12.))
                .child(self.pill(cx)),
        )
    }
}
