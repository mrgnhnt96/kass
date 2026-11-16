//! The CSS timing curves the React pill animated with, so the native one
//! moves the same way.

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> impl Fn(f32) -> f32 + Copy {
    move |t: f32| {
        let t = t.clamp(0., 1.);
        let curve = |a: f32, b: f32, s: f32| {
            let r = 1. - s;
            3. * r * r * s * a + 3. * r * s * s * b + s * s * s
        };
        let (mut lo, mut hi) = (0f32, 1f32);
        let mut s = t;
        for _ in 0..32 {
            let x = curve(x1, x2, s);
            if (x - t).abs() < 1e-5 {
                break;
            }
            if x < t {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) / 2.;
        }
        curve(y1, y2, s)
    }
}

pub fn linear(t: f32) -> f32 {
    t.clamp(0., 1.)
}

pub fn ease_in(t: f32) -> f32 {
    cubic_bezier(0.42, 0., 1., 1.)(t)
}

pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0., 0., 0.58, 1.)(t)
}

pub fn ease_in_out(t: f32) -> f32 {
    cubic_bezier(0.42, 0., 0.58, 1.)(t)
}

/// The value at `t` of keyframes `values` placed at `times` (each 0 to 1,
/// rising), with `eases[i]` between keyframes `i` and `i + 1`.
pub fn keyframes(t: f32, times: &[f32], values: &[f32], eases: &[&dyn Fn(f32) -> f32]) -> f32 {
    let at = segment(t, times);
    match at {
        Segment::Before => values[0],
        Segment::After => values[values.len() - 1],
        Segment::Within(i, progress) => {
            let eased = eases[i.min(eases.len() - 1)](progress);
            values[i] + (values[i + 1] - values[i]) * eased
        }
    }
}

pub enum Segment {
    Before,
    After,
    /// Between keyframes `.0` and `.0 + 1`, `.1` of the way.
    Within(usize, f32),
}

pub fn segment(t: f32, times: &[f32]) -> Segment {
    if t <= times[0] {
        return Segment::Before;
    }
    for i in 0..times.len() - 1 {
        if t <= times[i + 1] {
            let span = times[i + 1] - times[i];
            let progress = if span > 0. { (t - times[i]) / span } else { 1. };
            return Segment::Within(i, progress);
        }
    }
    Segment::After
}

/// How far a tween that waits `delay` and runs `duration` is at `t`, eased.
pub fn tween(t: f32, delay: f32, duration: f32, ease: impl Fn(f32) -> f32) -> f32 {
    if t <= delay {
        0.
    } else if t >= delay + duration {
        1.
    } else {
        ease((t - delay) / duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn curves_start_and_end_in_place() {
        for ease in [ease_in, ease_out, ease_in_out, linear] {
            assert!(close(ease(0.), 0.));
            assert!(close(ease(1.), 1.));
        }
        assert!(ease_in(0.5) < 0.5);
        assert!(ease_out(0.5) > 0.5);
        assert!(close(ease_in_out(0.5), 0.5));
    }

    #[test]
    fn keyframes_ease_each_segment() {
        let times = [0., 0.5, 1.];
        let values = [0.25, 1., 0.25];
        let eases: [&dyn Fn(f32) -> f32; 1] = [&linear];
        assert!(close(keyframes(0., &times, &values, &eases), 0.25));
        assert!(close(keyframes(0.25, &times, &values, &eases), 0.625));
        assert!(close(keyframes(0.5, &times, &values, &eases), 1.));
        assert!(close(keyframes(0.75, &times, &values, &eases), 0.625));
        assert!(close(keyframes(2., &times, &values, &eases), 0.25));
    }

    #[test]
    fn tweens_wait_for_their_delay() {
        assert_eq!(tween(0.5, 0.64, 0.4, linear), 0.);
        assert!(close(tween(0.84, 0.64, 0.4, linear), 0.5));
        assert_eq!(tween(2., 0.64, 0.4, linear), 1.);
    }
}
