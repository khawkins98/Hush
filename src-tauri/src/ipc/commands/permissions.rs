//! Permission-related IPC commands (#82 extraction; renamed from
//! `macos.rs` under #597).
//!
//! Today every command here is macOS-specific in implementation —
//! the file was renamed in advance of Linux (#106) and Windows
//! (#107) impls so cross-platform permission code has a home.
//! Commands are gated on `cfg(target_os = "macos")` for the
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
//!   [`crate::permissions::read_all`]. Side-effect-free; doesn't
//!   trigger OS prompts.
//! - [`reset_macos_permissions`] runs `tccutil reset` for the
//!   three TCC categories Hush actually touches (Microphone,
//!   ScreenCapture, ListenEvent / Input Monitoring).
//!   Accessibility was previously included but Hush never
//!   requests it (#273).
//! - [`prime_screen_recording_permission`] is now a no-op (#600):
//!   system-audio capture no longer uses ScreenCaptureKit, so
//!   Screen Recording TCC is not required.  Kept for frontend
//!   call-site compatibility while the frontend is being updated.
//! - [`relaunch_app`] calls `AppHandle::restart()`. Exposed as an
//!   IPC so the frontend relaunch banner can trigger it with a
//!   single `invoke`.
//!
//! Extracted from `commands/mod.rs` under #82 to give the
//! permissions surface its own module — already cfg-gated by
//! platform, with its own result types
//! (`MacosPermissionDiagnostic`, `MacosPermissionResetResult`)
//! that travel with the commands cleanly.

use serde::Serialize;
use tauri::State;

use crate::ipc::AppState;

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
                // Legacy Screen & System Audio Recording grant from builds before
                // #600 (when system audio used ScreenCaptureKit). Kept in the
                // reset list so old grants are cleaned up; Hush no longer
                // requires this permission for meeting mode.
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            // "accessibility" target intentionally absent — Hush
            // doesn't request Accessibility (#273). Removed from
            // the whitelist here so a stale frontend can't deep-
            // link the user to a pane that will never list Hush.
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

/// Touch ScreenCaptureKit so macOS adds Hush to the System Audio
/// permission list (and fires the standard TCC prompt if not yet
/// determined). Called from the Permissions tab's "Grant in
/// Settings…" button on the System Audio row, immediately before
/// deep-linking to System Settings.
///
/// **No-op since #600.** System-audio capture now uses
/// `AudioHardwareCreateProcessTap` which does not require Screen
/// Recording TCC on macOS 26+.  This command is kept registered so
/// any frontend call-sites that haven't been updated yet don't
/// produce a command-not-found error.
#[tauri::command]
pub async fn prime_screen_recording_permission(app: tauri::AppHandle) -> IpcResult<()> {
    let _ = app;
    Ok(())
}

/// Relaunch the app immediately via `AppHandle::restart()`.
///
/// Exposed as an IPC so the frontend's relaunch banner can trigger
/// it with a single `invoke("relaunch_app")`. The call does not
/// return — the process is replaced.
///
/// This is the correct fix after a System Audio (Screen Recording
/// TCC) grant: macOS caches the TCC deny in `mediaserverd` /
/// `coreaudiod` for the lifetime of the current process. Preflight
/// flipping true is not enough — only a fresh process sees the
/// grant take effect. The relaunch is unconditional once the user
/// confirms it in the UI.
#[tauri::command]
pub async fn relaunch_app(app: tauri::AppHandle) -> IpcResult<()> {
    app.restart();
}

/// Fire the macOS Microphone TCC prompt directly (#511).
///
/// Used by the first-run wizard's Allow button so the user can grant
/// inline without having to open System Settings. `AVCaptureDevice
/// requestAccessForMediaType:completionHandler:` shows the system
/// dialog and returns immediately; the user's choice surfaces via
/// the existing `get_permission_health` poll the frontend already
/// runs against the Settings → Permissions tab — same poll cadence,
/// different consumer.
///
/// No-op on non-macOS (Linux + Windows don't have an equivalent
/// per-app TCC layer for raw mic access). Always returns `Ok(())`
/// — the frontend treats this as a fire-and-forget and reads the
/// post-prompt state through the health probe.
#[tauri::command]
pub async fn request_microphone_permission() -> IpcResult<()> {
    crate::permissions::request_microphone_permission();
    Ok(())
}

/// Fire the macOS Input Monitoring TCC prompt synchronously (#511).
///
/// Same first-run-wizard motivation as
/// [`request_microphone_permission`], but `IOHIDRequestAccess`
/// blocks until the user responds — so this IPC awaits the user's
/// choice and returns the result. The frontend reflects the new
/// state in the same poll loop without needing to wait on the
/// return value, but having it lets the wizard show an immediate
/// status flip when the user clicks Allow.
///
/// On a successful grant, **immediately starts the PTT listener** in
/// the current session so the user doesn't need to restart Hush to
/// use push-to-talk. This is the correct place to fire the rdev
/// listener after a first-run grant: `lib.rs` no longer attempts the
/// listener for `NotDetermined` (doing so on macOS 26 causes an
/// auto-deny via CGEventTap), so this IPC owns the "first successful
/// grant → listener up" path.
///
/// Returns `true` when the user granted (or already-granted state
/// is observed without a prompt), `false` when denied / dismissed.
/// No-op on non-macOS — Linux + Windows don't have an Input
/// Monitoring TCC equivalent (the rdev listener works without a
/// per-app prompt there); always returns `Ok(true)` so the wizard's
/// success branch matches.
#[tauri::command]
pub async fn request_input_monitoring_permission(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<bool> {
    // Synchronous OS prompt — keep off the Tokio worker thread
    // so we don't tie up an async slot for the prompt's
    // duration. `spawn_blocking` is the standard escape hatch.
    let granted =
        tokio::task::spawn_blocking(crate::permissions::request_input_monitoring_permission)
            .await
            .map_err(|e| IpcError::Internal(format!("request IM permission task: {e}")))?;

    if granted {
        // IM just granted — start the PTT listener immediately so PTT
        // works in this session without a restart. `register_ptt_listener`
        // is idempotent (compare_exchange on the spawned latch), so calling
        // it here when it was already started at boot is a no-op.
        if let Err(e) = crate::hotkey::ptt::register_ptt_listener(
            &app,
            std::sync::Arc::clone(&state.ptt_combo),
            std::sync::Arc::clone(&state.ptt_active),
            std::sync::Arc::clone(&state.ptt_listener_spawned),
        ) {
            tracing::warn!(error = ?e, "PTT listener start after IM grant failed; restart may be needed");
        } else {
            tracing::info!("PTT listener started after IM grant");
        }
    }

    Ok(granted)
}

/// Bundle identifier this binary registers with macOS TCC. Hard-coded
/// because `tauri.conf.json`'s `identifier` is the source of truth and
/// reading it back through `AppHandle::config().identifier()` would
/// require platform conditional plumbing for what is effectively a
/// constant string. If the bundle id ever changes, this constant and
/// the `tauri.conf.json` field move together.
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_ID: &str = "io.github.khawkins98.hush";

/// What [`diagnose_macos_permissions`] returns to the frontend.
///
/// Snapshot of the bundle id, human-readable recovery hints, and (as
/// of #166) the live grant state of each TCC permission Hush touches.
/// The grant state comes from
/// [`crate::permissions::read_all`], which uses
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
    pub statuses: crate::permissions::PermissionStatuses,
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
    let statuses = crate::permissions::read_all();

    #[cfg(target_os = "macos")]
    {
        Ok(MacosPermissionDiagnostic {
            bundle_id: MACOS_BUNDLE_ID.to_owned(),
            microphone_hint: "Click Start recording to verify. macOS prompts the first \
                 time Hush opens an audio stream; if no prompt appears and the meter \
                 never moves, Microphone access is denied. Use Reset below to re-prompt \
                 cleanly. Hush will appear in the Microphone list under \
                 \"io.github.khawkins98.hush\" the first time you click Start (or under the \
                 launching binary for unsigned dev builds)."
                .to_owned(),
            input_monitoring_hint:
                "Required for push-to-talk. PTT is on by default; macOS prompts the \
                 first time the listener spawns. Disable in Settings → General → \
                 Hotkeys if you'd rather skip the prompt — the toggle hotkey \
                 (⌃⌥H) and the on-screen Start button work either way. Hush will \
                 appear in the Input Monitoring list under \"io.github.khawkins98.hush\" \
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

/// Three-state permission health snapshot (#378). Wraps
/// [`crate::permissions::PermissionsHealth`] and returns it from
/// the new [`get_permission_health`] IPC; the frontend uses the
/// per-permission verdicts to render Settings → Permissions
/// traffic-light rows + the small main-window status dot.
///
/// `PermissionsHealth` is itself serde-shaped, so this wrapper is
/// just a transport. Pulled into its own struct so the IPC return
/// type is a named struct rather than a bare alias — matches every
/// other macOS permissions command's shape.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionHealthResponse {
    pub health: crate::permissions::PermissionsHealth,
}

/// Read the three-state permission health (#378). Combines the
/// live OS preflight calls (cheap, no prompt) with the persisted
/// `last_confirmed` timestamps to disambiguate "never granted"
/// from "was granted, now stale".
///
/// Frontend calls this on Permissions-tab mount and on window
/// focus. The probe never prompts the user — preflight calls are
/// side-effect-free; the strongest confirmation comes from the
/// success path of `start_dictation` / SCK probe, which write
/// the `last_confirmed` timestamps from inside the Rust pipeline
/// (out of scope for this command).
#[tauri::command]
pub async fn get_permission_health(
    state: State<'_, AppState>,
) -> IpcResult<PermissionHealthResponse> {
    let statuses = crate::permissions::read_all();
    let screen_recording_last_confirmed = state
        .settings
        .get(crate::settings::keys::PERMISSIONS_SCREEN_RECORDING_LAST_CONFIRMED)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    let microphone_last_confirmed = state
        .settings
        .get(crate::settings::keys::PERMISSIONS_MICROPHONE_LAST_CONFIRMED)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;

    // `effective_screen_lc` may not change (screen_recording is always
    // NotApplicable now — no probe needed). `effective_mic_lc` is mutated
    // in the auto-confirm block below.
    let effective_screen_lc = screen_recording_last_confirmed.clone();
    let mut effective_mic_lc = microphone_last_confirmed.clone();
    if matches!(
        statuses.microphone,
        crate::permissions::PermissionStatus::Granted
    ) && microphone_last_confirmed.is_none()
    {
        match stamp_last_confirmed(
            &state,
            crate::settings::keys::PERMISSIONS_MICROPHONE_LAST_CONFIRMED,
        )
        .await
        {
            Ok(stamped) => {
                effective_mic_lc = Some(stamped);
            }
            Err(e) => {
                tracing::warn!(error = %e, "permission health: stamp microphone confirmed failed");
            }
        }
    }

    let health = crate::permissions::evaluate_permissions_health(
        statuses,
        effective_screen_lc.as_deref(),
        effective_mic_lc.as_deref(),
    );
    Ok(PermissionHealthResponse { health })
}

/// Write the current Unix-epoch-millis to a settings key, returning
/// the value that was written so the caller can keep its in-memory
/// view in sync without a re-read. Used both by the auto-confirm
/// path inside [`get_permission_health`] and the explicit
/// [`confirm_permission`] entry point.
async fn stamp_last_confirmed(state: &AppState, key: &str) -> anyhow::Result<String> {
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string();
    state
        .settings
        .set(key, &now_millis)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(now_millis)
}

/// Mark a permission as recently confirmed (#378). Called from the
/// frontend after a `start_dictation` (mic) or a successful
/// meeting start with system-audio (screen recording) — the
/// strongest possible signal that the underlying capability is
/// alive. Writes the current Unix-epoch-millis (decimal string)
/// to the persisted settings row keyed by the permission name.
/// `evaluate_permissions_health` only checks `Some/None` on the
/// row, so the wire format is opaque today; documenting it here
/// so a future migration that tries to parse doesn't trip over
/// the assumption.
///
/// The permission name argument is a stable string token rather
/// than the typed enum so the frontend can bind the call without
/// importing the Rust enum's serde shape — same pattern the rest
/// of the macOS commands use for path tokens.
#[tauri::command]
pub async fn confirm_permission(state: State<'_, AppState>, permission: String) -> IpcResult<()> {
    let key = match permission.as_str() {
        "screen-recording" => crate::settings::keys::PERMISSIONS_SCREEN_RECORDING_LAST_CONFIRMED,
        "microphone" => crate::settings::keys::PERMISSIONS_MICROPHONE_LAST_CONFIRMED,
        other => {
            // Argument-shape error rather than a settings-DB
            // failure: the frontend sent a token the contract
            // doesn't accept. Pre-#386 this returned
            // `IpcError::Settings` which `formatErrorDisplay`
            // renders as "Settings update failed" — wrong framing
            // for a frontend-bug class. `IpcError::Internal` is
            // the right "shouldn't happen at runtime" bucket.
            return Err(IpcError::Internal(format!(
                "unknown permission token {other:?} (expected 'screen-recording' or 'microphone')"
            )));
        }
    };
    stamp_last_confirmed(&state, key)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    Ok(())
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

/// Run the three `tccutil reset` commands documented in
/// `docs/macos-permissions.md` for `io.github.khawkins98.hush`: Microphone,
/// Screen Recording (`ScreenCapture` — system-audio capture for
/// meeting mode), and Input Monitoring (`ListenEvent`). Each is
/// independent and a missing-entry on any one is treated as a
/// soft success (the entry never existed to reset).
///
/// **Why no Accessibility reset (#273):** Hush's PTT path uses
/// `kIOHIDRequestTypeListenEvent` (the listen-only event tap,
/// covered by Input Monitoring), not the event-modification tap
/// that requires Accessibility. `Info.plist` has no
/// `NSAccessibilityUsageDescription` because the app legitimately
/// never asks for that permission. Resetting it was vestigial
/// noise from earlier prototypes — harmless but surprising in
/// `tccutil` output.
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
        // Accessibility was previously included but Hush never
        // requests it (#273); removed.
        let categories: [&str; 3] = ["Microphone", "ScreenCapture", "ListenEvent"];
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
