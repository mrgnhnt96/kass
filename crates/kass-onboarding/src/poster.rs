//! The building blocks of onboarding's posters: every step fills the window
//! with its own color, and text and buttons take theirs from [`Poster`].

use crate::flow::INK;
use crate::motion::{self, Frame};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::keys::{key_label, key_side};
use kass_ui::metrics;
use std::time::Instant;

pub type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A step's two colors.
#[derive(Clone, Copy)]
pub struct Poster {
    pub bg: Hsla,
    pub fg: Hsla,
}

pub const GREEN: u32 = 0x34C759;
pub const FAIL: u32 = 0xFF9A85;
pub const BRIGHT_GREEN: u32 = 0x7BE39A;

/// Headlines' font: the system's, heavy.
pub fn display() -> Div {
    div()
        .font_family(metrics::FONT_SANS)
        .font_weight(FontWeight::BOLD)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HeadlineSize {
    Md,
    Lg,
    Xl,
}

pub fn headline_size(size: HeadlineSize) -> Pixels {
    match size {
        HeadlineSize::Md => px(34.),
        HeadlineSize::Lg => px(44.),
        HeadlineSize::Xl => px(84.),
    }
}

pub fn headline(text: impl Into<SharedString>, size: HeadlineSize) -> Div {
    let size = headline_size(size);
    display()
        .max_w(px(720.))
        .text_size(size)
        .line_height(size * 1.04)
        .child(text.into())
}

pub fn lead(text: impl Into<SharedString>) -> Div {
    div()
        .max_w(px(540.))
        .text_size(px(15.))
        .line_height(px(24.))
        .opacity(0.8)
        .child(text.into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Outline,
    Ghost,
    Success,
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Kind,
    poster: Poster,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(46.))
        .rounded_full()
        .px(px(22.))
        .text_size(px(15.))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .child(label.into())
        .on_click(on_click);
    match kind {
        Kind::Primary => base
            .bg(poster.fg)
            .text_color(poster.bg)
            .hover(|s| s.opacity(0.9)),
        Kind::Outline => base
            .border(px(1.5))
            .border_color(poster.fg)
            .text_color(poster.fg)
            .hover(|s| s.bg(poster.fg.opacity(0.1))),
        Kind::Ghost => base
            .px(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(poster.fg)
            .opacity(0.75)
            .hover(|s| s.opacity(1.)),
        Kind::Success => base
            .bg(rgb(GREEN))
            .text_color(rgb(0x0B2A14))
            .hover(|s| s.opacity(0.9)),
    }
}

/// A button laid out but not shown, so the row keeps its size.
pub fn hidden(el: Stateful<Div>) -> Div {
    div().opacity(0.).child(el)
}

/// Room for one short status, kept whether or not there's anything to say,
/// so nothing below it moves.
pub fn status_line(text: Option<SharedString>) -> Div {
    div()
        .min_h(px(40.))
        .max_h(px(40.))
        .max_w(px(600.))
        .overflow_hidden()
        .text_size(px(13.))
        .line_height(px(20.))
        .opacity(0.85)
        .children(text)
}

pub fn actions() -> Div {
    div().mt_2().flex().flex_wrap().items_center().gap(px(10.))
}

pub fn icon(name: IconName, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(name.path())
        .size(size)
        .flex_none()
        .text_color(color)
}

/// A green card that says a step worked; it pops in at `start`.
pub fn success_card(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    frame: &Frame,
    start: Instant,
) -> Div {
    let green = Hsla::from(rgb(GREEN));
    let check = motion::pop(
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded_full()
            .bg(green)
            .child(icon(IconName::Check, px(18.), white())),
        frame,
        start,
        120.,
    );
    motion::pop(
        div()
            .flex()
            .items_center()
            .gap_3()
            .max_w(px(600.))
            .rounded(px(16.))
            .border(px(1.5))
            .border_color(green)
            .bg(green.opacity(0.15))
            .px_4()
            .py(px(14.))
            .child(check)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title.into()))
                    .child(div().text_size(px(13.)).opacity(0.8).child(detail.into())),
            ),
        frame,
        start,
        0.,
    )
}

/// A soft panel on the step's color, for notes and progress.
pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .max_w(px(600.))
        .rounded(px(16.))
        .bg(white().opacity(0.12))
        .px(px(18.))
        .py_4()
}

pub fn progress_bar(percent: f64, fail: bool, poster: Poster) -> Div {
    let fraction = (percent / 100.).clamp(0., 1.) as f32;
    div()
        .w_full()
        .h(px(6.))
        .rounded_full()
        .bg(poster.fg.opacity(0.22))
        .child(
            div()
                .h(px(6.))
                .rounded_full()
                .w(relative(fraction))
                .bg(if fail { rgb(FAIL).into() } else { poster.fg }),
        )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CapSize {
    Sm,
    Md,
    Xl,
}

/// The user's chord as keycaps. `down` sinks them, the way they move while
/// the real keys are held; a ring spreads from them meanwhile.
pub fn keycaps(
    keys: &[String],
    down: bool,
    size: CapSize,
    success: bool,
    poster: Poster,
    frame: &Frame,
    since: Instant,
) -> Div {
    let edge: Hsla = rgb(if success { GREEN } else { INK }).into();
    let (height, radius, text, lift, sink, gap) = match size {
        CapSize::Xl => (px(96.), px(20.), px(40.), px(10.), px(8.), px(14.)),
        CapSize::Md => (px(52.), px(12.), px(22.), px(4.), px(2.), px(6.)),
        CapSize::Sm => (px(32.), px(8.), px(14.), px(4.), px(2.), px(6.)),
    };
    let ring = if success { edge } else { poster.fg };
    let ripple = down.then(|| frame.looping(since) % 900. / 900.);
    div()
        .flex()
        .flex_none()
        .items_end()
        .gap(gap)
        // Room for the cap's shadow, so a pressed cap moves within it.
        .pb(lift)
        .children(keys.iter().map(|key| {
            let shadow = if down { sink.min(px(2.)) } else { lift };
            div()
                .relative()
                .top(if down { lift - shadow } else { px(0.) })
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .h(height)
                .min_w(height)
                .px(height * 0.22)
                .rounded(radius)
                .border_2()
                .border_color(edge)
                .bg(if down { rgb(0xFFE9A8) } else { rgb(0xFFFFFF) })
                .shadow(vec![BoxShadow {
                    color: edge,
                    offset: point(px(0.), shadow),
                    blur_radius: px(0.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .font_family(metrics::FONT_MONO)
                .font_weight(FontWeight::MEDIUM)
                .text_size(text)
                .text_color(rgb(INK))
                .child(key_label(key))
                .when_some(ripple, |cap, t| {
                    let grow = px(3.) + height * 0.22 * t;
                    cap.child(
                        div()
                            .absolute()
                            .top(-grow)
                            .left(-grow)
                            .right(-grow)
                            .bottom(-grow)
                            .rounded(radius + grow)
                            .border_2()
                            .border_color(ring)
                            .opacity(0.7 * (1. - t)),
                    )
                })
                .when_some(key_side(key), |cap, side| {
                    cap.child(
                        div()
                            .absolute()
                            .top(px(-8.))
                            .right(px(-8.))
                            .rounded(px(4.))
                            .bg(rgb(INK))
                            .px_1()
                            .font_family(metrics::FONT_MONO)
                            .text_size(px(9.))
                            .line_height(px(11.))
                            .text_color(white())
                            .child(side),
                    )
                })
        }))
}

/// Bars that dance while listening; always laid out, so the row keeps its size.
pub fn listening_bars(visible: bool, poster: Poster, frame: &Frame, since: Instant) -> Div {
    let time = if visible { frame.looping(since) } else { 0. };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(3.))
        .h(px(28.))
        .opacity(if visible { 1. } else { 0. })
        .children((0..8).map(|i| {
            let base = 10. + ((i * 7) % 18) as f32;
            let period = 620. + ((i * 130) % 400) as f32;
            let phase = ((time - i as f32 * 110.).max(0.) / period) % 2.;
            let swing = if phase < 1. { phase } else { 2. - phase };
            let scale = 0.35 + 0.65 * (0.5 - 0.5 * (swing * std::f32::consts::PI).cos());
            div()
                .w(px(4.))
                .h(px(base * if visible { scale } else { 1. }))
                .rounded(px(2.))
                .bg(poster.fg)
        }))
}

/// A Dictate button next to the user's keys, wherever a step asks them to talk.
#[allow(clippy::too_many_arguments)]
pub fn speak_row(
    id: impl Into<ElementId>,
    keys: &[String],
    listening: bool,
    disabled: bool,
    poster: Poster,
    frame: &Frame,
    since: Instant,
    on_dictate: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let label = if listening {
        kass_i18n::t("onboarding-stop")
    } else {
        kass_i18n::t("onboarding-dictate")
    };
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_2()
        .h(px(44.))
        .min_w(px(124.))
        .rounded_full()
        .bg(poster.fg)
        .text_color(poster.bg)
        .px(px(18.))
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(icon(IconName::Mic, px(16.), poster.bg))
        .child(label)
        .when(!disabled, |b| {
            b.cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .on_click(on_dictate)
        });
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .when(disabled, |row| row.opacity(0.45))
        .child(button)
        .child(
            div()
                .text_size(px(13.))
                .opacity(0.8)
                .child(kass_i18n::t("onboarding-or-hold")),
        )
        .child(keycaps(
            keys,
            listening,
            CapSize::Sm,
            false,
            poster,
            frame,
            since,
        ))
        .child(listening_bars(listening, poster, frame, since))
}

/// A one-line text in the poster's colors for the corner and small notes.
pub fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(metrics::FONT_MONO)
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(text.into())
}
