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

/// Strip `com.apple.quarantine` from the running `.app` bundle.
///
/// Returns `true` if the xattr was **present and successfully removed**,
/// Log the TCC identity and permission state at key lifecycle moments
/// (app startup, IM grant). Helps diagnose cdhash/identity mismatches
/// that cause permission grants not to survive a restart.
///
/// Runs `codesign --display --verbose=4` against the .app bundle to
/// surface the `Identifier` and `CandidateCDHash` fields — the pair
/// macOS uses as the TCC row key on macOS 26. If these differ between
/// the process that called `IOHIDRequestAccess` and the next launch,
/// the grant is invisible to the new process.
pub fn log_tcc_identity(context: &str) {
    let Ok(exe) = std::env::current_exe() else {
        tracing::warn!("[tcc-id] {context}: could not determine current exe path");
        return;
    };

    // Locate the .app bundle root (up to 5 levels up from the binary).
    let mut path = exe.as_path();
    let mut bundle = None;
    for _ in 0..5 {
        let Some(parent) = path.parent() else { break };
        if parent.extension().map(|e| e == "app").unwrap_or(false) {
            bundle = Some(parent.to_path_buf());
            break;
        }
        path = parent;
    }

    let quarantine_stripped = std::env::var("HUSH_QUARANTINE_STRIPPED").is_ok();

    let Some(bundle_path) = bundle else {
        tracing::info!(
            context,
            exe = %exe.display(),
            quarantine_stripped,
            "[tcc-id] running outside .app bundle — no TCC app identity (dev binary)"
        );
        return;
    };

    // Probe quarantine (independent of the env-var guard).
    let quarantine_present = std::process::Command::new("xattr")
        .args(["-p", "com.apple.quarantine"])
        .arg(&bundle_path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // codesign writes its --display output to stderr.
    let codesign_info = std::process::Command::new("codesign")
        .args(["--display", "--verbose=4"])
        .arg(&bundle_path)
        .output();

    let (identifier, cdhash) = match codesign_info {
        Ok(o) => {
            let raw = String::from_utf8_lossy(&o.stderr);
            let id = raw
                .lines()
                .find(|l| l.starts_with("Identifier="))
                .map(|l| l.trim_start_matches("Identifier=").to_owned())
                .unwrap_or_else(|| "(not found)".to_owned());
            // codesign may list multiple CandidateCDHash lines; the sha256 one
            // is what macOS 26 uses for TCC keying.
            let hash = raw
                .lines()
                .find(|l| l.starts_with("CandidateCDHash sha256="))
                .map(|l| l.trim_start_matches("CandidateCDHash sha256=").to_owned())
                .unwrap_or_else(|| "(not found)".to_owned());
            (id, hash)
        }
        Err(e) => (format!("codesign error: {e}"), String::new()),
    };

    let statuses = super::read_all();
    tracing::info!(
        context,
        bundle = %bundle_path.display(),
        quarantine_present,
        quarantine_stripped,
        %identifier,
        cdhash = %cdhash,
        microphone = ?statuses.microphone,
        input_monitoring = ?statuses.input_monitoring,
        screen_recording = ?statuses.screen_recording,
        "[tcc-id] TCC identity + permission snapshot"
    );
}

///
/// ## Why the return value matters — the restart contract
///
/// On macOS, a process's TCC identity is baked in **at launch time** based
/// on the code signature _and_ the quarantine state of the bundle at that
/// moment. Stripping the xattr from disk does not retroactively change the
/// running process's identity for `IOHIDRequestAccess` / `IOHIDCheckAccess`.
///
/// The correct fix is therefore:
/// 1. Strip quarantine (this call).
/// 2. If this call returns `true` (quarantine was present), **restart the
///    app immediately** before any window is shown.
/// 3. The relaunch inherits no quarantine → clean identity → both the TCC
///    grant and all future status checks use the same identity.
///
/// `tauri:bundle` (debug install via `cp -R`) doesn't need this because
/// `cp -R` never sets the quarantine xattr. DMG installs do because Finder
/// adds it when the user drags the app out.
///
/// Silently returns `false` when:
/// - The binary isn't inside an `.app` bundle (`cargo tauri dev`)
/// - The xattr is already absent (normal non-DMG installs)
/// - The process lacks write permission to the bundle path (e.g.,
///   system-wide `/Applications`; Gatekeeper handles it on first run)
///
/// See `learnings.md` 2026-05-13 for the full investigation.
pub fn strip_app_quarantine() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    // Walk up the path looking for the .app bundle root (up to 5 levels).
    // Typical layout: Hush.app/Contents/MacOS/hush — so the .app is 3 up.
    let mut path = exe.as_path();
    for _ in 0..5 {
        let Some(parent) = path.parent() else {
            return false;
        };
        if parent.extension().map(|e| e == "app").unwrap_or(false) {
            // First CHECK if the bundle root has quarantine set.
            // `xattr -p` exits 0 if the named attribute exists on the path,
            // exits 1 if absent. We MUST do this before the `-dr` strip because
            // `xattr -dr` (recursive delete) exits 0 even when no files carried
            // the attribute — a vacuous success that would make us return `true`
            // on every launch, causing an infinite restart loop.
            let check = std::process::Command::new("xattr")
                .args(["-p", "com.apple.quarantine"])
                .arg(parent)
                .output();
            let present = matches!(&check, Ok(o) if o.status.success());
            if !present {
                return false;
            }
            // Quarantine confirmed present — strip recursively.
            let strip = std::process::Command::new("xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(parent)
                .output();
            return match strip {
                Ok(o) if o.status.success() => {
                    tracing::info!(
                        bundle = ?parent,
                        "stripped com.apple.quarantine from app bundle — will restart for clean TCC identity"
                    );
                    true
                }
                Ok(o) => {
                    tracing::debug!(
                        status = ?o.status,
                        stderr = %String::from_utf8_lossy(&o.stderr),
                        "xattr quarantine strip: unexpected exit status after confirmed presence"
                    );
                    false
                }
                Err(e) => {
                    tracing::debug!(error = %e, "xattr quarantine strip: failed to spawn");
                    false
                }
            };
        }
        path = parent;
    }
    false
}
