//! Programmatic macOS permission status checks for the three TCC
//! categories Hush touches: Microphone, Screen Recording, and Input
//! Monitoring. The earlier diagnostic surface (in `ipc::commands`)
//! only emitted hint copy because of a long-held belief that macOS
//! doesn't expose read access to TCC. That's true for some
//! categories (Accessibility, Full Disk Access) — but **not** for
//! these three:
//!
//! - **Microphone**:
//!   `+[AVCaptureDevice authorizationStatusForMediaType:]` returns a
//!   real `AVAuthorizationStatus` enum without prompting.
//! - **Screen Recording**:
//!   `CGPreflightScreenCaptureAccess()` (CoreGraphics) returns a Bool
//!   without triggering the prompt.
//! - **Input Monitoring**:
//!   `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)` (IOKit) returns
//!   an `IOHIDAccessType` enum without prompting.
//!
//! Read these on demand and surface them through
//! [`crate::ipc::commands::diagnose_macos_permissions`] so the
//! frontend can render a green "all granted" affordance instead of
//! the unconditional yellow hint.
//!
//! ## Why FFI rather than the objc2-* binding crates
//!
//! The three system functions are simple C signatures (the mic one
//! is technically Objective-C, called via objc2 since it's already
//! in the dep tree from `screencapturekit`). Adding direct deps on
//! `objc2-av-foundation`, `objc2-core-graphics`, `objc2-io-kit`
//! would land a few hundred KLOC of generated bindings for three
//! function calls — not worth the build-time hit. Raw `extern "C"`
//! against the framework + a thin objc2 call for the AV one is
//! ~50 LOC and zero new transitive deps.

use serde::Serialize;

/// Programmatic status of a single TCC-gated permission. Mirrors the
/// `AVAuthorizationStatus` shape (the most expressive of the three
/// system APIs) and is also able to express "this platform doesn't
/// gate this permission" via [`Self::NotApplicable`] for non-macOS
/// builds.
///
/// camelCase serde rename keeps the wire shape consistent with the
/// frontend's other tagged unions; tag is `kebab-case` so a future
/// extension (e.g. `restricted` for MDM-locked categories) can land
/// without churning the JSON keys.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionStatus {
    /// User has granted the permission.
    Granted,
    /// User has explicitly denied the permission. Re-prompting from
    /// in-app no longer triggers the OS dialog; the user has to flip
    /// the switch in System Settings (or `tccutil reset`).
    Denied,
    /// User has not yet been asked — the OS prompt fires the next
    /// time the gated API is called. For Hush this is the most
    /// common state on a fresh install.
    NotDetermined,
    /// Hush is on a non-macOS host where this permission concept
    /// doesn't apply (Linux mic via PulseAudio, Windows mic via the
    /// Privacy framework — both gated outside the app).
    NotApplicable,
}

/// Snapshot of all three TCC permissions Hush touches. Read once at
/// settings-window mount and after a `tccutil reset` round-trip.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatuses {
    pub microphone: PermissionStatus,
    pub screen_recording: PermissionStatus,
    pub input_monitoring: PermissionStatus,
}

/// Read the current grant state for all three permissions. Cheap
/// and side-effect-free on macOS (no prompts trigger). On non-macOS
/// every field is [`PermissionStatus::NotApplicable`].
pub fn read_all() -> PermissionStatuses {
    #[cfg(target_os = "macos")]
    {
        PermissionStatuses {
            microphone: macos::microphone_status(),
            screen_recording: macos::screen_recording_status(),
            input_monitoring: macos::input_monitoring_status(),
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        PermissionStatuses {
            microphone: PermissionStatus::NotApplicable,
            screen_recording: PermissionStatus::NotApplicable,
            input_monitoring: PermissionStatus::NotApplicable,
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PermissionStatus;
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use std::ffi::c_void;

    // ---- Microphone ---------------------------------------------------
    //
    // `AVAuthorizationStatus` from AVFoundation:
    //   0 = NotDetermined, 1 = Restricted, 2 = Denied, 3 = Authorized.
    //
    // `AVMediaTypeAudio` is an exported NSString constant from the
    // framework; we link against it as an extern static.

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: *const c_void;
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
    // `CGPreflightScreenCaptureAccess()` returns a `bool` indicating
    // whether the calling process currently has Screen Recording
    // access. Side-effect-free; does not trigger the prompt.
    //
    // There is no "NotDetermined" variant exposed by this API — the
    // OS returns false in both the "never asked" and "explicitly
    // denied" cases. We map false to NotDetermined so the frontend
    // hint copy can stay neutral ("not yet granted") rather than
    // accusatory ("denied — fix it"). When the user actually starts
    // a system-audio meeting and the prompt fires, that's where the
    // determined-vs-undetermined distinction shows up — and by then
    // the next read flips to Granted or Denied accurately.

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }

    pub fn screen_recording_status() -> PermissionStatus {
        unsafe {
            if CGPreflightScreenCaptureAccess() {
                PermissionStatus::Granted
            } else {
                PermissionStatus::NotDetermined
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On macOS the call must not panic and must return the host's
    /// real status. We don't assert specific values — the test runs
    /// on whatever machine CI provides — but reading without a panic
    /// is itself the test of the FFI signatures.
    #[test]
    fn read_all_does_not_panic() {
        let _ = read_all();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_reports_not_applicable() {
        let s = read_all();
        assert_eq!(s.microphone, PermissionStatus::NotApplicable);
        assert_eq!(s.screen_recording, PermissionStatus::NotApplicable);
        assert_eq!(s.input_monitoring, PermissionStatus::NotApplicable);
    }
}
