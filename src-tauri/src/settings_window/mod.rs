//! Standalone Settings window — Phase 2 of the IA redesign.
//!
//! Symmetric with [`crate::hud`] in shape: declares a label
//! constant, a `show()` helper that locates the window via
//! `app.get_webview_window` and makes it visible/focused, and a
//! `hide()` helper for symmetry. The window itself is configured
//! in `tauri.conf.json` (label `settings`, hidden by default) so
//! its lifecycle stays declarative.
//!
//! ## Why a second top-level window
//!
//! macOS HIG: settings live in a dedicated window opened from
//! `App → Settings…` and bound to ⌘,. The post-Ventura "Settings"
//! rename (was "Preferences") and the toolbar-tabs convention are
//! the strong defaults; nesting settings inside the main window
//! breaks user muscle memory and the ⌘, accelerator. Phase 3 lifts
//! the existing Model / Vocabulary / Replacements / Permissions
//! panels here; this PR ships the shell so that move is a clean
//! rename + path-update rather than "introduce a window AND move
//! ten components in one PR."
//!
//! ## Show/focus policy
//!
//! - **Show + focus** when the user picks `Hush → Settings…` from
//!   the menu (⌘,) or invokes [`crate::ipc::commands::open_settings`].
//! - **Hide** when the user closes it. The window's `visible: false`
//!   default means a fresh app launch never flashes the settings
//!   surface; `show()` is the only path that makes it appear.
//! - Re-opening an already-visible window calls `set_focus()` so
//!   the existing window comes forward rather than spawning a
//!   second one.

use anyhow::{Context, Result};
use tauri::{AppHandle, Manager, Runtime};

/// Window label that matches the `tauri.conf.json` `windows[].label`.
/// Centralised so a typo in one call site doesn't silently miss the
/// settings window.
pub const SETTINGS_LABEL: &str = "settings";

/// Show the Settings window. If it's already visible, brings it to
/// the front instead of doing nothing (matches macOS Settings
/// behavior — opening Settings while it's already open focuses it).
///
/// Best-effort: a missing window is logged and returns Ok rather
/// than failing whatever invoked the open. The settings surface is
/// secondary; the main window staying responsive matters more.
pub fn show<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    match app.get_webview_window(SETTINGS_LABEL) {
        Some(window) => {
            window.show().context("show settings window")?;
            window.set_focus().context("focus settings window")?;
        }
        None => {
            tracing::error!(
                label = SETTINGS_LABEL,
                "Settings window not found; check tauri.conf.json"
            );
        }
    }
    Ok(())
}

/// Hide the Settings window. Symmetric with [`show`]; same
/// graceful degradation if the window is missing.
pub fn hide<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if let Some(window) = app.get_webview_window(SETTINGS_LABEL) {
        window.hide().context("hide settings window")?;
    }
    Ok(())
}
