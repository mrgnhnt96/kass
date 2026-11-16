//! The shared controls, matching the React app's `components/ui`.

mod button;
mod dialog;
mod input;
pub mod keys;
mod select;
mod setting;
mod sidebar;
mod toast;
mod toggle;

/// A shared callback with no event: a confirm, a toast action, a restart.
pub type Handler = std::rc::Rc<dyn Fn(&mut gpui_kit::Window, &mut gpui_kit::App)>;

pub use button::{Button, ButtonSize, ButtonVariant};
pub use dialog::{Confirm, close_dialog, dialog_footer, dialog_header, open_confirm, open_dialog};
pub use input::text_input;
pub use select::{SelectOption, SelectOptions, select, select_state};
pub use setting::{SettingRow, SettingSection};
pub use sidebar::{AlertLevel, Sidebar, SidebarFooter, SidebarTab, TabAlert, version_lines};
pub use toast::{Toast, ToastKind, show_toast};
pub use toggle::Toggle;
