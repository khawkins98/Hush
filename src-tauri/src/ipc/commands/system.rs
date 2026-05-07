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

/// Wire shape for [`get_log_dir`] (#622-followup). `None` means
/// "no on-disk log file is being written" — the file appender
/// either isn't supported on this platform (non-macOS) or was
/// disabled at startup via `HUSH_LOG_FILE=off`. The Debug tab uses
/// this to decide whether to surface the reveal-in-Finder controls
/// or hide the section.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogDirInfo {
    /// Absolute path to the daily-rolling log directory.
    pub dir: String,
    /// Filename of *today's* log file (e.g. `hush.log.2026-05-07`).
    /// Computed here so the frontend doesn't need to keep a date
    /// parser in sync with `tracing-appender`'s rotation suffix.
    pub today_file: String,
}

/// Resolve the on-disk log directory if file logging is engaged for
/// this process. Mirrors the resolution logic used by `init_tracing`
/// in `lib.rs` so the Debug tab points at exactly what's being
/// written. macOS-only by design (matches `resolve_log_dir`).
///
/// Reads `HUSH_LOG_FILE` directly so a user who disabled file
/// logging gets `None` and the Debug tab's "Open log dir" controls
/// stay hidden — better than showing a path to a non-existent file.
#[tauri::command]
pub fn get_log_dir() -> IpcResult<Option<LogDirInfo>> {
    // Honour the same opt-out env var as `init_tracing`. If file
    // logging was off at process start, the directory may not even
    // exist; surfacing it would be misleading.
    let want_file_log = !matches!(
        std::env::var("HUSH_LOG_FILE").as_deref(),
        Ok(v) if v.eq_ignore_ascii_case("off") || v == "0"
    );
    if !want_file_log {
        return Ok(None);
    }
    let Some(dir) = crate::resolve_log_dir() else {
        return Ok(None);
    };
    // tracing-appender's daily rotator uses a UTC-date suffix in
    // the form `hush.log.YYYY-MM-DD`. Match that so the user can
    // tail / grep today's file without having to figure out the
    // naming convention. UTC is what the rotator uses internally,
    // so this stays correct across timezone boundaries.
    let today = chrono_today_utc();
    Ok(Some(LogDirInfo {
        dir: dir.to_string_lossy().into_owned(),
        today_file: format!("hush.log.{today}"),
    }))
}

/// Get today's date in UTC as `YYYY-MM-DD`. Hand-rolled to avoid
/// pulling `chrono` just for this — `time` would also work but
/// neither is currently a dep. The format matches
/// `tracing_appender::rolling::Rotation::DAILY`'s suffix.
fn chrono_today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Days since 1970-01-01 (a Thursday).
    let days = secs.div_euclid(86_400);
    // Convert to civil date (Howard Hinnant's algorithm — public
    // domain, exact, no leap-second weirdness because UTC days are
    // 86_400 s by convention).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp.wrapping_sub(9) }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
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

/// Version + build timestamp bundled together for the debug surfaces.
///
/// `buildTimestamp` is Unix seconds set by `build.rs` at compile time
/// via `HUSH_BUILD_TIMESTAMP`. The frontend formats it as
/// `DD/MM/YYYY HH:MM` in local time.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: String,
    /// Unix seconds at compile time. 0 when the build stamp is unavailable.
    pub build_timestamp: u64,
}

#[tauri::command]
pub fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_timestamp: env!("HUSH_BUILD_TIMESTAMP").parse().unwrap_or(0),
    }
}

/// One per-phase entry in the startup timing trace (#584 Angle 1).
///
/// `elapsed_ms` is the absolute milliseconds since `build_default`
/// started — same scale the existing `tracing::info!` lines use, so
/// reading the values here matches what you'd see in the log. The
/// gap between consecutive phases gives the wall time of that phase.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupPhase {
    /// Human-readable phase name. Stable enough for the frontend to
    /// pin in tests; not a stable wire token, so renaming is a
    /// pure-prose change.
    pub name: String,
    /// Milliseconds elapsed since `build_default` started.
    pub elapsed_ms: u64,
}

/// Return the list of startup-phase timings captured during
/// `AppState::build_default` (#584 Angle 1).
///
/// The list is populated once at boot and held read-only on
/// `AppState`. Empty in `--no-default-features` builds where some
/// phases are skipped, but otherwise the same shape regardless of
/// platform. Used by the Debug tab to surface a per-phase trace
/// without the user having to grep the log.
#[tauri::command]
pub fn get_startup_timings(state: State<'_, AppState>) -> Vec<StartupPhase> {
    state.startup_timings.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrono_today_utc_format_matches_tracing_appender() {
        // Pin the format `YYYY-MM-DD` so the frontend's grep
        // suggestion ("hush.log.<today>") matches the actual file
        // tracing-appender writes. A drift here would silently
        // surface a wrong filename in the Debug tab.
        let today = chrono_today_utc();
        assert_eq!(today.len(), 10, "expected YYYY-MM-DD; got {today:?}");
        assert_eq!(today.chars().nth(4), Some('-'));
        assert_eq!(today.chars().nth(7), Some('-'));
        // All-numeric except the dashes.
        for (i, c) in today.chars().enumerate() {
            if i == 4 || i == 7 {
                continue;
            }
            assert!(c.is_ascii_digit(), "non-digit at {i} in {today:?}");
        }
    }

    #[test]
    fn get_log_dir_returns_none_when_disabled() {
        // HUSH_LOG_FILE=off must keep the Debug tab from advertising
        // a path the user disabled. Save+restore the env var so
        // parallel tests aren't poisoned.
        let saved = std::env::var("HUSH_LOG_FILE").ok();
        // Safety: tests in this module run in the same process; the
        // remove + restore window is short and no other test reads
        // the var. See the same idiom in `transcription::whisper`'s
        // env-var tests.
        unsafe { std::env::set_var("HUSH_LOG_FILE", "off") };
        let result = get_log_dir();
        match saved {
            Some(prev) => unsafe { std::env::set_var("HUSH_LOG_FILE", prev) },
            None => unsafe { std::env::remove_var("HUSH_LOG_FILE") },
        }
        assert!(matches!(result, Ok(None)), "got {result:?}");
    }
}
