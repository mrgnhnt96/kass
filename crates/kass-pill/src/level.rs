//! The recording pill's level bars: microphone loudness in dBFS turned into
//! seven bar heights that rise at once and fall back slowly.

use std::time::Duration;

/// Quieter than this draws the shortest bars.
pub const FLOOR_DB: f32 = -45.;
/// Ordinary speech reaches full height here.
pub const FULL_DB: f32 = -15.;
/// Louder than this is about to clip: the bars turn orange.
pub const CLIP_DB: f32 = -3.;

pub const BAR_MIN: f32 = 3.;
pub const BAR_MAX: f32 = 20.;
/// Taller in the middle, so the bars read as one shape.
pub const WEIGHTS: [f32; 7] = [0.6, 0.85, 1., 1., 1., 0.85, 0.6];
pub const COUNT: usize = WEIGHTS.len();

/// The most a bar falls per step, so a word's end trails off.
pub const MAX_FALL: f32 = 0.25;
/// How often the wave moves along, and how long each bar takes to get there.
pub const STEP: Duration = Duration::from_millis(50);

pub type Wave = [f32; COUNT];

/// Loudness as 0 (silence) to 1 (speech and louder).
pub fn level_from_db(db: Option<f32>) -> f32 {
    match db {
        Some(db) if !db.is_nan() => ((db - FLOOR_DB) / (FULL_DB - FLOOR_DB)).clamp(0., 1.),
        _ => 0.,
    }
}

pub fn silent_wave() -> Wave {
    [0.; COUNT]
}

/// The wave one step on: `level` enters on the left, never more than
/// [`MAX_FALL`] below the bar before it.
pub fn push_level(wave: &Wave, level: f32) -> Wave {
    let mut next = silent_wave();
    next[0] = level.max(wave[0] - MAX_FALL);
    next[1..].copy_from_slice(&wave[..COUNT - 1]);
    next
}

/// Each bar's height in points.
pub fn bar_heights(wave: &Wave) -> Wave {
    std::array::from_fn(|i| BAR_MIN + (BAR_MAX - BAR_MIN) * wave[i] * WEIGHTS[i])
}

pub fn is_clipping(db: Option<f32>) -> bool {
    db.is_some_and(|db| db > CLIP_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_maps_between_floor_and_full() {
        assert_eq!(level_from_db(None), 0.);
        assert_eq!(level_from_db(Some(f32::NAN)), 0.);
        assert_eq!(level_from_db(Some(-60.)), 0.);
        assert_eq!(level_from_db(Some(-30.)), 0.5);
        assert_eq!(level_from_db(Some(0.)), 1.);
    }

    #[test]
    fn bars_rise_at_once_and_fall_slowly() {
        let loud = push_level(&silent_wave(), 1.);
        assert_eq!(loud[0], 1.);
        let quiet = push_level(&loud, 0.);
        assert_eq!(quiet[..2], [0.75, 1.]);
        let quieter = push_level(&quiet, 0.);
        assert_eq!(quieter[..3], [0.5, 0.75, 1.]);
    }

    #[test]
    fn heights_follow_the_weights() {
        assert_eq!(bar_heights(&silent_wave()), [BAR_MIN; COUNT]);
        let full = bar_heights(&[1.; COUNT]);
        assert_eq!(full[3], BAR_MAX);
        assert_eq!(full[0], BAR_MIN + 17. * 0.6);
    }

    #[test]
    fn clipping_is_above_minus_three() {
        assert!(!is_clipping(None));
        assert!(!is_clipping(Some(-3.)));
        assert!(is_clipping(Some(-2.)));
    }
}
