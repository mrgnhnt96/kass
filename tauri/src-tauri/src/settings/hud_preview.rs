//! The dictation pill on a stand-in desktop, going through its states with
//! a sample voice, so you see what shows while you hold the shortcut.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_pill::{DictatePill, LevelEvent, NoHost, Phase, StateEvent, PILL_SPACE};
use kass_ui::{metrics, theme};

const TICK: Duration = Duration::from_millis(50);
/// How long the page can go undrawn before the preview stops moving.
const UNSEEN: Duration = Duration::from_millis(500);

/// The preview's states in order, and how long each shows.
const STEPS: [(Step, Duration); 4] = [
    (Step::Recording, Duration::from_millis(2600)),
    (Step::Transcribing, Duration::from_millis(1500)),
    (Step::Refining, Duration::from_millis(1500)),
    (Step::Rest, Duration::from_millis(900)),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Recording,
    Transcribing,
    Refining,
    Rest,
}

/// The sample voice `seconds` into recording: syllables, then a pause.
pub fn sample_db(seconds: f32) -> f32 {
    if seconds > 2. {
        return -64.;
    }
    -40. + 30. * (seconds * 7.3).sin().abs() * (0.55 + 0.45 * (seconds * 2.1).sin())
}

pub struct HudPreview {
    pill: Entity<DictatePill>,
    enabled: bool,
    drawn: Instant,
    _cycle: Task<()>,
}

impl HudPreview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let pill = cx.new(|_| DictatePill::new(Rc::new(NoHost)));
        let cycle = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut take = 0;
            let mut step = 0;
            let mut since = Duration::ZERO;
            loop {
                let Ok((pill, seen)) = this.read_with(cx, |this, _| {
                    (this.pill.clone(), this.drawn.elapsed() < UNSEEN)
                }) else {
                    return;
                };
                if !seen {
                    cx.background_executor().timer(UNSEEN).await;
                    continue;
                }
                let (current, length) = STEPS[step];
                let start = since.is_zero();
                let _ = pill.update(cx, |pill, cx| {
                    if start {
                        if current == Step::Recording {
                            take += 1;
                        }
                        for phase in phases(current) {
                            pill.on_state(StateEvent { take, phase }, cx);
                        }
                    }
                    if current == Step::Recording {
                        let db = sample_db(since.as_secs_f32());
                        pill.on_level(LevelEvent { take, db });
                    }
                });
                cx.background_executor().timer(TICK).await;
                since += TICK;
                if since >= length {
                    since = Duration::ZERO;
                    step = (step + 1) % STEPS.len();
                }
            }
        });
        Self {
            pill,
            enabled: true,
            drawn: Instant::now(),
            _cycle: cycle,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// What the pill is told at the start of each step.
fn phases(step: Step) -> Vec<Phase> {
    match step {
        Step::Recording => vec![Phase::Preparing, Phase::Recording],
        Step::Transcribing => vec![Phase::Transcribing { elapsed_ms: 0 }],
        Step::Refining => vec![Phase::Refining],
        Step::Rest => vec![Phase::Done],
    }
}

impl Render for HudPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = Instant::now();
        let t = theme(cx);
        div()
            .relative()
            .w(px(220.))
            .h(px(48.))
            .overflow_hidden()
            .rounded(metrics::RADIUS_LG)
            .bg(t.secondary)
            .when(!self.enabled, |preview| preview.opacity(0.5))
            .child(
                // The pill sits centred in its space; this lifts it 8px off
                // the bottom, as the React preview had it.
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(8. - (PILL_SPACE - 32.) / 2.))
                    .h(px(PILL_SPACE))
                    .child(self.pill.clone()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::sample_db;

    #[test]
    fn the_sample_voice_pauses_after_two_seconds() {
        assert_eq!(sample_db(2.5), -64.);
        let loud = (0..40)
            .map(|i| sample_db(i as f32 * 0.05))
            .fold(f32::MIN, f32::max);
        assert!(loud > -20. && loud <= -10., "{loud}");
        assert_eq!(sample_db(0.), -40.);
    }
}
