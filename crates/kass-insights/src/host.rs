//! What the Insights page asks of the app around it. Every method has a
//! default, so the page runs on its own, as in the example.

use gpui_kit::Image;
use std::sync::Arc;

pub trait InsightsHost {
    /// The icon of an installed app, by bundle id.
    fn app_icon(&self, _bundle_id: &str) -> Option<Arc<Image>> {
        None
    }
}

/// The host when the page runs on its own.
pub struct NoHost;

impl InsightsHost for NoHost {}
