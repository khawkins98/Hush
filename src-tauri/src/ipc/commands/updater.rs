//! Auto-update install IPC commands (#10).
//!
//! Step 5 of the implementation plan in `crate::updater::mod` —
//! the user-confirmation install path. The companion manual
//! "check for updates" probe lives in `commands::system::check_for_updates`
//! and ships independently; this module wraps
//! `tauri-plugin-updater`'s background-update lifecycle.
//!
//! ## Activation status
//!
//! As of this PR the plugin is **not yet registered** in
//! `lib.rs::run` (Step 4) and the `plugins.updater` block is
//! **not yet present** in `tauri.conf.json` (Step 2). Both gates
//! need maintainer-only actions first — see
//! `crate::updater::mod`'s "Implementation plan for #10" section
//! for the exact one-time steps (signing keypair, CI secrets,
//! conf block).
//!
//! Until those land, calling [`install_pending_update`] returns
//! [`IpcError::Internal`] with a clear message ("auto-update is
//! not configured for this build") because `app.updater()` errors
//! when the plugin isn't registered. The frontend's About-tab
//! Install button reads the same gate and stays disabled / hidden
//! when the plugin is unavailable, so the IPC error path is the
//! belt-and-braces fallback rather than a user-facing surface.

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

use super::{IpcError, IpcResult};

/// Wire-format payload for the `updater:download-progress` event.
/// The plugin invokes our progress callback once per chunk; we
/// forward to the frontend as a typed JSON object so the About-
/// tab progress bar can render bytes-received-of-total.
///
/// `chunk_len` is the **delta** for this event — the bytes added
/// since the previous progress callback, not a running total.
/// The frontend accumulates locally to render the progress bar.
/// Named explicitly to avoid the "downloaded" reading which
/// suggests a cumulative count.
///
/// `total` is `Option<u64>` because the upstream archive may not
/// declare a Content-Length (chunked transfer / unknown size).
/// The UI treats `None` as "indeterminate" — spinner instead of
/// percentage bar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterDownloadProgress {
    pub chunk_len: u64,
    pub total: Option<u64>,
}

/// Wire-format payload for the `updater:install-pending` event.
/// Fired exactly once after the download completes and before the
/// install begins, so the UI can swap from "Downloading…" to
/// "Installing…" before the app relaunches and the renderer dies.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterInstallPending {
    /// Human-readable version we're installing — surfaced in the
    /// final toast / status pill so the user has confirmation of
    /// what's about to launch.
    pub version: String,
}

/// Tauri event names emitted during the install flow. Centralised
/// here so the frontend's listener (`Events.UpdaterDownloadProgress`,
/// `Events.UpdaterInstallPending`) and the backend's emit sites
/// can't drift.
const EVENT_DOWNLOAD_PROGRESS: &str = "updater:download-progress";
const EVENT_INSTALL_PENDING: &str = "updater:install-pending";

/// Download + verify + install the pending update.
///
/// User-driven: the About-tab Install button calls this after the
/// manual "Check for updates" probe has reported
/// `kind: "updateAvailable"`. The plugin runs the download on its
/// own task, reports progress through a callback we forward to
/// `updater:download-progress`, fires `updater:install-pending`
/// when bytes are on disk + verified, then triggers the relaunch.
/// On macOS the relaunch may surface a Gatekeeper dialog (Hush
/// ships unsigned today) — see the About-tab UI for the user
/// notice.
///
/// Errors:
/// - Plugin not registered (Steps 1–4 of the spec not yet done):
///   `IpcError::Internal("auto-update is not configured for this build")`.
/// `expected_version` is the version string the user agreed to
/// install — typically the `latest` field of the
/// `UpdateCheckResult` the manual probe returned and the AboutTab
/// rendered. The IPC compares it against the version the plugin's
/// own `check()` resolves to and refuses to install on a mismatch
/// (TOCTOU defence): the user's consent was for X, but a release
/// rotated to Y between Check and Install would otherwise install
/// Y silently. Pass `None` to skip the version check (preserves
/// the pre-#497 behaviour for callers that don't track a version
/// pre-click).
///
/// - No update available at install time (race with the manual
///   probe — between "check" and "install" the GitHub release
///   could have been replaced or the user's version bumped via a
///   manual install): returns Ok — the relaunch step is skipped
///   silently. The frontend re-runs the check on next mount.
/// - **Version mismatch** (the plugin resolves a different version
///   than the user agreed to): `IpcError::Internal(<msg>)`. The
///   frontend re-fetches the check on retry so the user sees the
///   new version and can re-confirm.
/// - Network / signature / install-write failure:
///   `IpcError::Internal(<plugin error chain>)`. The user can
///   retry or fall back to the "Open release notes" manual link.
#[tauri::command]
pub async fn install_pending_update(
    app: AppHandle,
    expected_version: Option<String>,
) -> IpcResult<()> {
    let updater = app.updater().map_err(|_| IpcError::UpdaterUnavailable)?;

    let maybe_update = updater
        .check()
        .await
        .map_err(|e| IpcError::Internal(format!("update check failed: {e}")))?;

    let Some(update) = maybe_update else {
        // Race with the manual probe — the user clicked Install
        // but the version we'd offered has been superseded /
        // withdrawn. Returning Ok lets the frontend reset its UI
        // to "up to date" on the next refresh; surfacing this as
        // an error would be noisier than the situation deserves.
        return Ok(());
    };

    if let Some(expected) = expected_version.as_deref() {
        if update.version != expected {
            return Err(IpcError::Internal(format!(
                "update version mismatch: you agreed to install {expected}, \
                 but the latest is now {} — please re-check",
                update.version
            )));
        }
    }

    let version = update.version.clone();
    let app_for_progress = app.clone();

    update
        .download_and_install(
            move |chunk_len, total| {
                let payload = UpdaterDownloadProgress {
                    chunk_len: chunk_len as u64,
                    total,
                };
                if let Err(e) = app_for_progress.emit(EVENT_DOWNLOAD_PROGRESS, &payload) {
                    tracing::warn!(error = ?e, "updater: emit download-progress failed");
                }
            },
            move || {
                let payload = UpdaterInstallPending {
                    version: version.clone(),
                };
                if let Err(e) = app.emit(EVENT_INSTALL_PENDING, &payload) {
                    tracing::warn!(error = ?e, "updater: emit install-pending failed");
                }
            },
        )
        .await
        .map_err(|e| IpcError::Internal(format!("update install failed: {e}")))?;

    // App relaunches automatically once `download_and_install`
    // returns Ok; this line is reached only on the successful
    // path before the relaunch interrupts execution.
    Ok(())
}
