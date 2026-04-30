//! Native macOS menu bar — Phase 2 of the IA redesign.
//!
//! macOS users expect:
//!
//! - A proper menu bar with their app name (not "tauri-app").
//! - `App → Settings…` bound to `⌘,` (HIG; muscle memory across
//!   every macOS app).
//! - Standard Edit menu (cut / copy / paste / select all).
//! - View entries that switch the main window's sidebar section,
//!   so power users can jump tabs without reaching for the mouse.
//!
//! We do not pretend to ship a serious menu on Linux or Windows
//! (their conventions are different and Hush isn't tested there
//! hands-on). On non-macOS, [`apply`] returns Ok without touching
//! the default menu — Tauri's auto-generated minimal menu stays in
//! place.
//!
//! ## Wire shape: menu IDs → frontend
//!
//! Menu items have stable string IDs so the menu-event handler can
//! dispatch on them without depending on label copy:
//!
//! - `settings` — open the Settings window
//!   ([`crate::settings_window::show`])
//! - `goto-dictation` / `goto-meetings` / `goto-history` /
//!   `goto-configuration` — emit `menu:goto-section` Tauri event
//!   with the section name as payload; the main window's onMount
//!   listener flips its `activeSection` rune.
//!
//! ## Why event-not-IPC for the goto- entries
//!
//! The frontend already owns the sidebar section state. Pushing it
//! through an IPC `set_active_section` would mean a server-side
//! mirror of UI state with no purpose. A one-way event is cheap
//! and matches how `meeting:source-failed` already works.

#[cfg(target_os = "macos")]
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder},
    AppHandle, Emitter, Runtime,
};

#[cfg(not(target_os = "macos"))]
use tauri::{AppHandle, Runtime};

/// Install the native macOS menu and wire up its event handler. On
/// non-macOS this is a no-op. Best-effort: a menu-build failure is
/// logged and swallowed — the app still launches with Tauri's
/// default auto-generated menu.
pub fn apply<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = build_and_set_menu(app) {
        tracing::error!(error = ?e, "failed to build native app menu; falling back to Tauri default");
    }
}

#[cfg(target_os = "macos")]
fn build_and_set_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // App submenu: leftmost on macOS, conventionally named after the
    // app. Settings + standard "hide / hide others / show all / quit"
    // group are HIG-canonical; missing any of them feels off.
    let app_submenu = SubmenuBuilder::new(app, "Hush")
        .about(None)
        .item(&MenuItemBuilder::with_id("check-for-updates", "Check for Updates…").build(app)?)
        .separator()
        .item(
            &MenuItemBuilder::with_id("settings", "Settings…")
                .accelerator("CmdOrCtrl+,")
                .build(app)?,
        )
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let edit_submenu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    // View submenu: section navigation. ⌘1..⌘3 mirror the sidebar
    // order. Configuration was a Phase 1 placeholder; Phase 3 moved
    // its panels into the Settings window (⌘, on the App menu).
    let view_submenu = SubmenuBuilder::new(app, "View")
        .item(
            &MenuItemBuilder::with_id("goto-dictation", "Dictation")
                .accelerator("CmdOrCtrl+1")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("goto-meetings", "Meetings")
                .accelerator("CmdOrCtrl+2")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("goto-history", "History")
                .accelerator("CmdOrCtrl+3")
                .build(app)?,
        )
        .build()?;

    let window_submenu = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .fullscreen()
        .build()?;

    let menu = MenuBuilder::new(app)
        .items(&[&app_submenu, &edit_submenu, &view_submenu, &window_submenu])
        .build()?;

    app.set_menu(menu)?;

    // Menu-event dispatch. Stable IDs are matched directly; goto-
    // ones are derived by stripping the prefix so adding a future
    // section is a one-line change in the View submenu.
    app.on_menu_event(move |app, event| {
        let id = event.id.as_ref();
        match id {
            "settings" => {
                if let Err(e) = crate::settings_window::show(app) {
                    tracing::error!(error = ?e, "menu: open settings");
                }
            }
            "check-for-updates" => {
                // One-click semantics (#265). Pre-fix the menu
                // opened Settings → About and waited for a second
                // click on the in-tab "Check for updates" button.
                // Every polished macOS app (Slack, 1Password,
                // Xcode, Safari) fires the check on the menu click
                // directly. We do the same: spawn the probe, emit
                // the result as `updater:result`. The About tab
                // subscribes to that event and renders the outcome
                // — same UI, just driven by event instead of
                // button. Opening Settings → About in parallel
                // gives the user a place to read the result; if
                // they have Settings open already this is a no-op.
                let app_handle = app.clone();
                if let Err(e) = crate::settings_window::show(app) {
                    tracing::error!(error = ?e, "menu: open settings (check-for-updates)");
                }
                if let Err(e) = app.emit("settings:goto-tab", "about") {
                    tracing::warn!(error = ?e, "menu: emit goto-tab(about)");
                }
                // Inflight guard — review #3 caught that rapid
                // double-clicks would spawn two parallel probes,
                // each emitting `updater:result` and each burning
                // a slot from GitHub's 60/hr unauthenticated rate
                // limit. Skip the spawn if a probe is already
                // running. AcqRel cmpxchg pairs with the Release
                // store at task end so the next click sees a
                // freshly-cleared flag only after the previous
                // emit landed.
                use std::sync::atomic::{AtomicBool, Ordering};
                static PROBE_INFLIGHT: AtomicBool = AtomicBool::new(false);
                if PROBE_INFLIGHT
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    tauri::async_runtime::spawn(async move {
                        use tauri::Manager as _;
                        let state = app_handle.state::<crate::ipc::AppState>();
                        let outcome = crate::updater::check_for_updates(&state.http).await;
                        PROBE_INFLIGHT.store(false, Ordering::Release);
                        match outcome {
                            Ok(result) => {
                                if let Err(e) = app_handle.emit("updater:result", &result) {
                                    tracing::warn!(
                                        error = ?e,
                                        "menu check-for-updates: emit result failed"
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = ?e,
                                    "menu check-for-updates: probe failed"
                                );
                            }
                        }
                    });
                } else {
                    tracing::debug!("menu check-for-updates: probe already in flight; skipping");
                }
            }
            id if id.starts_with("goto-") => {
                let section = id.trim_start_matches("goto-").to_owned();
                if let Err(e) = app.emit("menu:goto-section", section) {
                    tracing::warn!(error = ?e, "menu: emit goto-section");
                }
            }
            _ => {}
        }
    });

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn build_and_set_menu<R: Runtime>(_app: &AppHandle<R>) -> tauri::Result<()> {
    Ok(())
}
