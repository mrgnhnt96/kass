//! App icons for Captures, which shows the app each dictation went to, and
//! app categories, which suggest a writing style for a new app.
//!
//! The icon is looked up by bundle id at display time rather than saved with
//! the capture, so it follows the installed app and costs nothing to store.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::focus_capture::{ns_string_to_rust, AutoreleasePool};
use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};

type Id = *mut Object;

/// The App Store category the app with `bundle_id` declares in its
/// Info.plist (`LSApplicationCategoryType`, e.g.
/// `public.app-category.developer-tools`). `None` when the app isn't
/// installed or declares none. Read once per app per launch.
pub fn app_category(bundle_id: &str) -> Option<String> {
    static CATEGORIES: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = CATEGORIES.get_or_init(Default::default);
    if let Some(known) = cache.lock().ok()?.get(bundle_id) {
        return known.clone();
    }
    let category = read_category(bundle_id);
    if let Ok(mut known) = cache.lock() {
        known.insert(bundle_id.to_string(), category.clone());
    }
    category
}

fn read_category(bundle_id: &str) -> Option<String> {
    let bundle_id = std::ffi::CString::new(bundle_id).ok()?;
    unsafe {
        let _pool = AutoreleasePool::new();
        let workspace: Id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let id: Id = msg_send![class!(NSString), stringWithUTF8String: bundle_id.as_ptr()];
        let url: Id = msg_send![workspace, URLForApplicationWithBundleIdentifier: id];
        if url.is_null() {
            return None;
        }
        let bundle: Id = msg_send![class!(NSBundle), bundleWithURL: url];
        if bundle.is_null() {
            return None;
        }
        let key: Id = msg_send![class!(NSString), stringWithUTF8String: c"LSApplicationCategoryType".as_ptr()];
        let value: Id = msg_send![bundle, objectForInfoDictionaryKey: key];
        if value.is_null() {
            return None;
        }
        let is_string: bool = msg_send![value, isKindOfClass: class!(NSString)];
        if !is_string {
            return None;
        }
        ns_string_to_rust(value).filter(|category| !category.is_empty())
    }
}

/// Pixel size of the rendered icon: sharp at 2x for a 16pt display.
const ICON_PIXELS: i64 = 64;

#[repr(C)]
struct NSRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// The icon of the app with `bundle_id`, as a PNG data URL. `None` when no
/// installed app has that bundle id.
pub fn icon_data_url(bundle_id: &str) -> Option<String> {
    let bundle_id = std::ffi::CString::new(bundle_id).ok()?;
    unsafe {
        let _pool = AutoreleasePool::new();
        let workspace: Id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let id: Id = msg_send![class!(NSString), stringWithUTF8String: bundle_id.as_ptr()];
        let url: Id = msg_send![workspace, URLForApplicationWithBundleIdentifier: id];
        if url.is_null() {
            return None;
        }
        let path: Id = msg_send![url, path];
        let icon: Id = msg_send![workspace, iconForFile: path];
        if icon.is_null() {
            return None;
        }

        // Draw into a fixed-size bitmap: NSImage's own PNG export would pick
        // its largest representation, up to 1024px.
        let rep: Id = msg_send![class!(NSBitmapImageRep), alloc];
        let color_space: Id =
            msg_send![class!(NSString), stringWithUTF8String: c"NSDeviceRGBColorSpace".as_ptr()];
        let rep: Id = msg_send![rep,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>()
            pixelsWide: ICON_PIXELS
            pixelsHigh: ICON_PIXELS
            bitsPerSample: 8i64
            samplesPerPixel: 4i64
            hasAlpha: true
            isPlanar: false
            colorSpaceName: color_space
            bytesPerRow: 0i64
            bitsPerPixel: 0i64];
        if rep.is_null() {
            return None;
        }
        let _: Id = msg_send![rep, autorelease];
        let context: Id =
            msg_send![class!(NSGraphicsContext), graphicsContextWithBitmapImageRep: rep];
        if context.is_null() {
            return None;
        }
        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: context];
        let size = ICON_PIXELS as f64;
        // NSCompositingOperationSourceOver = 2.
        let _: () = msg_send![icon,
            drawInRect: NSRect { x: 0.0, y: 0.0, width: size, height: size }
            fromRect: NSRect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 }
            operation: 2u64
            fraction: 1.0f64];
        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

        // NSBitmapImageFileTypePNG = 4.
        let properties: Id = msg_send![class!(NSDictionary), dictionary];
        let png: Id = msg_send![rep, representationUsingType: 4u64 properties: properties];
        if png.is_null() {
            return None;
        }
        let encoded: Id = msg_send![png, base64EncodedStringWithOptions: 0u64];
        ns_string_to_rust(encoded).map(|b64| format!("data:image/png;base64,{b64}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{app_category, icon_data_url};

    #[test]
    fn reads_an_installed_apps_category() {
        assert_eq!(
            app_category("com.apple.TextEdit").as_deref(),
            Some("public.app-category.productivity")
        );
        // Cached: the second read gives the same answer.
        assert_eq!(
            app_category("com.apple.TextEdit").as_deref(),
            Some("public.app-category.productivity")
        );
        assert_eq!(app_category("sh.kass.no-such-app"), None);
    }

    #[test]
    fn renders_an_installed_apps_icon() {
        let url = icon_data_url("com.apple.finder").expect("Finder has an icon");
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"));
    }

    #[test]
    fn unknown_bundle_ids_have_no_icon() {
        assert_eq!(icon_data_url("sh.kass.no-such-app"), None);
    }
}
