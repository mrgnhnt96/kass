//! The chip above the pill when the user asks for a writing style by name
//! ("use formal mode"): it rises in on the style it replaced, rolls to the
//! new one, glows once as it lands and drifts away.

use crate::ease::{self, Segment, cubic_bezier, keyframes, tween};
use crate::session::StyleChange;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::time::Duration;

/// One pass, in seconds: rise in, roll the name, glow as it lands, drift out.
const TOTAL_S: f32 = 2.72;
const ENTER_S: f32 = 0.32;
const ROLL_S: [f32; 2] = [0.64, 1.04];
/// The style cue plays at the glow's start (`STYLE_CUE_DELAY_MS` in sound_cues.rs).
const GLOW_S: [f32; 3] = [0.92, 1.08, 1.76];
const EXIT_S: f32 = 2.32;
const ROW: f32 = 14.;

/// How long the chip plays, after which it's done.
pub const DURATION: Duration = Duration::from_millis((TOTAL_S * 1000.) as u64);

/// The chip's bottom edge, above the window's bottom edge.
const BOTTOM: f32 = 54.;

/// The chip for `change`, positioned over the window's bottom 64 points,
/// playing from when it's first drawn.
pub fn style_chip(change: StyleChange, accent: Hsla) -> impl IntoElement {
    let id = ElementId::NamedInteger("style-chip".into(), change.key);
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(BOTTOM))
        .flex()
        .justify_center()
        .with_animation(
            id,
            Animation::new(Duration::from_secs_f32(TOTAL_S)),
            move |el, delta| el.child(chip_at(delta * TOTAL_S, &change, accent)),
        )
}

fn rgba(color: Hsla) -> Rgba {
    color.into()
}

fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (rgba(a), rgba(b));
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
    .into()
}

fn ring(color: Hsla, spread: f32) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(spread),
        inset: false,
    }
}

/// `0 0 0 1px <edge>, 0 0 0 <spread>px <halo>, 0 6px 12px -4px rgba(0,0,0,.45)`.
#[derive(Clone, Copy)]
struct Glow {
    edge: Hsla,
    spread: f32,
    halo: Hsla,
}

fn glow_at(t: f32, accent: Hsla) -> Vec<BoxShadow> {
    let white = white().opacity(0.1);
    let frames = [
        Glow {
            edge: white,
            spread: 0.,
            halo: accent.opacity(0.),
        },
        Glow {
            edge: white,
            spread: 0.,
            halo: accent.opacity(0.),
        },
        Glow {
            edge: accent.opacity(0.9),
            spread: 4.,
            halo: accent.opacity(0.25),
        },
        Glow {
            edge: white,
            spread: 8.,
            halo: accent.opacity(0.),
        },
    ];
    let end = GLOW_S[2];
    let times = [0., GLOW_S[0] / end, GLOW_S[1] / end, 1.];
    let glow = match ease::segment((t / end).min(1.), &times) {
        Segment::Before => frames[0],
        Segment::After => frames[3],
        Segment::Within(i, progress) => {
            let p = ease::ease_out(progress);
            let (a, b) = (&frames[i], &frames[i + 1]);
            Glow {
                edge: mix(a.edge, b.edge, p),
                spread: a.spread + (b.spread - a.spread) * p,
                halo: mix(a.halo, b.halo, p),
            }
        }
    };
    vec![
        BoxShadow {
            color: black().opacity(0.45),
            offset: point(px(0.), px(6.)),
            blur_radius: px(12.),
            spread_radius: px(-4.),
            inset: false,
        },
        ring(glow.halo, glow.spread),
        ring(glow.edge, 1.),
    ]
}

/// The chip `t` seconds in.
fn chip_at(t: f32, change: &StyleChange, accent: Hsla) -> Div {
    let at = |s: f32| s / TOTAL_S;
    let times = [0., at(ENTER_S), at(EXIT_S), 1.];
    let progress = t / TOTAL_S;
    let opacity = keyframes(
        progress,
        &times,
        &[0., 1., 1., 0.],
        &[&ease::ease_out, &ease::linear, &ease::ease_in],
    );
    let rise = cubic_bezier(0.2, 0.8, 0.2, 1.);
    let y = keyframes(
        progress,
        &times,
        &[8., 0., 0., -4.],
        &[&rise, &ease::linear, &ease::ease_in],
    );
    let rolls = change.from.is_some();
    let landed = tween(t, ROLL_S[0], ROLL_S[1] - ROLL_S[0] - 0.16, ease::ease_out);
    let dot = if rolls {
        mix(white().opacity(0.35), accent, landed)
    } else {
        accent
    };
    let name = match &change.from {
        Some(from) => {
            let roll = tween(
                t,
                ROLL_S[0],
                ROLL_S[1] - ROLL_S[0],
                cubic_bezier(0.5, 0., 0.1, 1.),
            );
            let fade = tween(t, ROLL_S[0], 0.32, ease::ease_out);
            div().h(px(ROW)).overflow_hidden().child(
                div()
                    .relative()
                    .top(px(-ROW * roll))
                    .flex()
                    .flex_col()
                    .line_height(px(ROW))
                    .child(div().opacity(0.55 * (1. - fade)).child(from.clone()))
                    .child(div().child(change.to.clone())),
            )
        }
        None => div().overflow_hidden().child(change.to.clone()),
    };
    div()
        .relative()
        .top(px(y))
        .opacity(opacity)
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(22.))
        .px(px(10.))
        .whitespace_nowrap()
        .rounded_full()
        .bg(black().opacity(0.9))
        .shadow(glow_at(t, accent))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(white().opacity(0.9))
        .child(div().size(px(5.)).flex_none().rounded_full().bg(dot))
        .child(name)
}
