//! macOS TCC permission FFI.
//!
//! Direct `extern "C"` against AVFoundation (Microphone) and IOKit
//! (Input Monitoring), plus a thin objc2 call for the AV class method.
//! Screen Recording was deprecated in favour of CoreAudio process tap
//! (#588) — the [`screen_recording_status`] reader exists for
//! migration UX (showing stale grants from older builds in the
//! diagnostics surface) and always returns
//! [`super::PermissionStatus::NotApplicable`].
//!
//! This file is `mod macos;` from [`super`]; nothing here is meant for
//! cross-platform consumption. The platform-dispatching public API
//! lives in [`super`] under `cfg`-gating.

use super::PermissionStatus;
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

// ---- Microphone ---------------------------------------------------
//
// `AVAuthorizationStatus` from AVFoundation:
//   0 = NotDetermined, 1 = Restricted, 2 = Denied, 3 = Authorized.
//
// `AVMediaTypeAudio` is an exported `NSString *` constant from the
// framework. Type it as `*const AnyObject` so the signature matches
// Apple's header (`NSString * const`) — reviewer-flagged that
// `*const c_void` worked by accident on AArch64/x86_64 but lied
// about the wire shape.

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    static AVMediaTypeAudio: *const AnyObject;
}

pub fn microphone_status() -> PermissionStatus {
    // AVCaptureDevice is an Objective-C class. `objc2`'s
    // `class!()` macro resolves it at runtime (the framework is
    // dynamically linked). Calling the class method
    // `+authorizationStatusForMediaType:` returns an i32.
    unsafe {
        let cls = match AnyClass::get(c"AVCaptureDevice") {
            Some(c) => c,
            // The class missing means AVFoundation isn't loaded
            // (shouldn't happen on macOS), so we conservatively
            // report NotDetermined and let the user discover it
            // by clicking Start.
            None => return PermissionStatus::NotDetermined,
        };
        let status: i32 = msg_send![cls, authorizationStatusForMediaType: AVMediaTypeAudio];
        match status {
            0 => PermissionStatus::NotDetermined,
            1 => PermissionStatus::Denied, // "Restricted" — treat as Denied for UX.
            2 => PermissionStatus::Denied,
            3 => PermissionStatus::Granted,
            _ => PermissionStatus::NotDetermined,
        }
    }
}

// ---- Screen Recording ---------------------------------------------
//
// Screen Recording TCC is no longer required since #588 switched
// system-audio capture to `AudioHardwareCreateProcessTap` (CoreAudio tap).
// The tap does not require Screen Recording permission on macOS 26+
// (confirmed by the probe in #585; see `learnings.md`).
// We always return `NotApplicable` so the frontend's permissions UI
// does not prompt the user to grant a permission Hush no longer needs.

pub fn screen_recording_status() -> PermissionStatus {
    PermissionStatus::NotApplicable
}

// ---- Input Monitoring ---------------------------------------------
//
// `IOHIDCheckAccess(IOHIDRequestType)` returns an
// `IOHIDAccessType` enum:
//   0 = Granted, 1 = Unknown (= NotDetermined), 2 = Denied.
// `kIOHIDRequestTypeListenEvent = 1` is the Input Monitoring
// category (vs `kIOHIDRequestTypePostEvent = 0` = Accessibility).

const K_IO_HID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    // `IOHIDRequestAccess` fires the synchronous Input
    // Monitoring TCC prompt and blocks until the user responds.
    // Returns `true` (1) on grant, `false` (0) on denial /
    // dismiss. Used by the first-run wizard's Allow button
    // (#511) so the user grants permissions inline without
    // having to open System Settings manually.
    fn IOHIDRequestAccess(request_type: u32) -> u8;
}

pub fn input_monitoring_status() -> PermissionStatus {
    unsafe {
        match IOHIDCheckAccess(K_IO_HID_REQUEST_TYPE_LISTEN_EVENT) {
            0 => PermissionStatus::Granted,
            1 => PermissionStatus::NotDetermined,
            2 => PermissionStatus::Denied,
            _ => PermissionStatus::NotDetermined,
        }
    }
}

/// Fire the macOS Input Monitoring TCC prompt synchronously
/// (#511). Returns `true` if the user granted on this call,
/// `false` if they denied or dismissed. Already-granted
/// installs return `true` immediately without a prompt.
pub fn request_input_monitoring() -> bool {
    unsafe { IOHIDRequestAccess(K_IO_HID_REQUEST_TYPE_LISTEN_EVENT) != 0 }
}

/// Fire the macOS Microphone TCC prompt asynchronously (#511).
/// `AVCaptureDevice requestAccessForMediaType:completionHandler:`
/// returns immediately and shows the system dialog; the user
/// responds at their leisure. We pass a NULL completion handler
/// because the frontend polls `get_permission_health` to
/// observe the resulting state — wiring a block-based callback
/// would require an extra dependency (`block2`) for no
/// behavioural gain over the polling shape that's already
/// established for the Settings → Permissions tab.
pub fn request_microphone() {
    unsafe {
        let cls = match AnyClass::get(c"AVCaptureDevice") {
            Some(c) => c,
            None => return,
        };
        let _: () = msg_send![
            cls,
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: std::ptr::null::<std::ffi::c_void>()
        ];
    }
}
