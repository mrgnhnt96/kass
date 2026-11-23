//! Installed apps' icons, for any page that shows an app. The app around the
//! pages says where they come from once with [`set_app_icons`]; until then,
//! and in the examples, there are none and pages draw the app's initial.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Global, Image};

type Lookup = Rc<dyn Fn(&str) -> Option<Arc<Image>>>;

struct AppIcons(Lookup);

impl Global for AppIcons {}

/// Where app icons come from: the icon of the installed app with a bundle
/// id, or `None` when it isn't installed.
pub fn set_app_icons(lookup: impl Fn(&str) -> Option<Arc<Image>> + 'static, cx: &mut App) {
    cx.set_global(AppIcons(Rc::new(lookup)));
}

/// The icon of the installed app with `bundle_id`, if any.
pub fn app_icon(bundle_id: &str, cx: &App) -> Option<Arc<Image>> {
    if bundle_id.is_empty() {
        return None;
    }
    (cx.try_global::<AppIcons>()?.0)(bundle_id)
}
