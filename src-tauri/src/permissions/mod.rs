//! Programmatic permission status checks.
//!
//! Cross-platform types ([`PermissionStatus`], [`PermissionsHealth`], etc.)
//! plus the platform-dispatching public API ([`read_all`],
//! [`request_microphone_permission`], [`request_input_monitoring_permission`])
//! live in this file. The macOS-specific FFI lives in [`macos`]; future
//! Linux (#106) and Windows (#107) implementations will be peers under
//! the same pattern.
//!
//! ## Why this module exists
//!
//! macOS exposes read access to three TCC categories Hush touches:
//!
//! - **Microphone**:
//!   `+[AVCaptureDevice authorizationStatusForMediaType:]` returns a
//!   real `AVAuthorizationStatus` enum without prompting.
//! - **Screen Recording**:
//!   `CGPreflightScreenCaptureAccess()` (CoreGraphics) returns a Bool
//!   without triggering the prompt. Hush no longer requires Screen
//!   Recording post-#588 (system audio uses CoreAudio process tap),
//!   but the read path stays for migration UX.
//! - **Input Monitoring**:
//!   `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)` (IOKit) returns
//!   an `IOHIDAccessType` enum without prompting.
//!
//! Read these on demand and surface them through
//! [`crate::ipc::commands::permissions::diagnose_macos_permissions`] so
//! the frontend can render a green "all granted" affordance instead of
//! the unconditional yellow hint.
//!
//! ## Why FFI rather than the objc2-* binding crates
//!
//! The three system functions are simple C signatures (the mic one is
//! technically Objective-C, called via objc2 since it's already in the
//! dep tree from `screencapturekit`). Adding direct deps on
//! `objc2-av-foundation`, `objc2-core-graphics`, `objc2-io-kit` would
//! land a few hundred KLOC of generated bindings for three function
//! calls — not worth the build-time hit. Raw `extern "C"` against the
//! framework + a thin objc2 call for the AV one is ~50 LOC and zero
//! new transitive deps.

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;

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

/// Three-state health view per permission (#378). Differentiates
/// "never granted" from "was granted, now revoked" — which the
/// raw OS APIs collapse into a single `false` for Screen
/// Recording (a notarised rebuild rotates the signing identity,
/// TCC's bundle-ID + signature fingerprint no longer matches, the
/// row is silently invalidated). The frontend renders a traffic-
/// light dot per state; the Stale variant gets a clearer "access
/// was revoked — restore in System Settings" hint than a generic
/// "enable in System Settings".
///
/// Mapping from raw `PermissionStatus` + `last_confirmed`:
///
/// | status        | last_confirmed | health      |
/// |---------------|----------------|-------------|
/// | Granted       | any            | Confirmed   |
/// | Denied / NotDetermined | Some  | Stale       |
/// | Denied / NotDetermined | None  | NotGranted  |
/// | NotApplicable | any            | NotApplicable |
///
/// `NotApplicable` keeps the type usable on non-macOS without
/// forcing the frontend to special-case the platform.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionHealth {
    /// Currently granted — preflight returned true. The dot reads
    /// green; no action affordance.
    Confirmed,
    /// Previously granted (we have a `last_confirmed` timestamp on
    /// disk) but preflight now returns false. Almost always means
    /// a notarisation rebuild rotated the signing identity and TCC
    /// invalidated the entry. Yellow dot + "access was revoked"
    /// copy.
    Stale,
    /// No record of a prior grant. Either fresh install or the
    /// user reset permissions via `tccutil`. Red dot + "enable in
    /// System Settings" copy.
    NotGranted,
    /// Hush is on a non-macOS host.
    NotApplicable,
}

/// Companion to `PermissionStatuses`: the same three permissions
/// but expressed as health states (#378). Returned by the
/// `get_permission_health` IPC; the frontend uses it to render
/// the Permissions tab traffic-light row + the small main-window
/// status dot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsHealth {
    pub microphone: PermissionHealth,
    pub screen_recording: PermissionHealth,
    pub input_monitoring: PermissionHealth,
}

/// Resolve raw permission statuses + persisted `last_confirmed`
/// timestamps into a [`PermissionsHealth`] snapshot (#378).
///
/// Pulled out of the IPC entry point so the three-state transition
/// logic has a unit-testable seam without an `AppState`. Each
/// timestamp arg is `Option<&str>` matching the settings repo's
/// return shape — `None` means "no row in the settings table" and
/// is the load-bearing signal for the NotGranted vs Stale split.
pub fn evaluate_permissions_health(
    statuses: PermissionStatuses,
    screen_recording_last_confirmed: Option<&str>,
    microphone_last_confirmed: Option<&str>,
) -> PermissionsHealth {
    PermissionsHealth {
        microphone: classify_health(statuses.microphone, microphone_last_confirmed),
        screen_recording: classify_health(
            statuses.screen_recording,
            screen_recording_last_confirmed,
        ),
        // Input Monitoring isn't covered by the staleness story —
        // the IOHIDCheckAccess API already exposes Denied vs
        // NotDetermined accurately, so the three-state mapping is
        // mechanical: Granted → Confirmed, Denied → NotGranted,
        // NotDetermined → NotGranted, NotApplicable → NotApplicable.
        // Future-proofed in `classify_health` by passing `None` for
        // last_confirmed; the helper handles it.
        input_monitoring: classify_health(statuses.input_monitoring, None),
    }
}

fn classify_health(status: PermissionStatus, last_confirmed: Option<&str>) -> PermissionHealth {
    match status {
        PermissionStatus::Granted => PermissionHealth::Confirmed,
        PermissionStatus::NotApplicable => PermissionHealth::NotApplicable,
        PermissionStatus::Denied | PermissionStatus::NotDetermined => match last_confirmed {
            Some(_) => PermissionHealth::Stale,
            None => PermissionHealth::NotGranted,
        },
    }
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

/// Read the current microphone permission status without triggering
/// a TCC prompt. Safe to call at any time including before the user
/// has interacted with any audio API.
pub fn microphone_status() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        macos::microphone_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        PermissionStatus::NotApplicable
    }
}

/// Synchronous Input-Monitoring TCC prompt (#511). Blocks the
/// caller until the user clicks Allow / Deny on the system
/// dialog. `true` = granted by the call (or already-granted
/// reading without a prompt). No-op stub returns `true` on
/// non-macOS where the per-app TCC layer doesn't exist.
pub fn request_input_monitoring_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::request_input_monitoring()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Asynchronous Microphone TCC prompt (#511). Returns immediately
/// after firing the system dialog; the user's choice surfaces via
/// the existing `read_all()` poll cadence the frontend already
/// runs. No-op on non-macOS — there's no per-app mic TCC there.
pub fn request_microphone_permission() {
    #[cfg(target_os = "macos")]
    {
        macos::request_microphone();
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

    // -- evaluate_permissions_health (#378) ------------------------------

    #[test]
    fn classify_granted_is_confirmed_regardless_of_last_confirmed() {
        // Granted always wins over the timestamp — the live OS API
        // is the source of truth when the answer is yes.
        assert_eq!(
            classify_health(PermissionStatus::Granted, None),
            PermissionHealth::Confirmed
        );
        assert_eq!(
            classify_health(PermissionStatus::Granted, Some("2026-04-01T10:00:00Z")),
            PermissionHealth::Confirmed
        );
    }

    #[test]
    fn classify_denied_with_history_is_stale() {
        // Was granted before, no longer is — the cert / bundle
        // rotation case the issue calls out as the load-bearing
        // user-experience problem.
        assert_eq!(
            classify_health(PermissionStatus::Denied, Some("2026-04-01T10:00:00Z"),),
            PermissionHealth::Stale
        );
        assert_eq!(
            classify_health(
                PermissionStatus::NotDetermined,
                Some("2026-04-01T10:00:00Z"),
            ),
            PermissionHealth::Stale,
            "preflight-false on Screen Recording (which can't \
             distinguish denied vs not-asked) maps to Stale when \
             we have a prior grant"
        );
    }

    #[test]
    fn classify_denied_without_history_is_not_granted() {
        // Fresh install — no record on disk, no grant in the live
        // status. Red dot.
        assert_eq!(
            classify_health(PermissionStatus::Denied, None),
            PermissionHealth::NotGranted
        );
        assert_eq!(
            classify_health(PermissionStatus::NotDetermined, None),
            PermissionHealth::NotGranted
        );
    }

    #[test]
    fn classify_not_applicable_passes_through() {
        // Linux / Windows builds — the type stays usable without
        // forcing the frontend to special-case platform.
        assert_eq!(
            classify_health(PermissionStatus::NotApplicable, None),
            PermissionHealth::NotApplicable
        );
        assert_eq!(
            classify_health(
                PermissionStatus::NotApplicable,
                Some("2026-04-01T10:00:00Z"),
            ),
            PermissionHealth::NotApplicable
        );
    }

    #[test]
    fn evaluate_combines_three_permissions_with_independent_history() {
        // End-to-end: mic granted (no history needed), screen
        // recording stale (history but preflight-false), input
        // monitoring not granted. The composed struct should
        // carry each verdict independently.
        let statuses = PermissionStatuses {
            microphone: PermissionStatus::Granted,
            screen_recording: PermissionStatus::Denied,
            input_monitoring: PermissionStatus::NotDetermined,
        };
        let health = evaluate_permissions_health(statuses, Some("2026-04-30T14:00:00Z"), None);
        assert_eq!(health.microphone, PermissionHealth::Confirmed);
        assert_eq!(health.screen_recording, PermissionHealth::Stale);
        assert_eq!(health.input_monitoring, PermissionHealth::NotGranted);
    }
}
