//! What kind of connection an input device has, from CoreAudio's transport
//! type. A Bluetooth headset's microphone is slow to open (the headset
//! switches from its listening profile to its headset profile first), so a
//! take on one starts on the built-in microphone ([`super::capture`]).

/// How an input device is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    BuiltIn,
    Bluetooth,
    Other,
}

/// Classify a CoreAudio transport type (a four-char code).
pub fn kind_of_transport(transport: u32) -> DeviceKind {
    match &transport.to_be_bytes() {
        b"bltn" => DeviceKind::BuiltIn,
        b"blue" | b"blea" => DeviceKind::Bluetooth,
        _ => DeviceKind::Other,
    }
}

/// Every CoreAudio device by name (as cpal names it) with its kind.
#[cfg(target_os = "macos")]
pub fn device_kinds() -> Vec<(String, DeviceKind)> {
    macos::devices()
        .into_iter()
        .map(|(name, transport)| (name, kind_of_transport(transport)))
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn device_kinds() -> Vec<(String, DeviceKind)> {
    Vec::new()
}

/// The kind of the device named `name` among `kinds`; [`DeviceKind::Other`]
/// when it isn't there.
pub fn kind_of(kinds: &[(String, DeviceKind)], name: &str) -> DeviceKind {
    kinds
        .iter()
        .find(|(device_name, _)| device_name == name)
        .map_or(DeviceKind::Other, |&(_, kind)| kind)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::ptr;

    use core_foundation_sys::base::CFRelease;
    use core_foundation_sys::string::CFStringRef;

    const fn code(c: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*c)
    }

    const SYSTEM_OBJECT: u32 = 1;
    const HARDWARE_DEVICES: u32 = code(b"dev#");
    /// `kAudioDevicePropertyDeviceNameCFString`, the name cpal reports.
    const DEVICE_NAME: u32 = code(b"lnam");
    const TRANSPORT_TYPE: u32 = code(b"tran");
    const SCOPE_GLOBAL: u32 = code(b"glob");
    const ELEMENT_MAIN: u32 = 0;

    #[repr(C)]
    struct PropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const PropertyAddress,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
    }

    fn address(selector: u32) -> PropertyAddress {
        PropertyAddress {
            selector,
            scope: SCOPE_GLOBAL,
            element: ELEMENT_MAIN,
        }
    }

    /// Read a fixed-size property.
    fn read<T: Default>(object: u32, selector: u32) -> Option<T> {
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `value` is a valid buffer of `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                object,
                &address(selector),
                0,
                ptr::null(),
                &mut size,
                &mut value as *mut T as *mut c_void,
            )
        };
        (status == 0).then_some(value)
    }

    fn name(device: u32) -> Option<String> {
        let mut string: CFStringRef = ptr::null();
        let mut size = std::mem::size_of::<CFStringRef>() as u32;
        // SAFETY: the property is a CFStringRef we own and release.
        unsafe {
            let status = AudioObjectGetPropertyData(
                device,
                &address(DEVICE_NAME),
                0,
                ptr::null(),
                &mut size,
                &mut string as *mut CFStringRef as *mut c_void,
            );
            if status != 0 || string.is_null() {
                return None;
            }
            let name = crate::focus_capture::cfstring_to_rust(string);
            CFRelease(string as *const c_void);
            name
        }
    }

    /// Every audio device's name and transport type.
    pub fn devices() -> Vec<(String, u32)> {
        let mut size = 0u32;
        // SAFETY: a size query on the system object.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                SYSTEM_OBJECT,
                &address(HARDWARE_DEVICES),
                0,
                ptr::null(),
                &mut size,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        let mut ids = vec![0u32; size as usize / std::mem::size_of::<u32>()];
        // SAFETY: `ids` holds `size` bytes.
        let status = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM_OBJECT,
                &address(HARDWARE_DEVICES),
                0,
                ptr::null(),
                &mut size,
                ids.as_mut_ptr() as *mut c_void,
            )
        };
        if status != 0 {
            return Vec::new();
        }
        ids.truncate(size as usize / std::mem::size_of::<u32>());
        ids.into_iter()
            .filter_map(|id| Some((name(id)?, read::<u32>(id, TRANSPORT_TYPE)?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_codes_classify() {
        let code = |c: &[u8; 4]| u32::from_be_bytes(*c);
        assert_eq!(kind_of_transport(code(b"bltn")), DeviceKind::BuiltIn);
        assert_eq!(kind_of_transport(code(b"blue")), DeviceKind::Bluetooth);
        assert_eq!(kind_of_transport(code(b"blea")), DeviceKind::Bluetooth);
        assert_eq!(kind_of_transport(code(b"usb ")), DeviceKind::Other);
    }
}
