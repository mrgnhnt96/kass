//! What the native Insights page needs from the app: installed apps' icons.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use gpui_kit::{Image, ImageFormat};
use kass_insights::InsightsHost;

use crate::app_icon;

/// App icons by bundle id, read from the system once each.
#[derive(Default)]
pub struct Icons {
    known: RefCell<HashMap<String, Option<Arc<Image>>>>,
}

impl InsightsHost for Icons {
    fn app_icon(&self, bundle_id: &str) -> Option<Arc<Image>> {
        if let Some(known) = self.known.borrow().get(bundle_id) {
            return known.clone();
        }
        let icon = app_icon::icon_data_url(bundle_id).and_then(|url| decode(&url));
        self.known
            .borrow_mut()
            .insert(bundle_id.to_string(), icon.clone());
        icon
    }
}

/// A `data:image/png;base64,…` URL as an image.
fn decode(url: &str) -> Option<Arc<Image>> {
    let data = url.strip_prefix("data:image/png;base64,")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn reads_png_data_urls_only() {
        assert!(decode("data:image/png;base64,iVBORw0KGgo=").is_some());
        assert!(decode("data:image/png;base64,not base64!").is_none());
        assert!(decode("https://example.com/icon.png").is_none());
    }
}
