//! Kass's look in GPUI: the theme tokens and the controls every screen is
//! built from. Call [`init`] once at startup, after `gpui_kit::init`, and open
//! windows with `gpui_kit::open_window` so dialogs and toasts have a root to
//! draw in. Register [`Assets`] on the application for the icons.

mod app_icons;
mod assets;
pub mod controls;
pub mod theme;

pub use app_icons::{app_icon, set_app_icons};
pub use assets::Assets;
pub use controls::*;
pub use theme::{
    Appearance, KassTheme, Palette, ThemeChoice, metrics, set_theme_choice,
    system_appearance_changed, theme,
};

use gpui_kit::App;

/// Install Kass's theme. Call after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    theme::init(cx);
}
