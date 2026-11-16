//! Dictating a teach reply or a style's description instead of typing it.
//! The app that hosts the page owns dictation (starting a take from Kass's
//! own button, and the state events every take sends), so it hands the page
//! an implementation of [`StyleDictation`]. Without one, the page shows no
//! Dictate or "Say it" button, as the React page did outside the app.

use gpui_kit::{App, Subscription};
use std::rc::Rc;

/// Where the current take is, as a reply box shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TakeState {
    /// Nothing recording, or the last take was cancelled or failed.
    #[default]
    Idle,
    /// Heard: from the Dictate button, or from the dictation shortcut.
    Listening { from_button: bool },
    /// Finished speaking; the take is being transcribed and cleaned up.
    Cleaning,
    /// A take from the button is done and saved as a capture. The page
    /// fetches its text; a shortcut take types its own.
    ButtonDone,
}

/// Called with each step of every take.
pub type OnTake = Rc<dyn Fn(TakeState, &mut App)>;

pub trait StyleDictation: 'static {
    /// Starts a take from the Dictate button. Its text lands in Captures,
    /// never typed into another app.
    fn start(&self, cx: &mut App);

    /// Ends the current take.
    fn stop(&self, cx: &mut App);

    /// Follows every take until the subscription is dropped.
    fn follow(&self, on_take: OnTake, cx: &mut App) -> Subscription;
}

/// Tells the button's take from shortcut takes, given each take's state
/// events (`preparing`, `recording`, `transcribing`, `refining`, `done`,
/// `cancelled`, `error`) and its id. A host's [`StyleDictation`] keeps one.
#[derive(Debug, Default)]
pub struct TakeTracker {
    /// Set from the click until the take's id is known; its first event can
    /// come before the start call returns.
    pending: bool,
    button: Option<u64>,
}

impl TakeTracker {
    /// The button was clicked; its take's id isn't known yet.
    pub fn starting(&mut self) {
        self.pending = true;
        self.button = None;
    }

    /// The button's take started as `take`, or didn't start (`None`).
    pub fn started(&mut self, take: Option<u64>) {
        self.pending = false;
        if take.is_some() {
            self.button = take;
        }
    }

    /// What the reply box shows after `state` for `take`; `None` for a state
    /// it doesn't follow.
    pub fn event(&mut self, state: &str, take: u64) -> Option<TakeState> {
        if self.pending && self.button.is_none() && state == "preparing" {
            self.button = Some(take);
        }
        let from_button = self.button == Some(take);
        match state {
            "preparing" | "recording" => Some(TakeState::Listening { from_button }),
            "transcribing" | "refining" => Some(TakeState::Cleaning),
            "done" if from_button => {
                self.button = None;
                Some(TakeState::ButtonDone)
            }
            "done" => Some(TakeState::Idle),
            "cancelled" | "error" => {
                if from_button {
                    self.button = None;
                }
                Some(TakeState::Idle)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TakeState, TakeTracker};

    #[test]
    fn shortcut_takes_are_not_the_buttons() {
        let mut tracker = TakeTracker::default();
        assert_eq!(
            tracker.event("recording", 4),
            Some(TakeState::Listening { from_button: false })
        );
        assert_eq!(tracker.event("refining", 4), Some(TakeState::Cleaning));
        assert_eq!(tracker.event("done", 4), Some(TakeState::Idle));
    }

    #[test]
    fn the_buttons_take_ends_in_button_done() {
        let mut tracker = TakeTracker::default();
        tracker.starting();
        tracker.started(Some(7));
        assert_eq!(
            tracker.event("preparing", 7),
            Some(TakeState::Listening { from_button: true })
        );
        assert_eq!(tracker.event("done", 7), Some(TakeState::ButtonDone));
        // The next take is a shortcut take again.
        assert_eq!(
            tracker.event("preparing", 8),
            Some(TakeState::Listening { from_button: false })
        );
    }

    #[test]
    fn the_first_event_can_beat_the_start_reply() {
        let mut tracker = TakeTracker::default();
        tracker.starting();
        assert_eq!(
            tracker.event("preparing", 3),
            Some(TakeState::Listening { from_button: true })
        );
        tracker.started(Some(3));
        assert_eq!(tracker.event("done", 3), Some(TakeState::ButtonDone));
    }

    #[test]
    fn a_cancelled_button_take_is_forgotten() {
        let mut tracker = TakeTracker::default();
        tracker.starting();
        tracker.started(Some(2));
        assert_eq!(tracker.event("cancelled", 2), Some(TakeState::Idle));
        assert_eq!(
            tracker.event("recording", 2),
            Some(TakeState::Listening { from_button: false })
        );
        assert_eq!(tracker.event("unknown", 2), None);
    }
}
