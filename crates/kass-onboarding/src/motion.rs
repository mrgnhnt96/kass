//! Onboarding's motion: pieces that rise in, words struck through, letters
//! that pop up one by one, text typed out, confetti and the color that
//! spreads from the last click. Everything moves by offset, opacity or
//! paint only, so once it settles nothing has changed size or place.
//!
//! Motion is worked out from the time each piece started, every frame; a
//! [`Frame`] notes whether anything is still moving so the page asks for
//! the next frame only then. With Reduce Motion on, everything shows
//! settled at once.

use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::time::{Duration, Instant};

/// How far a piece rises (or falls, going back) as it comes in.
const RISE_PX: f32 = 22.;
const RISE_MS: f32 = 620.;
/// Between one piece of a screen and the next.
const STAGGER_MS: f32 = 70.;
const STRIKE_MS: f32 = 520.;
const POP_MS: f32 = 560.;
const WIPE_MS: f32 = 700.;
/// Spoken words type out this fast, and never take longer than the cap.
const TYPE_MS_PER_CHAR: f32 = 16.;
const TYPE_MAX_MS: f32 = 1000.;

/// One render's clock.
pub struct Frame {
    now: Instant,
    reduce: bool,
    moving: Cell<bool>,
}

impl Frame {
    pub fn new(reduce: bool) -> Self {
        Self {
            now: Instant::now(),
            reduce,
            moving: Cell::new(false),
        }
    }

    /// Whether something is still moving: the page asks for another frame.
    pub fn moving(&self) -> bool {
        self.moving.get()
    }

    /// Milliseconds since `start`.
    pub fn since(&self, start: Instant) -> f32 {
        self.now.saturating_duration_since(start).as_secs_f32() * 1000.
    }

    /// From 0 to 1 over `duration` ms, `delay` ms after `start`.
    pub fn progress(&self, start: Instant, delay: f32, duration: f32) -> f32 {
        if self.reduce {
            return 1.;
        }
        let t = ((self.since(start) - delay) / duration).clamp(0., 1.);
        if t < 1. {
            self.moving.set(true);
        }
        t
    }

    /// Whether `delay` ms have passed since `start`, waking the page then.
    pub fn after(&self, start: Instant, delay: f32) -> bool {
        self.progress(start, delay, 1.) >= 1.
    }

    /// Keeps frames coming, for something that loops while it shows.
    pub fn keep_moving(&self) {
        if !self.reduce {
            self.moving.set(true);
        }
    }

    /// Milliseconds since `start` for looping motion (always 0 with Reduce Motion).
    pub fn looping(&self, start: Instant) -> f32 {
        if self.reduce {
            return 0.;
        }
        self.moving.set(true);
        self.since(start)
    }
}

/// CSS's `cubic-bezier(x1, y1, x2, y2)` at `t`.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let curve = |a: f32, b: f32, s: f32| {
        let u = 1. - s;
        3. * u * u * s * a + 3. * u * s * s * b + s * s * s
    };
    // Find s where x(s) = t by bisection: the curves here are monotonic in x.
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.;
        if curve(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    curve(y1, y2, (lo + hi) / 2.)
}

/// The rise's spring-like curve (it overshoots a little).
fn rise_curve(t: f32) -> f32 {
    cubic_bezier(0.2, 0.9, 0.3, 1.15, t)
}

/// The color wipe's in-out curve.
fn wipe_curve(t: f32) -> f32 {
    cubic_bezier(0.65, 0., 0.35, 1., t)
}

/// The `index`th piece of a screen that opened at `start`, rising in (or
/// falling in, going `back`).
pub fn rise<E: Styled + IntoElement>(
    el: E,
    frame: &Frame,
    start: Instant,
    index: usize,
    back: bool,
) -> E {
    let delay = STAGGER_MS * index.min(6) as f32;
    rise_late(el, frame, start, delay, back)
}

/// A piece rising in `delay` ms after `start`.
pub fn rise_late<E: Styled + IntoElement>(
    el: E,
    frame: &Frame,
    start: Instant,
    delay: f32,
    back: bool,
) -> E {
    let t = frame.progress(start, delay, RISE_MS);
    let eased = rise_curve(t);
    let offset = RISE_PX * (1. - eased) * if back { -1. } else { 1. };
    el.relative().top(px(offset)).opacity(t.min(1.).powf(0.6))
}

/// A piece popping in `delay` ms after `start`: it fades up from a little
/// below. (The web version also grew it from 82%; GPUI can't scale a div.)
pub fn pop<E: Styled + IntoElement>(el: E, frame: &Frame, start: Instant, delay: f32) -> E {
    let t = frame.progress(start, delay, POP_MS);
    let eased = cubic_bezier(0.2, 1.5, 0.4, 1., t);
    el.relative().top(px(8. * (1. - eased))).opacity(t.min(1.))
}

/// How long `chars` characters take to type out.
pub fn typing_ms(chars: usize) -> f32 {
    (chars as f32 * TYPE_MS_PER_CHAR).min(TYPE_MAX_MS)
}

/// How many of `chars` characters are typed `delay` ms after `start`.
pub fn typed(frame: &Frame, start: Instant, delay: f32, chars: usize) -> usize {
    if chars == 0 {
        return 0;
    }
    let t = frame.progress(start, delay, typing_ms(chars).max(1.));
    ((t * chars as f32).ceil() as usize).min(chars)
}

/// A word struck through `delay` ms after `start`: a line draws across it,
/// then the word fades back.
pub fn strike(
    word: impl Into<SharedString>,
    color: Hsla,
    thickness: Pixels,
    frame: &Frame,
    start: Instant,
    delay: f32,
) -> Div {
    let t = frame.progress(start, delay, STRIKE_MS);
    let drawn = cubic_bezier(0.6, 0., 0.3, 1., (t / 0.6).min(1.));
    let fade = if t <= 0.6 {
        1.
    } else {
        1. - 0.55 * (t - 0.6) / 0.4
    };
    div()
        .relative()
        .flex_none()
        .opacity(fade)
        .child(word.into())
        .child(
            div()
                .absolute()
                .left_0()
                .top(relative(0.55))
                .h(thickness)
                .w(relative(drawn))
                .bg(color),
        )
}

/// Text as a wrapping row of words, so each word can move on its own.
pub fn words_row() -> Div {
    div().flex().flex_wrap().items_baseline()
}

/// A word, split so `shown` of its characters show and the rest keep their room.
pub fn typed_word(word: &str, shown: usize) -> Div {
    let cut = word
        .char_indices()
        .nth(shown)
        .map(|(i, _)| i)
        .unwrap_or(word.len());
    let (seen, rest) = word.split_at(cut);
    let mut el = div().flex().flex_none();
    if !seen.is_empty() {
        el = el.child(seen.to_string());
    }
    if !rest.is_empty() {
        el = el.child(div().opacity(0.).child(rest.to_string()));
    }
    el
}

/// `text`'s letters rising into place one after another, `delay` ms after
/// `start`, like it's being said.
pub fn rise_letters(text: &str, frame: &Frame, start: Instant, delay: f32, size: Pixels) -> Div {
    let mut row = div().flex().flex_wrap();
    let mut index = 0usize;
    let words: Vec<&str> = text.split(' ').collect();
    for (w, word) in words.iter().enumerate() {
        let mut letters = div().flex().flex_none();
        for letter in word.chars() {
            let t = frame.progress(start, delay + index as f32 * 30., 420.);
            let eased = cubic_bezier(0.2, 1.2, 0.4, 1., t);
            letters = letters.child(
                div()
                    .relative()
                    .top(size * 0.35 * (1. - eased))
                    .opacity(t)
                    .child(letter.to_string()),
            );
            index += 1;
        }
        row = row.child(letters);
        if w + 1 < words.len() {
            row = row.child(div().flex_none().w(size * 0.26));
        }
    }
    row
}

/// The window's color: `to` spreads in a circle from `origin` over `from`.
pub fn color_wipe(
    frame: &Frame,
    from: Hsla,
    to: Hsla,
    start: Instant,
    origin: Point<Pixels>,
) -> impl IntoElement {
    let t = wipe_curve(frame.progress(start, 0., WIPE_MS));
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, from));
            if t >= 1. {
                window.paint_quad(fill(bounds, to));
                return;
            }
            let r = px(1400. * t);
            let circle = Bounds::new(point(origin.x - r, origin.y - r), size(r * 2., r * 2.));
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                window.paint_quad(fill(circle, to).corner_radii(r));
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

const CONFETTI_COLORS: [u32; 6] = [0xFFFFFF, 0xF6C343, 0xF2542D, 0x7BE39A, 0x8FA2FF, 0xFF9AC8];

/// Where a burst of confetti comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Burst {
    /// From both bottom corners, for the end.
    Celebrate,
    /// From a point, as fractions of the window.
    Small(f32, f32),
}

#[derive(Clone, Copy)]
struct Piece {
    origin: (f32, f32),
    dx: f32,
    dy: f32,
    fall: f32,
    spin: f32,
    color: u32,
    w: f32,
    h: f32,
    round: bool,
    duration: f32,
}

/// A small fixed-seed random source, so a burst's pieces stay put between
/// frames without being stored.
struct Rand(u64);

impl Rand {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % 10_000) as f32 / 10_000.
    }
}

fn pieces(burst: Burst, seed: u64) -> Vec<Piece> {
    let mut rand = Rand(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let celebrate = burst == Burst::Celebrate;
    let count = if celebrate { 110 } else { 36 };
    (0..count)
        .map(|i| {
            let from_left = i % 2 == 0;
            let origin = match burst {
                Burst::Celebrate => (if from_left { 0.04 } else { 0.96 }, 1.02),
                Burst::Small(x, y) => (x, y),
            };
            let angle = if celebrate {
                (if from_left { -60. } else { -120. }) + (rand.next() - 0.5) * 50.
            } else {
                rand.next() * 360.
            };
            let speed = if celebrate {
                520. + rand.next() * 420.
            } else {
                120. + rand.next() * 160.
            };
            let rad = angle.to_radians();
            let long = rand.next() > 0.5;
            Piece {
                origin,
                dx: rad.cos() * speed,
                dy: rad.sin() * speed,
                fall: 260. + rand.next() * 260.,
                spin: (rand.next() - 0.5) * 900.,
                color: CONFETTI_COLORS[i % CONFETTI_COLORS.len()],
                w: 6. + rand.next() * 6.,
                h: if long {
                    12. + rand.next() * 6.
                } else {
                    6. + rand.next() * 4.
                },
                round: rand.next() > 0.7,
                duration: (if celebrate { 2.2 } else { 1.3 }) + rand.next() * 0.8,
            }
        })
        .collect()
}

/// Keyframes at 0, 0.35 and 1 of the way, eased out between them.
fn keyframes(a: f32, b: f32, c: f32, t: f32) -> f32 {
    let ease = |s: f32| 1. - (1. - s) * (1. - s);
    if t < 0.35 {
        a + (b - a) * ease(t / 0.35)
    } else {
        b + (c - b) * ease((t - 0.35) / 0.65)
    }
}

/// A burst of confetti over the window, `delay` ms after `start`. Nothing
/// is drawn once it's over.
pub fn confetti(frame: &Frame, burst: Burst, start: Instant, delay: f32) -> Option<AnyElement> {
    if frame.reduce {
        return None;
    }
    let elapsed = (frame.since(start) - delay) / 1000.;
    if elapsed > 3.4 {
        return None;
    }
    frame.keep_moving();
    if elapsed < 0. {
        return None;
    }
    // The same pieces every frame: seeded from the burst, not the clock.
    let seed = match burst {
        Burst::Celebrate => 7,
        Burst::Small(x, y) => (x * 1000.) as u64 * 31 + (y * 1000.) as u64,
    };
    let list = pieces(burst, seed);
    Some(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                for p in &list {
                    let t = (elapsed / p.duration).clamp(0., 1.);
                    if t >= 1. {
                        continue;
                    }
                    let x = keyframes(0., p.dx, p.dx * 1.15, t);
                    let y = keyframes(0., p.dy, p.dy + p.fall, t);
                    let turn = keyframes(0., p.spin / 2., p.spin, t).to_radians();
                    let opacity = if t < 0.35 { 1. } else { 1. - (t - 0.35) / 0.65 };
                    // Tumbling: the piece's width shrinks and grows as it turns.
                    let w = (p.w * turn.cos().abs() + p.h * turn.sin().abs() * 0.3).max(2.);
                    let h = p.h;
                    let at = point(
                        bounds.origin.x + bounds.size.width * p.origin.0 + px(x),
                        bounds.origin.y + bounds.size.height * p.origin.1 + px(y),
                    );
                    let color = rgb(p.color);
                    let color = Hsla::from(color).opacity(opacity);
                    let radius = if p.round { px(w.min(h) / 2.) } else { px(2.) };
                    window.paint_quad(
                        fill(Bounds::new(at, size(px(w), px(h))), color).corner_radii(radius),
                    );
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element(),
    )
}

/// How long until `start + delay` ms, for timers that change what shows.
pub fn until(start: Instant, delay_ms: u64) -> Duration {
    (start + Duration::from_millis(delay_ms)).saturating_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use super::{Burst, cubic_bezier, pieces, typing_ms};

    #[test]
    fn bezier_ends_where_it_should() {
        assert!(cubic_bezier(0.2, 0.9, 0.3, 1.15, 0.).abs() < 0.001);
        assert!((cubic_bezier(0.2, 0.9, 0.3, 1.15, 1.) - 1.).abs() < 0.001);
        // It overshoots before settling.
        assert!(cubic_bezier(0.2, 0.9, 0.3, 1.15, 0.7) > 1.);
    }

    #[test]
    fn typing_is_capped() {
        assert_eq!(typing_ms(10), 160.);
        assert_eq!(typing_ms(500), 1000.);
    }

    #[test]
    fn confetti_is_the_same_each_frame() {
        let a = pieces(Burst::Small(0.2, 0.4), 3);
        let b = pieces(Burst::Small(0.2, 0.4), 3);
        assert_eq!(a.len(), 36);
        assert_eq!(a[5].dx, b[5].dx);
        assert_eq!(pieces(Burst::Celebrate, 7).len(), 110);
    }
}
