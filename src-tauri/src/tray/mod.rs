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
    let popover = MenuItem::with_id(
        app,
        "tray:popover",
        // Hush's design-inspired distillation (#427 Item 1) calls
        // for a Panic-style menu-bar popover as the primary quick-
        // access surface. This menu item summons it. Phase 1
        // leaves the existing left-click-opens-menu behaviour
        // intact; replacing the click is a follow-up once the
        // popover has had hands-on testing on macOS.
        "Quick popover",
        true,
        None::<&str>,
    )?;
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
    // Custom Quit item (#328). Pre-fix this was
    // `PredefinedMenuItem::quit` which routes through Tauri's
    // platform-native quit and ends up firing
    // `RunEvent::ExitRequested`. The new ExitRequested
    // interceptor in `lib.rs::run` prevents *every* exit unless
    // the user explicitly clicked Quit, so a custom item that
    // sets the "user requested" flag synchronously before
    // calling `app.exit(0)` is the only path that proceeds.
    let quit = MenuItem::with_id(app, "tray:quit", "Quit Hush", true, None::<&str>)?;

    let separator = PredefinedMenuItem::separator(app)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &popover,
            &toggle,
            &separator,
            &settings,
            &separator2,
            &quit,
        ],
    )?;

    let _tray = TrayIconBuilder::with_id("hush-tray")
        .menu(&menu)
        // macOS template image (#275). macOS menu-bar template
        // images MUST be flat monochrome — black shapes on
        // transparent — for `icon_as_template(true)` to work.
        // Pre-fix this used `default_window_icon()` (full-color
        // RGBA) which produced a black blob with no recognisable
        // shape on light menu bars. The dedicated `tray-icon.png`
        // (16×16) + `tray-icon@2x.png` (32×32) assets are
        // alpha-extracted silhouettes of the brand mark.
        //
        // `Image::from_bytes` parses the PNGs at compile time via
        // `include_bytes!`; the assets ship alongside the source
        // so they don't need a `bundle.resources` entry. macOS
        // picks the @2x variant on retina displays automatically
        // when both are present in `Image`'s pixel data — but
        // Tauri's `Image::from_bytes` only takes one PNG, so we
        // load the @2x variant directly and let macOS down-scale
        // for non-retina (the 32px source halves cleanly to 16px
        // with no aliasing issues for a high-contrast silhouette).
        //
        // Fallback chain on errors:
        // 1. Tray icon decode failure → `default_window_icon()`
        //    (looks wrong in dark mode but at least visible).
        // 2. That also failing → empty image (clickable region,
        //    no glyph — still better than failing the whole build).
        .icon({
            const TRAY_ICON_2X: &[u8] = include_bytes!("../../icons/tray-icon@2x.png");
            tauri::image::Image::from_bytes(TRAY_ICON_2X).unwrap_or_else(|e| {
                tracing::warn!(
                    error = ?e,
                    "tray: failed to decode tray-icon@2x.png; falling back to app icon"
                );
                app.default_window_icon()
                    .cloned()
                    .unwrap_or_else(|| tauri::image::Image::new_owned(Vec::new(), 0, 0))
            })
        })
        // macOS: render as a tinted "template" image so it adapts
        // to dark/light menu bar. Now actually correct because the
        // input is monochrome with a real alpha channel; pre-#275
        // this flag was wired but the input was wrong.
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
        "tray:popover" => show_menu_bar_popover(app),
        "tray:toggle" => emit_toggle(app),
        "tray:settings" => {
            if let Err(e) = crate::settings_window::show(app) {
                tracing::warn!(error = ?e, "tray: failed to open settings window");
            }
        }
        "tray:quit" => crate::request_user_quit(app),
        _ => {}
    }
}

/// Show + focus the menu-bar quick popover (#427 Item 1). The
/// window is created with `visible: false` in `tauri.conf.json`
/// so it stays hidden until the user invokes it from the tray
/// menu. Best-effort — a missing window or failed `show()` is
/// logged and swallowed; the user can still reach the main
/// window via the "Show Hush" menu item.
fn show_menu_bar_popover<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("menu-bar") else {
        tracing::warn!("tray: menu-bar popover window not found");
        return;
    };
    if let Err(e) = window.show() {
        tracing::warn!(error = ?e, "tray: failed to show menu-bar popover");
        return;
    }
    if let Err(e) = window.set_focus() {
        tracing::warn!(error = ?e, "tray: failed to focus menu-bar popover");
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
