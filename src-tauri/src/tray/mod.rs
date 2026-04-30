//! Status-bar / system-tray icon.
//!
//! macOS users expect a menu-bar icon for any always-on utility (the
//! reference shape is what CleanShot X / Bartender / Wispr Flow ship);
//! Windows and Linux users expect the same affordance in the system
//! tray. Tauri 2 ships tray-icon support in core (no plugin), so this
//! module is a thin wrapper over [`tauri::tray::TrayIconBuilder`].
//!
//! ## What the icon does
//!
//! - **Click the icon (left-click on macOS / Windows, right-click on
//!   Linux's notification area):** opens the menu below.
//! - **Show Hush:** focuses the main window if open, restores it if
//!   minimised, brings it forward if the user alt-tabbed away.
//! - **Start / Stop Recording:** state-aware label that mirrors the
//!   frontend's `recording` rune. Clicking emits `hotkey:toggle` —
//!   the frontend's existing listener handles start/stop. The label
//!   updates via the `ui:recording-state` Tauri event the frontend
//!   pushes when its `recording` value changes.
//! - **Open Settings…:** opens (or focuses) the standalone Settings
//!   window. Useful if the user closed the main window and only has
//!   the tray to reach configuration.
//! - **Quit:** clean exit through Tauri's app-handle.
//!
//! ## Why event-not-IPC for "Toggle Recording"
//!
//! The frontend already owns the recording-state machine (the
//! `recording` rune, the `busy` flag, the audio source picker). Adding
//! an IPC entry point that calls `start_dictation` directly would mean
//! a second start path that has to re-derive the source, the
//! audio-source-listing fetch, the model-loaded check. Reusing the
//! `hotkey:toggle` event keeps one path.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Listener, Manager, Runtime};

/// Build the tray icon and install it on the app. Best-effort: a
/// build failure is logged and swallowed — the rest of the app
/// (window, hotkey, dictation pipeline) continues without a tray.
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = build(app) {
        tracing::error!(error = ?e, "failed to install tray icon");
    }
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    // Menu items keyed by stable string ids so the event handler
    // dispatches on the id rather than label copy.
    let show = MenuItem::with_id(app, "tray:show", "Show Hush", true, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        "tray:toggle",
        // Default label assumes "not recording". The
        // `ui:recording-state` listener below swaps to "Stop
        // Recording" when the frontend's `recording` rune flips.
        "Start Recording",
        true,
        // Render the same accelerator as the global toggle hotkey
        // so the user sees the keyboard binding next to the menu
        // entry. The hotkey itself is registered separately via
        // `tauri-plugin-global-shortcut`; this string is purely a
        // hint glyph (Tauri does not actually wire it).
        //
        // Must be `Ctrl+Alt+H` — *not* `CmdOrCtrl+Alt+H` (#264).
        // The `CmdOrCtrl` token resolves to ⌘ on macOS, but
        // `DEFAULT_TOGGLE_HOTKEY` (in `hotkey/mod.rs`) is literal
        // `Ctrl` (⌃) — and `Cmd+Alt+H` is the macOS system
        // shortcut for "Hide All Other Apps", so the wrong glyph
        // not only mis-labelled the menu but pointed at a built-in
        // shortcut that does the wrong thing entirely.
        Some("Ctrl+Alt+H"),
    )?;
    let settings = MenuItem::with_id(
        app,
        "tray:settings",
        "Open Settings…",
        true,
        Some("CmdOrCtrl+,"),
    )?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit Hush"))?;

    let separator = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&show, &toggle, &separator, &settings, &separator2, &quit],
    )?;

    let _tray = TrayIconBuilder::with_id("hush-tray")
        .menu(&menu)
        // Reuse the bundled app icon (the same one shown in the
        // sidebar brand chip). On macOS the menu-bar renders this
        // as a 16-22 px square; the existing icon is high-enough
        // resolution that it scales cleanly.
        .icon(app.default_window_icon().cloned().unwrap_or_else(|| {
            // Fallback: macOS treats `Image::default()` as an empty
            // icon which still installs a clickable region but is
            // visually empty. Better than failing the whole build.
            tauri::image::Image::new_owned(Vec::new(), 0, 0)
        }))
        // macOS: render as a tinted "template" image so it adapts
        // to dark/light menu bar. Without this, a coloured icon
        // looks wrong in dark mode. Other platforms ignore this
        // hint.
        .icon_as_template(true)
        // On macOS the tray is a true "menu extra" — left-click
        // opens the menu. On Windows / Linux some users expect
        // left-click to open the main window directly; we honour
        // the `on_tray_icon_event` below for that.
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(handle_icon_event)
        .build(app)?;

    // Listen for the frontend's recording-state pushes and update
    // the toggle menu item's label. The frontend emits
    // `ui:recording-state` with a JSON boolean payload whenever its
    // `recording` rune changes — see `src/routes/+page.svelte`. The
    // toggle item is cloned into the closure so the listener owns a
    // direct handle and doesn't need to walk the tray's menu tree
    // (Tauri's `TrayIcon` has no `menu()` getter as of 2.10).
    // Listener is detached and lives for the lifetime of the app.
    let toggle_for_listener = toggle.clone();
    app.listen("ui:recording-state", move |event| {
        let recording: bool = serde_json::from_str(event.payload()).unwrap_or(false);
        let label = if recording {
            "Stop Recording"
        } else {
            "Start Recording"
        };
        let _ = toggle_for_listener.set_text(label);
    });

    Ok(())
}

fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "tray:show" => show_main_window(app),
        "tray:toggle" => emit_toggle(app),
        "tray:settings" => {
            if let Err(e) = crate::settings_window::show(app) {
                tracing::warn!(error = ?e, "tray: failed to open settings window");
            }
        }
        _ => {}
    }
}

fn handle_icon_event<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, event: TrayIconEvent) {
    // We let `menu_on_left_click(true)` handle the macOS / Windows
    // primary-click → open-menu path. The only event we explicitly
    // handle here is a double-click → focus main window, which
    // `menu_on_left_click` doesn't cover. Keep this small: more
    // event handling here means less predictable click behaviour.
    if let TrayIconEvent::DoubleClick {
        button: MouseButton::Left,
        ..
    } = event
    {
        show_main_window(tray.app_handle());
    }
}

fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn emit_toggle<R: Runtime>(app: &AppHandle<R>) {
    // Reuse the existing toggle-hotkey event channel so the
    // frontend's `hotkey:toggle` listener handles start/stop.
    // Cheap; the frontend already gates on `busy` / `recording`.
    if let Err(e) = app.emit("hotkey:toggle", ()) {
        tracing::warn!(error = ?e, "tray: failed to emit hotkey:toggle");
    }
}
