//! One dictation take as the pill shows it, from the `dictation:*` events
//! the dictation code sends. Only timing is left to the caller: an applied
//! event cancels the pending [`Later`] and may ask for a new one.

use serde::Deserialize;
use std::time::Duration;

/// How long the "pasted" dot stays before the pill is gone.
pub const REST_FADE: Duration = Duration::from_millis(900);
/// Most microphones deliver sound within ~0.1 s, so the pill shows
/// recording right away; only one still silent after this (a Bluetooth
/// headset switching to call mode) says it's opening.
pub const SLOW_MICROPHONE: Duration = Duration::from_millis(300);

/// `dictation:state`: where take `take` is. Takes count up, so a take still
/// finishing can't overwrite the pill of a newer one.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct StateEvent {
    pub take: u64,
    #[serde(flatten)]
    pub phase: Phase,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Recording,
    Transcribing {
        #[serde(default)]
        elapsed_ms: u64,
    },
    Refining,
    Done,
    /// Escape: gone at once, without the resting fade.
    Cancelled,
    Error {
        #[serde(default)]
        message: String,
        #[serde(default)]
        visible_ms: u64,
    },
}

/// `dictation:level`: microphone loudness, about 20 times a second.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct LevelEvent {
    pub take: u64,
    pub db: f32,
}

/// `dictation:style`: the writing style asked for by name ("use formal mode").
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct StyleEvent {
    pub take: u64,
    pub from: Option<String>,
    pub to: String,
}

/// What the pill shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillState {
    Hidden,
    /// The microphone is still opening.
    Preparing,
    Recording,
    Transcribing,
    Refining,
    /// Delivered: a dot, fading out.
    Rest,
    Error,
}

impl PillState {
    pub fn is_recording(self) -> bool {
        matches!(self, Self::Preparing | Self::Recording)
    }
}

/// A style change for the chip above the pill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleChange {
    /// Counts up per change, so a new one replays the animation.
    pub key: u64,
    /// The style it replaced; `None` when the take was already in it.
    pub from: Option<String>,
    pub to: String,
}

/// Something to do after a delay, unless another event comes first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Later {
    /// The microphone is slow: say it's opening.
    ShowPreparing,
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// From an older take: nothing changed.
    Ignored,
    /// Cancel what was pending, then run this later, if anything.
    Applied(Option<(Duration, Later)>),
}

#[derive(Clone, Debug)]
pub struct Session {
    take: u64,
    state: PillState,
    error: Option<String>,
    db: Option<f32>,
    style: Option<StyleChange>,
    styles: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            take: 0,
            state: PillState::Hidden,
            error: None,
            db: None,
            style: None,
            styles: 0,
        }
    }
}

impl Session {
    pub fn state(&self) -> PillState {
        self.state
    }

    /// The error to show; `None` for the fallback text.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Loudness while recording; `None` before the first reading.
    pub fn input_db(&self) -> Option<f32> {
        if self.state.is_recording() {
            self.db
        } else {
            None
        }
    }

    pub fn style(&self) -> Option<&StyleChange> {
        self.style.as_ref()
    }

    pub fn apply(&mut self, event: &StateEvent) -> Outcome {
        if let Phase::Preparing = event.phase {
            if event.take < self.take {
                return Outcome::Ignored;
            }
            self.take = event.take;
        } else if event.take != self.take {
            return Outcome::Ignored;
        }
        let later = match &event.phase {
            Phase::Preparing => {
                self.error = None;
                self.db = None;
                self.style = None;
                self.state = PillState::Recording;
                Some((SLOW_MICROPHONE, Later::ShowPreparing))
            }
            Phase::Recording => {
                self.state = PillState::Recording;
                None
            }
            Phase::Transcribing { .. } => {
                self.state = PillState::Transcribing;
                None
            }
            Phase::Refining => {
                self.state = PillState::Refining;
                None
            }
            Phase::Done => {
                self.state = PillState::Rest;
                Some((REST_FADE, Later::Hide))
            }
            Phase::Cancelled => {
                self.hide();
                None
            }
            Phase::Error {
                message,
                visible_ms,
            } => {
                self.error = (!message.is_empty()).then(|| message.clone());
                self.state = PillState::Error;
                Some((Duration::from_millis(*visible_ms), Later::Hide))
            }
        };
        Outcome::Applied(later)
    }

    pub fn run(&mut self, later: Later) {
        match later {
            Later::ShowPreparing => self.state = PillState::Preparing,
            Later::Hide => self.hide(),
        }
    }

    pub fn level(&mut self, event: LevelEvent) {
        if event.take == self.take {
            self.db = Some(event.db);
        }
    }

    /// Whether a style change belongs to the current take.
    pub fn wants_style(&self, event: &StyleEvent) -> bool {
        event.take == self.take
    }

    pub fn show_style(&mut self, event: StyleEvent) -> Option<&StyleChange> {
        if !self.wants_style(&event) {
            return None;
        }
        self.styles += 1;
        self.style = Some(StyleChange {
            key: self.styles,
            from: event.from,
            to: event.to,
        });
        self.style.as_ref()
    }

    pub fn finish_style(&mut self) {
        self.style = None;
    }

    /// The error was clicked away (or anything else ends the pill now).
    pub fn hide(&mut self) {
        self.state = PillState::Hidden;
        self.error = None;
        self.style = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(json: &str) -> StateEvent {
        serde_json::from_str(json).unwrap()
    }

    fn at(take: u64, phase: Phase) -> StateEvent {
        StateEvent { take, phase }
    }

    #[test]
    fn reads_the_dictation_events() {
        assert_eq!(
            event(r#"{"take":3,"state":"preparing"}"#),
            at(3, Phase::Preparing)
        );
        assert_eq!(
            event(r#"{"take":3,"state":"transcribing","elapsed_ms":1200}"#),
            at(3, Phase::Transcribing { elapsed_ms: 1200 })
        );
        assert_eq!(
            event(r#"{"take":3,"state":"error","message":"No speech","visible_ms":1800}"#),
            at(
                3,
                Phase::Error {
                    message: "No speech".into(),
                    visible_ms: 1800
                }
            )
        );
        let style: StyleEvent =
            serde_json::from_str(r#"{"take":3,"from":null,"to":"Formal"}"#).unwrap();
        assert_eq!(style.from, None);
        let level: LevelEvent = serde_json::from_str(r#"{"take":3,"db":-21.5}"#).unwrap();
        assert_eq!(level.db, -21.5);
    }

    #[test]
    fn a_take_records_then_rests_then_hides() {
        let mut s = Session::default();
        assert_eq!(
            s.apply(&at(1, Phase::Preparing)),
            Outcome::Applied(Some((SLOW_MICROPHONE, Later::ShowPreparing)))
        );
        assert_eq!(s.state(), PillState::Recording);
        s.level(LevelEvent { take: 1, db: -20. });
        assert_eq!(s.input_db(), Some(-20.));
        s.apply(&at(1, Phase::Transcribing { elapsed_ms: 900 }));
        assert_eq!(s.state(), PillState::Transcribing);
        assert_eq!(s.input_db(), None);
        s.apply(&at(1, Phase::Refining));
        assert_eq!(
            s.apply(&at(1, Phase::Done)),
            Outcome::Applied(Some((REST_FADE, Later::Hide)))
        );
        assert_eq!(s.state(), PillState::Rest);
        s.run(Later::Hide);
        assert_eq!(s.state(), PillState::Hidden);
    }

    #[test]
    fn a_slow_microphone_says_it_is_opening() {
        let mut s = Session::default();
        s.apply(&at(1, Phase::Preparing));
        s.run(Later::ShowPreparing);
        assert_eq!(s.state(), PillState::Preparing);
        assert_eq!(s.apply(&at(1, Phase::Recording)), Outcome::Applied(None));
        assert_eq!(s.state(), PillState::Recording);
    }

    #[test]
    fn older_takes_cannot_overwrite_newer_ones() {
        let mut s = Session::default();
        s.apply(&at(2, Phase::Preparing));
        assert_eq!(s.apply(&at(1, Phase::Preparing)), Outcome::Ignored);
        assert_eq!(s.apply(&at(1, Phase::Done)), Outcome::Ignored);
        s.level(LevelEvent { take: 1, db: -1. });
        assert_eq!(s.input_db(), None);
        assert_eq!(s.state(), PillState::Recording);
    }

    #[test]
    fn errors_show_for_their_time() {
        let mut s = Session::default();
        s.apply(&at(1, Phase::Preparing));
        let message = "Kass isn't running".to_string();
        let outcome = s.apply(&at(
            1,
            Phase::Error {
                message: message.clone(),
                visible_ms: 4000,
            },
        ));
        assert_eq!(
            outcome,
            Outcome::Applied(Some((Duration::from_secs(4), Later::Hide)))
        );
        assert_eq!(s.error(), Some(message.as_str()));
        s.run(Later::Hide);
        assert_eq!((s.state(), s.error()), (PillState::Hidden, None));

        s.apply(&at(2, Phase::Preparing));
        s.apply(&at(
            2,
            Phase::Error {
                message: String::new(),
                visible_ms: 10,
            },
        ));
        assert_eq!((s.state(), s.error()), (PillState::Error, None));
    }

    #[test]
    fn cancelling_hides_at_once() {
        let mut s = Session::default();
        s.apply(&at(1, Phase::Preparing));
        assert_eq!(s.apply(&at(1, Phase::Cancelled)), Outcome::Applied(None));
        assert_eq!(s.state(), PillState::Hidden);
    }

    #[test]
    fn style_changes_belong_to_their_take() {
        let mut s = Session::default();
        s.apply(&at(1, Phase::Preparing));
        let stale = StyleEvent {
            take: 0,
            from: None,
            to: "Casual".into(),
        };
        assert!(s.show_style(stale).is_none());
        let first = s
            .show_style(StyleEvent {
                take: 1,
                from: Some("Casual".into()),
                to: "Formal".into(),
            })
            .cloned()
            .unwrap();
        assert_eq!(first.key, 1);
        let second = s
            .show_style(StyleEvent {
                take: 1,
                from: None,
                to: "Formal".into(),
            })
            .cloned()
            .unwrap();
        assert_eq!(second.key, 2);
        s.apply(&at(2, Phase::Preparing));
        assert!(s.style().is_none());
    }
}
