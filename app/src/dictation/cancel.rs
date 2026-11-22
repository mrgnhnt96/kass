//! Escape cancels a take: from key-down until its text starts going in.
//!
//! Each take carries a [`CancelSwitch`]. Escape flips every open one; the
//! take's stream sends `cancel` so the server saves nothing, and settling
//! checks the switch before it inserts anything. Once insertion has claimed
//! the switch, Escape leaves the take alone, so text is never half-inserted.
//!
//! [`Takes`] holds the take recording now plus every take still open, so the
//! rules (Escape stops the microphone, a chord released afterwards finds
//! nothing to stop) are testable without a microphone or an app.

use std::sync::Arc;

use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    Cancelled,
    /// Text is going in: too late to cancel.
    Delivering,
}

#[derive(Debug)]
pub struct CancelSwitch {
    phase: watch::Sender<Phase>,
}

impl Default for CancelSwitch {
    fn default() -> Self {
        Self {
            phase: watch::Sender::new(Phase::Open),
        }
    }
}

impl CancelSwitch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Cancel the take unless its text has started going in. Returns whether
    /// this call cancelled it.
    pub fn cancel(&self) -> bool {
        self.phase.send_if_modified(|phase| {
            let open = *phase == Phase::Open;
            if open {
                *phase = Phase::Cancelled;
            }
            open
        })
    }

    /// Claim the take for insertion. `false` when it was cancelled: insert
    /// nothing.
    pub fn begin_delivery(&self) -> bool {
        let mut allowed = true;
        self.phase.send_if_modified(|phase| match *phase {
            Phase::Open => {
                *phase = Phase::Delivering;
                true
            }
            Phase::Delivering => false,
            Phase::Cancelled => {
                allowed = false;
                false
            }
        });
        allowed
    }

    pub fn is_cancelled(&self) -> bool {
        *self.phase.borrow() == Phase::Cancelled
    }

    /// Resolves once the take is cancelled; never, otherwise.
    pub async fn cancelled(&self) {
        let mut phase = self.phase.subscribe();
        if phase.wait_for(|p| *p == Phase::Cancelled).await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// The take recording now, and every take Escape can still cancel.
#[derive(Debug)]
pub struct Takes<R> {
    recording: Option<(u64, R)>,
    open: Vec<(u64, Arc<CancelSwitch>)>,
}

impl<R> Default for Takes<R> {
    fn default() -> Self {
        Self {
            recording: None,
            open: Vec::new(),
        }
    }
}

impl<R> Takes<R> {
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn recording(&self) -> Option<&R> {
        self.recording.as_ref().map(|(_, r)| r)
    }

    /// A take started recording.
    pub fn begin(&mut self, id: u64, switch: Arc<CancelSwitch>, recording: R) {
        self.recording = Some((id, recording));
        self.open.push((id, switch));
    }

    /// End the recording take if `matches` it (chord end, the stop button).
    /// It stays open to Escape while it is transcribed.
    pub fn release(&mut self, matches: impl Fn(&R) -> bool) -> Option<R> {
        match &self.recording {
            Some((_, recording)) if matches(recording) => self.recording.take().map(|(_, r)| r),
            _ => None,
        }
    }

    /// Escape: cancel every take whose text hasn't started going in. Returns
    /// their ids, and the recording take if it was one, for its microphone
    /// to be stopped.
    pub fn cancel(&mut self) -> (Vec<u64>, Option<R>) {
        let cancelled: Vec<u64> = self
            .open
            .iter()
            .filter(|(_, switch)| switch.cancel())
            .map(|(id, _)| *id)
            .collect();
        let recording = match &self.recording {
            Some((id, _)) if cancelled.contains(id) => self.recording.take().map(|(_, r)| r),
            _ => None,
        };
        (cancelled, recording)
    }

    /// The take has settled (delivered, failed or cancelled).
    pub fn end(&mut self, id: u64) {
        self.open.retain(|(open, _)| *open != id);
    }

    /// No take is recording, or still being transcribed or put in.
    pub fn is_idle(&self) -> bool {
        self.recording.is_none() && self.open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording_take(takes: &mut Takes<&'static str>, id: u64) -> Arc<CancelSwitch> {
        let switch = CancelSwitch::new();
        takes.begin(id, switch.clone(), "microphone");
        switch
    }

    #[test]
    fn idle_once_every_take_has_settled() {
        let mut takes = Takes::default();
        assert!(takes.is_idle());
        recording_take(&mut takes, 1);
        assert!(!takes.is_idle());
        takes.release(|_| true);
        assert!(!takes.is_idle(), "still being transcribed");
        takes.end(1);
        assert!(takes.is_idle());
    }

    #[test]
    fn escape_while_recording_cancels_and_stops_the_microphone() {
        let mut takes = Takes::default();
        let switch = recording_take(&mut takes, 1);
        assert_eq!(takes.cancel(), (vec![1], Some("microphone")));
        assert!(switch.is_cancelled());
        assert!(!takes.is_recording());
        // Nothing may be inserted afterwards.
        assert!(!switch.begin_delivery());
    }

    #[test]
    fn a_chord_released_after_a_cancel_finds_nothing_to_stop() {
        let mut takes = Takes::default();
        recording_take(&mut takes, 1);
        takes.cancel();
        assert_eq!(takes.release(|_| true), None);
        // And a second Escape has nothing left to cancel.
        assert_eq!(takes.cancel(), (vec![], None));
    }

    #[test]
    fn escape_while_transcribing_or_refining_cancels_the_released_take() {
        let mut takes = Takes::default();
        let switch = recording_take(&mut takes, 1);
        assert_eq!(takes.release(|_| true), Some("microphone"));
        assert_eq!(takes.cancel(), (vec![1], None));
        assert!(switch.is_cancelled());
        assert!(!switch.begin_delivery());
    }

    #[test]
    fn escape_after_insertion_began_is_ignored() {
        let mut takes = Takes::default();
        let switch = recording_take(&mut takes, 1);
        takes.release(|_| true);
        assert!(switch.begin_delivery());
        assert_eq!(takes.cancel(), (vec![], None));
        assert!(!switch.is_cancelled());
        // Delivery already claimed stays claimed.
        assert!(switch.begin_delivery());
    }

    #[test]
    fn escape_with_no_take_is_ignored() {
        let mut takes: Takes<&str> = Takes::default();
        assert_eq!(takes.cancel(), (vec![], None));
    }

    #[test]
    fn a_settled_take_is_no_longer_cancelled() {
        let mut takes = Takes::default();
        let switch = recording_take(&mut takes, 1);
        takes.release(|_| true);
        takes.end(1);
        assert_eq!(takes.cancel(), (vec![], None));
        assert!(!switch.is_cancelled());
    }

    #[test]
    fn escape_cancels_a_finishing_take_and_the_next_one_recording() {
        let mut takes = Takes::default();
        let first = recording_take(&mut takes, 1);
        takes.release(|_| true);
        let second = recording_take(&mut takes, 2);
        assert_eq!(takes.cancel(), (vec![1, 2], Some("microphone")));
        assert!(first.is_cancelled() && second.is_cancelled());
    }

    #[test]
    fn release_only_ends_a_matching_take() {
        let mut takes = Takes::default();
        recording_take(&mut takes, 1);
        assert_eq!(takes.release(|r| *r == "other"), None);
        assert!(takes.is_recording());
    }

    #[tokio::test]
    async fn cancelled_resolves_once_escape_is_pressed() {
        let switch = CancelSwitch::new();
        let waiter = {
            let switch = switch.clone();
            tokio::spawn(async move { switch.cancelled().await })
        };
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        assert!(switch.cancel());
        waiter.await.unwrap();
        // Already cancelled: resolves at once.
        switch.cancelled().await;
    }
}
