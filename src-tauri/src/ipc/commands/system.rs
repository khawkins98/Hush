//! System / lifecycle IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the cross-cutting
//! lifecycle commands (window-show, first-run flag, LaunchAgent
//! retry, manual update probe) sit in a peer file the way
//! `meeting.rs`, `models.rs`, and `dictionary.rs` already do.
//! No behaviour change — pure code-move.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::system::show_main_window`, etc.). `pub use`
//! re-exports do not carry the macro's hidden `__cmd__<name>`
//! symbol — see `learnings.md` 2026-04-25.

use tauri::{AppHandle, State};

use super::super::AppState;
use super::{poisoned, IpcError, IpcResult, UPDATE_CHECK_TTL};

/// Show + focus the main `"Hush"` window (#427 Item 1). Called by
/// the menu-bar quick popover's "Open Hush" link so the popover
/// can bring the main window forward without needing the broader
/// `core:window:allow-get-all-windows` JS permission. Best-effort:
/// a missing window or a `show()` / `set_focus()` failure logs a
/// warning and returns Ok — the user can still reach the main
/// window via the tray's "Show Hush" menu item.
#[tauri::command]
pub fn show_main_window(app: AppHandle) -> IpcResult<()> {
    use tauri::Manager as _;
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("show_main_window: main window not found");
        return Ok(());
    };
    if let Err(e) = window.show() {
        tracing::warn!(error = ?e, "show_main_window: show failed");
    }
    if let Err(e) = window.unminimize() {
        tracing::warn!(error = ?e, "show_main_window: unminimize failed");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = ?e, "show_main_window: set_focus failed");
    }
    Ok(())
}

/// Show + focus the floating debug-console window (declared in
/// `tauri.conf.json` with `"label": "debug"`, `visible: false`).
/// Called from the Settings → Debug tab when the developer console
/// is enabled. Opens it as a palette that floats above the main
/// window so the user can watch the live log while clicking around
/// the app.
#[tauri::command]
pub fn open_debug_window(app: AppHandle) -> IpcResult<()> {
    use tauri::Manager as _;
    let Some(window) = app.get_webview_window("debug") else {
        tracing::warn!("open_debug_window: debug window not found");
        return Ok(());
    };
    if let Err(e) = window.show() {
        tracing::warn!(error = ?e, "open_debug_window: show failed");
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = ?e, "open_debug_window: set_focus failed");
    }
    Ok(())
}

/// Returns whether the macOS first-run welcome has been shown and
/// dismissed for this install. The value is stored under
/// [`crate::settings::keys::FIRST_RUN_COMPLETED`] as the literal
/// string `"true"` once dismissed; any other state (including the
/// settings row being absent) reads as `false`.
#[tauri::command]
pub async fn get_first_run_completed(state: State<'_, AppState>) -> IpcResult<bool> {
    let value = state
        .settings
        .get(crate::settings::keys::FIRST_RUN_COMPLETED)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    Ok(value.as_deref() == Some("true"))
}

/// Persist that the user has dismissed the welcome modal. Idempotent;
/// calling twice is the same as once.
#[tauri::command]
pub async fn mark_first_run_completed(state: State<'_, AppState>) -> IpcResult<()> {
    state
        .settings
        .set(crate::settings::keys::FIRST_RUN_COMPLETED, "true")
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Clear the first-run-completed flag so the welcome modal renders
/// again on the next app launch. Used by the Settings → General
/// "Show welcome on next launch" affordance — useful for users
/// who dismissed the welcome too quickly and want to re-read the
/// permissions explainer.
#[tauri::command]
pub async fn reset_first_run(state: State<'_, AppState>) -> IpcResult<()> {
    state
        .settings
        .set(crate::settings::keys::FIRST_RUN_COMPLETED, "false")
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// LaunchAgent path-staleness flag (#317). #271's setup hook
/// re-registers the autostart plist with the current binary
/// path on every launch where autostart is enabled — but if
/// `enable()` fails (read-only home, fs permission issue) the
/// LaunchAgent still points at whatever path it had before, and
/// the user gets no signal. This IPC + the retry below give
/// Settings → General a way to surface the failure and let the
/// user trigger another attempt.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartPathStatus {
    /// True if `lib.rs::run`'s post-#271 re-register hit an
    /// error. False on every other path (autostart not enabled,
    /// re-register succeeded, or non-macOS where the flag is
    /// always false because the re-register block is gated to
    /// macOS).
    pub stale: bool,
}

#[tauri::command]
pub fn get_autostart_path_status(state: State<'_, AppState>) -> IpcResult<AutostartPathStatus> {
    Ok(AutostartPathStatus {
        stale: state
            .runtime_flags
            .autostart_path_stale
            .load(std::sync::atomic::Ordering::Relaxed),
    })
}

/// Retry the LaunchAgent re-register that failed at boot (#317).
/// Returns `true` if the retry succeeded (and clears the stale
/// flag so subsequent `get_autostart_path_status` calls see the
/// cleaner state); returns `false` if the retry also failed.
///
/// Settings → General's "Click to update" button calls this when
/// the user wants to retry without restarting the app.
#[tauri::command]
pub fn retry_autostart_registration(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<bool> {
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_autostart::ManagerExt;
        let mgr = app.autolaunch();
        match mgr.enable() {
            Ok(()) => {
                state
                    .runtime_flags
                    .autostart_path_stale
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    "autostart: retry_autostart_registration succeeded; LaunchAgent path is now current"
                );
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "autostart: retry_autostart_registration failed; flag stays set"
                );
                Ok(false)
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        let _ = state;
        Ok(true)
    }
}

/// Manual "Check for updates" probe (#223). Calls
/// [`crate::updater::check_for_updates`] against the app's shared
/// HTTP client; the result drives an in-app dialog.
///
/// Caches the last successful result for [`UPDATE_CHECK_TTL`]
/// (#333) so a spam-clicking user or a shared-IP environment
/// (corporate NAT, family Wi-Fi with multiple installs) can't burn
/// the unauthenticated-GitHub rate limit. Auto-update is the
/// separate [#10] follow-up.
///
/// [#10]: https://github.com/khawkins98/Hush/issues/10
#[tauri::command]
pub async fn check_for_updates(
    state: State<'_, AppState>,
) -> IpcResult<crate::updater::UpdateCheckResult> {
    check_for_updates_inner(&state, std::time::Instant::now()).await
}

/// Inner implementation that takes the current instant explicitly
/// so unit tests can pin time without an actual sleep. The IPC
/// command always passes `Instant::now()`.
pub(crate) async fn check_for_updates_inner(
    state: &AppState,
    now: std::time::Instant,
) -> IpcResult<crate::updater::UpdateCheckResult> {
    {
        let cached = state.last_update_check.lock().map_err(poisoned)?;
        if let Some((at, result)) = cached.as_ref() {
            if now.duration_since(*at) < UPDATE_CHECK_TTL {
                return Ok(result.clone());
            }
        }
    }
    let fresh = crate::updater::check_for_updates(&state.http).await?;
    *state.last_update_check.lock().map_err(poisoned)? = Some((now, fresh.clone()));
    Ok(fresh)
}

/// Return the running Hush version string, sourced from `Cargo.toml`.
///
/// Used by the Debug tab's issue-report generator so the report
/// always includes the correct version without a separate IPC round.
#[tauri::command]
pub fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
