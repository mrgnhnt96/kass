//! Saying a phrase instead of typing it. The app that hosts the page owns
//! dictation (starting a take, its state events, claiming a shortcut take
//! before it's typed, finding the take's raw transcript), so it hands the
//! page an implementation of [`PhraseDictation`]. Without one, the page shows
//! no mic and asks for the phrase to be typed.

use gpui_kit::{App, Subscription};
use std::rc::Rc;

/// Where a take for the "When I say" box is, as the box shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhraseTake {
    /// Heard: from the mic button, or while the dictation shortcut is held.
    Listening { from_button: bool },
    /// Finished speaking; the take is being transcribed.
    Cleaning,
    /// Cancelled, failed, or done with nothing heard.
    Idle,
    /// Done: the take's raw transcript, as heard, before cleanup rewrote it
    /// or swapped in a phrase already in the dictionary. The box tidies it.
    Heard(String),
}

/// Whether a shortcut take that begins now is meant for the box.
pub type AimsHere = Rc<dyn Fn(&App) -> bool>;

/// Called with each step of a take the box follows.
pub type OnTake = Rc<dyn Fn(PhraseTake, &mut App)>;

pub trait PhraseDictation: 'static {
    /// Starts a take from the mic button. Its text goes to the box, never
    /// typed into another app.
    fn start(&self, cx: &mut App);

    /// Ends the mic button's take.
    fn stop(&self, cx: &mut App);

    /// Follows takes for one box until the subscription is dropped: every
    /// take from [`start`](Self::start), and each shortcut take that begins
    /// while `aims_here` is true, which the host claims instead of typing.
    /// A shortcut take that begins elsewhere is typed as usual and not
    /// reported.
    fn follow(&self, aims_here: AimsHere, on_take: OnTake, cx: &mut App) -> Subscription;
}
