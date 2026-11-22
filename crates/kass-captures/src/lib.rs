//! Kass's Captures page in GPUI: every dictation, what Kass heard and
//! delivered, and teaching it what you meant.

mod app_list;
pub mod audio;
mod detail;
pub mod format;
pub mod host;
mod inspector;
mod list;
mod page;
pub mod phrases;
pub mod player;
mod spelling;
pub mod teach;
mod widgets;
pub mod word_diff;

pub use host::{CapturesHost, NoHost, Route};
pub use page::CapturesPage;
