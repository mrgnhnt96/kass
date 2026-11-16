//! The Writing Style page in GPUI: styles and the apps they cover, each
//! style's settings, teaching it by replying to conversations, what it
//! learns from, and its recent corrections.

mod dictation;
mod model;
mod page;
mod teach;
mod widgets;

pub use dictation::{OnTake, StyleDictation, TakeState, TakeTracker};
pub use page::WritingStylePage;
