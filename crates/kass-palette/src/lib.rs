//! Kass's ⌘K command palette in GPUI: search captures and run commands.

pub mod search;
mod view;

pub use view::{CommandPalette, PaletteCommand, PaletteHost};
