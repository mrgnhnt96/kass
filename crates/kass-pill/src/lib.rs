//! Kass's dictation pill in GPUI: the capsule at the bottom of the screen
//! that shows a take recording, transcribing, refining and done, and the
//! chip above it when the user asks for a writing style by name.

pub mod chip;
pub mod ease;
pub mod level;
mod pill;
pub mod session;

pub use pill::{DictatePill, NoHost, PILL_SPACE, PillHost};
pub use session::{LevelEvent, Phase, PillState, StateEvent, StyleEvent};
