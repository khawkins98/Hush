//! macOS-only IPC commands (#82 extraction).
//!
//! Three commands all gated on `cfg(target_os = "macos")` for the
//! interesting branch, with non-macOS fallthroughs that return
//! "not applicable" so the frontend doesn't have to platform-
//! branch every call site:
//!
//! - [`open_macos_privacy_pane`] launches `System Settings →
//!   Privacy & Security → <pane>` via the canonical
//!   `x-apple.systempreferences:` URL scheme. Whitelists the pane
//!   targets so the command can't be pivoted into an arbitrary
//!   `open` shell.
//! - [`diagnose_macos_permissions`] returns bundle id + recovery
//!   hints + live grant state from
//!   [`crate::macos_perms::read_all`]. Side-effect-free; doesn't
//!   trigger OS prompts.
//! - [`reset_macos_permissions`] runs `tccutil reset` for the
//!   three TCC categories Hush touches (Microphone,
//!   ListenEvent / Input Monitoring, Accessibility).
//!
//! Extracted from `commands/mod.rs` under #82 to give the macOS
//! permissions surface its own module — already cfg-gated by
//! platform, with its own result types
//! (`MacosPermissionDiagnostic`, `MacosPermissionResetResult`)
//! that travel with the commands cleanly.

use serde::Serialize;

// `IpcError` is only referenced inside `#[cfg(target_os = "macos")]`
// blocks; the non-macOS Linux/Windows compilations don't need the
// name in scope. Gate the import accordingly so clippy's
// `unused-imports` lint stays clean across CI targets.
#[cfg(target_os = "macos")]
use super::IpcError;
use super::IpcResult;

/// Open the macOS System Settings pane the user needs to grant
/// the named permission. Tauri's shell plugin can launch arbitrary
/// URLs but its capability config requires us to whitelist URL
/// schemes — `x-apple.systempreferences:` isn't on the default
/// list. Routing through this command instead lets us pre-vet the
/// targets (a small enum of known panes) and keeps the capabilities
/// surface minimal.
///
/// On non-macOS platforms this is a no-op that returns `Ok(())`,
/// since the frontend's welcome modal is already gated on
/// `target_os = "macos"`. The fallthrough avoids a `cfg`-based
/// command-not-found error if the frontend ever calls this on the
/// wrong platform.
#[tauri::command]
pub async fn open_macos_privacy_pane(target: String) -> IpcResult<()> {
    #[cfg(target_os = "macos")]
    {
        // Whitelisted targets — anything else gets rejected so a
        // misbehaving frontend can't pivot this into an arbitrary
        // command launcher.
        let url = match target.as_str() {
            "microphone" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            "input-monitoring" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            "screen-recording" => {
                // Screen & System Audio Recording pane — the one that
                // governs ScreenCaptureKit (system-audio capture in
                // meeting mode). Surfaces stale rows after a
                // `tccutil reset` so the user can `-` them out.
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            "accessibility" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            other => {
                // Frontend sent a non-whitelisted target — this is
                // a protocol bug, not a user-configurable setting,
                // so surface as `Internal` (not `Settings`).
                return Err(IpcError::Internal(format!(
                    "unknown privacy pane target: {other:?}"
                )));
            }
        };

        // `open` is the macOS canonical "launch by URL scheme"
        // command; it Just Works for `x-apple.systempreferences:`.
        // No shell injection risk because the URL is a hard-coded
        // string keyed by a whitelisted enum.
        std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| IpcError::Internal(format!("open System Settings: {e}")))?;

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        // No-op on Linux / Windows so the frontend doesn't have to
        // branch by platform — the welcome modal that calls this is
        // already macOS-only, and a stray invoke from the wrong
        // platform should fail soft.
        let _ = target;
        Ok(())
    }
}

/// Touch ScreenCaptureKit so macOS adds Hush to the Screen
/// Recording permission list (and fires the standard TCC prompt
/// if not yet determined). Called from the Permissions tab's
/// "Grant in Settings…" button on the Screen Recording row,
/// immediately before deep-linking to System Settings.
///
/// Without this priming step, a user who hasn't yet started a
/// Meeting Mode session lands in the Screen & System Audio
/// Recording pane only to find Hush isn't listed — macOS only
/// enrolls an app once it actively requests the permission.
/// `audio::prime_screen_recording_permission` calls
/// `SCShareableContent::get()` and discards the result; the side
/// effect is that the Hush row appears in the list.
///
/// No-op on non-macOS. Errors at the SCK layer (rare on a healthy
/// system) surface as `IpcError::Internal` — but since the
/// "permission denied" case is the very state we're priming, the
/// underlying helper swallows it and returns `Ok(())`.
#[tauri::command]
pub async fn prime_screen_recording_permission() -> IpcResult<()> {
    #[cfg(target_os = "macos")]
    {
        crate::audio::prime_screen_recording_permission()
            .map_err(|e| IpcError::Internal(format!("prime SCK permission: {e:#}")))?;
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// Bundle identifier this binary registers with macOS TCC. Hard-coded
/// because `tauri.conf.json`'s `identifier` is the source of truth and
/// reading it back through `AppHandle::config().identifier()` would
/// require platform conditional plumbing for what is effectively a
/// constant string. If the bundle id ever changes, this constant and
/// the `tauri.conf.json` field move together.
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_ID: &str = "com.khawkins.hush";

/// What [`diagnose_macos_permissions`] returns to the frontend.
///
/// Snapshot of the bundle id, human-readable recovery hints, and (as
/// of #166) the live grant state of each TCC permission Hush touches.
/// The grant state comes from
/// [`crate::macos_perms::read_all`], which uses
/// `AVCaptureDevice.authorizationStatusForMediaType:` (mic),
/// `CGPreflightScreenCaptureAccess()` (screen recording), and
/// `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)` (input
/// monitoring). All three are side-effect-free reads — calling them
/// does NOT trigger the OS prompt.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosPermissionDiagnostic {
    /// The bundle id macOS uses to key TCC entries against this binary.
    /// Stable for the signed-bundle path; on unsigned dev builds TCC
    /// may instead key on the binary hash, which is why a `tccutil
    /// reset … <bundle_id>` can return "no entry" — see
    /// `docs/macos-permissions.md` for the full picture.
    pub bundle_id: String,
    /// Human-readable hint about how to verify Microphone access.
    /// Stays even when `statuses.microphone == Granted` so the user
    /// has the recovery copy if they later need to reset.
    pub microphone_hint: String,
    /// Human-readable hint about Input Monitoring (PTT). PTT is on
    /// by default everywhere (#194 — fufesou's `rdev` fork fixed
    /// the macOS-26 TSM abort, so the listener can spawn cleanly).
    /// macOS prompts the first time the listener spawns. Disable
    /// in Settings → General → Hotkeys if the prompt isn't wanted.
    pub input_monitoring_hint: String,
    /// Whether the running platform supports the in-app reset action.
    /// True only on macOS — `reset_macos_permissions` is a no-op
    /// elsewhere. The frontend uses this to decide whether to show
    /// the Reset button at all.
    pub can_reset: bool,
    /// Live grant state for each TCC permission. Drives the green /
    /// yellow indicator pills in the Settings → Permissions tab and
    /// the Dictation-tab status hint.
    pub statuses: crate::macos_perms::PermissionStatuses,
}

/// Best-effort diagnostic snapshot for the macOS permission story.
///
/// Returns immediately on every platform. On non-macOS, returns hints
/// that explain there's nothing to diagnose; on macOS, returns the
/// bundle id and the recovery copy. Does not probe Microphone or
/// Input Monitoring directly — both probes have the side effect of
/// triggering OS prompts, which we don't want a passive diagnostic to
/// do.
///
/// Pairs with [`reset_macos_permissions`]: the diagnostic is the
/// "what do I see?" half; the reset is the "click here to fix it"
/// half. See `docs/macos-permissions.md` for the manual recipe this
/// in-app surface wraps.
#[tauri::command]
pub async fn diagnose_macos_permissions() -> IpcResult<MacosPermissionDiagnostic> {
    let statuses = crate::macos_perms::read_all();

    #[cfg(target_os = "macos")]
    {
        Ok(MacosPermissionDiagnostic {
            bundle_id: MACOS_BUNDLE_ID.to_owned(),
            microphone_hint: "Click Start recording to verify. macOS prompts the first \
                 time Hush opens an audio stream; if no prompt appears and the meter \
                 never moves, Microphone access is denied. Use Reset below to re-prompt \
                 cleanly. Hush will appear in the Microphone list under \
                 \"com.khawkins.hush\" the first time you click Start (or under the \
                 launching binary for unsigned dev builds)."
                .to_owned(),
            input_monitoring_hint:
                "Required for push-to-talk. PTT is on by default; macOS prompts the \
                 first time the listener spawns. Disable in Settings → General → \
                 Hotkeys if you'd rather skip the prompt — the toggle hotkey \
                 (⌃⌥H) and the on-screen Start button work either way. Hush will \
                 appear in the Input Monitoring list under \"com.khawkins.hush\" \
                 once the listener has spawned at least once (or under the \
                 launching binary for unsigned dev builds — see CLAUDE.md's \
                 \"macOS TCC dev-binary quirk\" section)."
                    .to_owned(),
            can_reset: true,
            statuses,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(MacosPermissionDiagnostic {
            bundle_id: String::new(),
            microphone_hint: "Microphone permission is handled by your platform's audio stack \
                 (PulseAudio / PipeWire on Linux, Privacy on Windows). The in-app \
                 diagnostic is macOS-only."
                .to_owned(),
            input_monitoring_hint: "Input Monitoring is a macOS concept; not applicable here."
                .to_owned(),
            can_reset: false,
            statuses,
        })
    }
}

/// What [`reset_macos_permissions`] returns. The string is a one-line
/// summary suitable for showing in the UI as a confirmation banner.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosPermissionResetResult {
    /// True if at least one TCC entry was reset; false if every
    /// `tccutil reset` returned "no entry" (the unsigned-dev-binary
    /// case where TCC isn't keying on the bundle id at all).
    pub any_reset: bool,
    /// One-line user-facing message — populated either way.
    pub summary: String,
}

/// Run the four `tccutil reset` commands documented in
/// `docs/macos-permissions.md` for `com.khawkins.hush`. Microphone,
/// Screen Recording (`ScreenCapture` — system-audio capture for
/// meeting mode), Input Monitoring (`ListenEvent`), and
/// Accessibility are all reset; each is independent and a
/// missing-entry on any one is treated as a soft success (the
/// entry never existed to reset).
///
/// On non-macOS this is a no-op that reports "not applicable".
///
/// The reset takes effect on the *next* launch — the running process
/// keeps any grants it already had. The summary copy spells out the
/// follow-up: quit + relaunch, and if a stale row persists in
/// System Settings (older signing identity, etc.) the `−` button
/// at the bottom of that pane removes it cleanly.
#[tauri::command]
pub async fn reset_macos_permissions() -> IpcResult<MacosPermissionResetResult> {
    #[cfg(target_os = "macos")]
    {
        // ScreenCapture was previously missing from this list — a
        // real bug, not just polish: hitting Reset wouldn't actually
        // clear the Screen Recording grant, so users iterating on
        // dev builds saw stale "GRANTED" rows survive a reset.
        let categories: [&str; 4] = [
            "Microphone",
            "ScreenCapture",
            "ListenEvent",
            "Accessibility",
        ];
        let mut any_reset = false;
        for cat in categories {
            let status = std::process::Command::new("tccutil")
                .arg("reset")
                .arg(cat)
                .arg(MACOS_BUNDLE_ID)
                .status()
                .map_err(|e| IpcError::Internal(format!("run tccutil reset {cat}: {e}")))?;
            // `tccutil reset` exits 0 on a real reset and non-zero
            // on "no entry to reset". The latter is a soft success
            // for our purposes (the user wanted the slate clean).
            if status.success() {
                any_reset = true;
            }
        }
        let summary = if any_reset {
            "Reset complete. Quit and reopen Hush so macOS re-prompts on next \
             use. If a stale Hush.app row still appears in System Settings → \
             Privacy & Security (older signing identity from a previous build), \
             select it and click the − button at the bottom of that pane to \
             remove it — the next prompt will then create a fresh entry that \
             matches the current build."
                .to_owned()
        } else {
            "No TCC entries to reset (the bundle id may not be registered, common on \
             unsigned dev builds). If permissions still feel stuck, build a signed \
             bundle (`npm run tauri build`) and try its first launch."
                .to_owned()
        };
        Ok(MacosPermissionResetResult { any_reset, summary })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(MacosPermissionResetResult {
            any_reset: false,
            summary: "Permission reset is macOS-only (TCC is an Apple framework).".to_owned(),
        })
    }
}
