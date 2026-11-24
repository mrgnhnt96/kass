//! The Dictionary page: words and phrases Kass should hear or write a
//! certain way, everywhere, in a writing style, or in one app.

mod dictation;
mod page;
mod places;
mod say_box;
pub mod scopes;
mod widgets;

pub use dictation::{AimsHere, OnTake, PhraseDictation, PhraseTake};
pub use page::DictionaryPage;
pub use places::{OnToggle, Selected, places_menu};
pub use say_box::{SayBox, SayFilled, SayHint};
pub use widgets::{chord_keys, with_keys};
