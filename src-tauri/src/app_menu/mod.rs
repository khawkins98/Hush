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
//! - `goto-dictation` / `goto-history` — emit `menu:goto-section`
//!   Tauri event with the section name as payload; the main
//!   window's onMount listener flips its `activeSection` rune.
//!   (Pre-#357 Phase 1 also exposed `goto-meetings`; meetings now
//!   surface in History once Phase 2 lands.)
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
        // Custom "About Hush" that opens Settings → About tab (#478-adjacent).
        // Using `.about(None)` would show the bare native macOS panel (icon +
        // version only). The custom AboutTab.svelte is far richer — version,
        // blurb, pipeline diagram, update checker, links — so we intercept the
        // click and route it to Settings just like "Check for Updates" does.
        .item(&MenuItemBuilder::with_id("about-hush", "About Hush").build(app)?)
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
        // Custom Quit (#328). Pre-fix this used `.quit()` which
        // routes through Tauri's native macOS terminate path and
        // ends up firing `RunEvent::ExitRequested`. The interceptor
        // in `lib.rs::run` blocks every exit unless the user
        // clicked Quit, so a custom item that sets the
        // "user requested" flag synchronously before calling
        // `app.exit(0)` is the only path that proceeds.
        .item(
            &MenuItemBuilder::with_id("app-quit", "Quit Hush")
                .accelerator("CmdOrCtrl+Q")
                .build(app)?,
        )
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

    // View submenu: section navigation. ⌘1/⌘2 mirror the sidebar
    // order after #357 Phase 1 collapsed Dictation/Meetings/History
    // to Dictation/History — meeting sessions surface in the unified
    // History feed once Phase 2 lands. Configuration was a pre-IA
    // placeholder; its panels live in the standalone Settings
    // window (⌘, on the App menu).
    let view_submenu = SubmenuBuilder::new(app, "View")
        .item(
            &MenuItemBuilder::with_id("goto-dictation", "Dictation")
                .accelerator("CmdOrCtrl+1")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("goto-history", "History")
                .accelerator("CmdOrCtrl+2")
                .build(app)?,
        )
        .build()?;

    // Window submenu carries the standard macOS window-management
    // affordances PLUS our custom `close-window` item bound to
    // ⌘W (#336). Tauri 2 doesn't synthesise a Close item for us,
    // so without this entry ⌘W is a silent no-op — a real-feel
    // papercut that distinguishes "Mac app" from "web app in a
    // window". The item ID is dispatched in `on_menu_event` to
    // hide (not destroy) the focused window: main + settings rejoin
    // the close-hide pattern wired in lib.rs::run; HUD hides without
    // affecting the in-flight recording.
    let window_submenu = SubmenuBuilder::new(app, "Window")
        .item(
            &MenuItemBuilder::with_id("close-window", "Close Window")
                .accelerator("CmdOrCtrl+W")
                .build(app)?,
        )
        .separator()
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
                // #479 slice 3: Settings is an inline panel inside
                // the main window now. Emit `settings:goto-tab` and
                // let the main window's listener flip its active
                // section + tab.
                if let Err(e) = app.emit("settings:goto-tab", "general") {
                    tracing::error!(error = ?e, "menu: emit goto-tab(general)");
                }
            }
            "app-quit" => crate::request_user_quit(app),
            "about-hush" => {
                if let Err(e) = crate::settings_window::show(app) {
                    tracing::error!(error = ?e, "menu: open settings (about)");
                }
                if let Err(e) = app.emit("settings:goto-tab", "about") {
                    tracing::warn!(error = ?e, "menu: emit goto-tab(about)");
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
                // #479 slice 3: emit goto-tab to the inline panel —
                // the main window's listener flips its active
                // section + tab.
                if let Err(e) = app.emit("settings:goto-tab", "about") {
                    tracing::warn!(error = ?e, "menu: emit goto-tab(about)");
                }
                // Inflight guard — rapid double-clicks would
                // otherwise spawn two parallel probes, each
                // emitting `updater:result` and each burning a slot
                // from GitHub's 60/hr unauthenticated rate limit.
                // RAII guard so a panic inside the spawned task
                // (or a runtime abort) still clears the flag —
                // pre-fix a bare `store(false)` after the await
                // could be skipped, leaving the flag stuck `true`
                // and silently disabling the menu item for the
                // rest of the process lifetime (review #4 R-3).
                use std::sync::atomic::{AtomicBool, Ordering};
                static PROBE_INFLIGHT: AtomicBool = AtomicBool::new(false);
                struct InflightGuard;
                impl Drop for InflightGuard {
                    fn drop(&mut self) {
                        PROBE_INFLIGHT.store(false, Ordering::Release);
                    }
                }
                if PROBE_INFLIGHT
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    tauri::async_runtime::spawn(async move {
                        // Holding `_guard` for the task body
                        // ensures Drop runs whether we exit via
                        // normal completion, await cancellation,
                        // or panic unwind.
                        let _guard = InflightGuard;
                        use tauri::Manager as _;
                        let state = app_handle.state::<crate::ipc::AppState>();
                        match crate::updater::check_for_updates(&state.http).await {
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
            "close-window" => {
                // ⌘W (#336). Hide the focused window rather than
                // destroying it — the close-hide pattern in
                // lib.rs::run already does this for the red-✕
                // button on `main` and `settings`; we route ⌘W
                // through the same path so a future change to the
                // hide policy applies uniformly. HUD hides without
                // affecting the in-flight recording (same semantics
                // as the dismiss button on the HUD itself).
                //
                // Tauri 2 has no direct "focused window" getter on
                // AppHandle, so we iterate webview_windows() and
                // pick the one whose `is_focused()` returns true.
                use tauri::Manager as _;
                let focused =
                    app.webview_windows()
                        .into_iter()
                        .find_map(|(_, w)| match w.is_focused() {
                            Ok(true) => Some(w),
                            _ => None,
                        });
                if let Some(window) = focused {
                    let label = window.label().to_owned();
                    if let Err(e) = window.hide() {
                        tracing::warn!(
                            error = ?e,
                            label = %label,
                            "menu close-window: hide failed"
                        );
                    }
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
