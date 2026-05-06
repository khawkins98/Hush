// Domain modules. Exposed at the crate root so integration tests and the
// IPC layer can address them by their public surface.
pub mod app_menu;
pub mod audio;
pub mod audio_cues;
pub mod db;
pub mod debug_log;
pub mod diarization;
pub mod dictionary;
pub mod events;
pub mod history;
pub mod hotkey;
pub mod hud;
pub mod ipc;
pub mod meeting;
pub mod permissions;
pub mod repository;
pub mod settings;
pub mod transcription;
pub mod tray;
pub mod updater;

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{Emitter, Manager};

/// Set by the tray's "Quit Hush" / app-menu "Quit" handlers right
/// before they call `app_handle.exit(0)`. Read by the
/// `RunEvent::ExitRequested` interceptor in `run()` to distinguish
/// "user explicitly quit" from "Tauri's runtime decided to exit
/// because no webview windows are visible" (#328).
///
/// On Linux/Windows the runtime's default is to quit when the
/// last window closes — but Hush's close-hide pattern (`#263`)
/// hides every window so a normal close-the-main-window action
/// would leave the runtime with zero visible windows and the
/// tray icon would vanish along with the app. Same risk theoretical
/// on macOS though `set_activation_policy(Accessory)` masks it on
/// the background-launch path. The flag is the single source of
/// truth for "intentional quit" across both Quit paths.
///
/// `static` rather than threaded through `AppState` because the
/// menu / tray handlers live in `setup` closures that don't have
/// access to AppState yet (it's `manage`'d slightly earlier but
/// the menu builders capture `&AppHandle`, not `State`). A static
/// AtomicBool is the simplest cross-handler signal — no
/// coordination, no locking, deterministic memory model.
static USER_QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Public helper called from the tray + app-menu Quit handlers.
/// Sets the flag synchronously, then calls `app_handle.exit(0)`.
/// The synchronous-before-exit ordering matters: the
/// `RunEvent::ExitRequested` handler reads the flag, and `exit`
/// dispatches the event later in the runtime. By the time the
/// event fires the flag is already set.
pub fn request_user_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    USER_QUIT_REQUESTED.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Did the LaunchAgent fire us with `--background`? Returns true if
/// any arg in the iterator is exactly `"--background"`. Extracted
/// to a testable helper (review #4 R-5) so the policy — case-
/// sensitive, exact match, no `=value` form — has a unit test
/// pinning it. Pre-fix this lived inline in `setup` and was only
/// reachable via dev-launch smoke.
///
/// Gated to macOS because that's the only platform that registers
/// a LaunchAgent (Linux/Windows autostart paths don't pass the
/// flag). Ungated, Ubuntu CI's `clippy --all-targets` flags it as
/// `dead_code` since the only call site is in a
/// `#[cfg(target_os = "macos")]` block.
#[cfg(target_os = "macos")]
fn is_background_launch(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|a| a == "--background")
}

/// Hide the macOS zoom (green ＋) traffic-light button for a window
/// whose `maximizable` is false. `maximizable: false` greys out the
/// button but leaves it visible as a disabled circle — removing it
/// entirely is cleaner. Called once from setup for the `main` window.
///
/// Best-effort: a missing handle or failed ObjC call is logged and
/// swallowed; the worst case is the greyed-out button remains.
#[cfg(target_os = "macos")]
fn hide_macos_zoom_button<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let label = window.label().to_owned();
    let ns_window_ptr = match window.ns_window() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = ?e,
                window = %label,
                "hide_macos_zoom_button: ns_window() failed"
            );
            return;
        }
    };
    let ns_window = ns_window_ptr as *mut AnyObject;
    if ns_window.is_null() {
        tracing::warn!(
            window = %label,
            "hide_macos_zoom_button: ns_window pointer is null"
        );
        return;
    }
    // Safety: Tauri owns the NSWindow for the window's full
    // lifetime. `standardWindowButton:` returns a retained
    // pointer to the button view (an NSButton subclass); calling
    // `setHidden:` on it is a simple boolean setter with no
    // ownership transfer.
    //
    // NSWindowZoomButton = 2 in AppKit's NSWindowButton enum.
    unsafe {
        let zoom_btn: *mut AnyObject = msg_send![ns_window, standardWindowButton: 2usize];
        if !zoom_btn.is_null() {
            let _: () = msg_send![zoom_btn, setHidden: true];
        }
    }
}

/// Make a borderless macOS window draggable from any non-
/// interactive area (#427 Item 1).
///
/// `decorations: false` windows in Tauri 2 have their NSWindow
/// movable styleMask bit stripped, which silently breaks
/// `data-tauri-drag-region`, `startDragging()`, and even
/// programmatic `setPosition` calls during a JS-tracked drag.
/// `setMovable: YES` + `setMovableByWindowBackground: YES`
/// restore the AppKit-level drag handling, after which Tauri's
/// drag-region attribute starts working as documented.
///
/// Called once per window from the `setup` hook. Best-effort:
/// a missing handle or failed cast is logged at warn and
/// swallowed — the worst case is a non-draggable window, which
/// matches the pre-fix behaviour.
#[cfg(target_os = "macos")]
fn unlock_macos_window_drag<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let label = window.label().to_owned();
    let ns_window_ptr = match window.ns_window() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = ?e,
                window = %label,
                "unlock_macos_window_drag: ns_window() failed"
            );
            return;
        }
    };
    let ns_window = ns_window_ptr as *mut AnyObject;
    if ns_window.is_null() {
        tracing::warn!(
            window = %label,
            "unlock_macos_window_drag: ns_window pointer is null"
        );
        return;
    }
    // Safety: Tauri owns the NSWindow for the window's full
    // lifetime; `setMovable` / `setMovableByWindowBackground`
    // are setter selectors that don't take or transfer
    // ownership and don't mutate runtime state visible to Rust.
    unsafe {
        let _: () = msg_send![ns_window, setMovable: true];
        let _: () = msg_send![ns_window, setMovableByWindowBackground: true];
    }
}

/// Filename for the app's SQLite database, stored in the platform's
/// per-app data directory (e.g. `~/Library/Application Support/Hush/`
/// on macOS).
const DB_FILENAME: &str = "hush.db";

/// Subdirectory under the platform app-data dir where the model
/// picker scans for downloaded GGUF files. Auto-download (when it
/// lands) will write here; for now users put files here manually.
const MODELS_DIRNAME: &str = "models";

/// Bundle identifier the app shipped under before #525. The rename to
/// `io.github.khawkins98.hush` strands user data (DB, downloaded
/// models) at the old path on any pre-rename install.
const LEGACY_BUNDLE_ID: &str = "com.khawkins.hush";

/// Move the legacy `com.khawkins.hush` app-data directory to the new
/// path on first launch after the rename (#525). Idempotent — a no-op
/// when the legacy path doesn't exist or the new path is already
/// populated. Logs at info on success, warn on conflict, error on
/// `rename` failure (e.g. cross-volume — extremely rare on macOS
/// since both paths share `~/Library/Application Support/`).
///
/// Only the Application Support directory is migrated. `~/Library/
/// Caches/com.khawkins.hush/` regenerates automatically and isn't
/// worth the failure surface; `~/Library/LaunchAgents/com.khawkins.
/// hush.plist` (autostart) points at the old binary path and is
/// stale after a rebuild — the user re-toggles Settings → Start at
/// Login to register a fresh entry. Documented in #525.
fn migrate_legacy_app_data_dir(new_path: &std::path::Path) {
    let Some(parent) = new_path.parent() else {
        return;
    };
    let old_path = parent.join(LEGACY_BUNDLE_ID);
    if !old_path.exists() {
        return;
    }
    if new_path.exists() {
        tracing::warn!(
            old = %old_path.display(),
            new = %new_path.display(),
            "legacy bundle-id app-data dir present alongside the new one — leaving the old path untouched"
        );
        return;
    }
    match std::fs::rename(&old_path, new_path) {
        Ok(()) => tracing::info!(
            from = %old_path.display(),
            to = %new_path.display(),
            "migrated app-data dir from legacy bundle identifier (#525)"
        ),
        Err(e) => tracing::error!(
            error = ?e,
            from = %old_path.display(),
            to = %new_path.display(),
            "failed to migrate legacy app-data dir; the app will run with a fresh data dir"
        ),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing here so service-construction errors (database
    // open, whisper model load) reach `RUST_LOG` consumers before the
    // Tauri event loop starts.
    //
    // We compose two layers:
    //   1. The standard fmt subscriber (writes to stderr / RUST_LOG).
    //   2. DebugLogLayer — captures events into a ring buffer and
    //      forwards them to the frontend via the `log:event` Tauri
    //      event once the AppHandle is available (#532).
    //
    // `try_init` rather than `init` so re-runs in tests
    // (`cargo tauri dev`-restart-cycle) do not panic.
    let debug_log = crate::debug_log::DebugLogState::new();
    {
        use tracing_subscriber::prelude::*;
        let fmt_layer = tracing_subscriber::fmt::layer().with_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        );
        let debug_layer = crate::debug_log::DebugLogLayer::new(debug_log.clone());
        let _ = tracing_subscriber::registry()
            .with(fmt_layer)
            .with(debug_layer)
            .try_init();
    }

    tauri::Builder::default()
        // Single-instance lock (#326). Registered first so a second
        // launch bails out before any of the side-effect-bearing
        // plugins below open SQLite, install a CGEventTap, register
        // the toggle hotkey, etc. The handler runs on the
        // *already-running* primary instance with the second
        // instance's argv: bring the existing main window forward
        // (the typical post-`--background` state has it hidden) and
        // focus it so the user sees Hush respond to the second
        // launch attempt. The second process exits on its own when
        // the plugin returns.
        //
        // Background-launch carve-out (#349). If the second launch
        // is itself `--background` (e.g. a duplicate LaunchAgent
        // fire — rare, but observed across macOS resume-from-sleep
        // edge cases), do NOT show the window. Surfacing a window
        // a user explicitly didn't ask for would defeat the
        // background-launch discipline maintained at the existing
        // post-setup branch around line 280. On non-macOS the
        // helper is unavailable (no LaunchAgent path), so the
        // carve-out is a no-op and the original "show + focus"
        // behaviour stays.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            use tauri::Manager;
            #[cfg(target_os = "macos")]
            {
                if is_background_launch(_args.iter().cloned()) {
                    tracing::info!(
                        "single-instance: second launch is --background; \
                         leaving primary window untouched"
                    );
                    return;
                }
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // Install the global-shortcut handler at plugin-build time. Specific
        // shortcuts are registered later from `setup`, where we have access
        // to the [`AppHandle`] needed to call the registration API.
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(hotkey::handle_shortcut_event)
                .build(),
        )
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // Pass `--background` so the LaunchAgent-fired launch
            // is distinguishable from a user-initiated launch
            // (#268). The setup hook hides the main window and
            // sets the activation policy to Accessory when this
            // arg is present, matching the silent-tray-launch
            // behaviour every macOS background utility uses.
            // User-initiated launches via Finder / Spotlight
            // don't pass it, so they show the main window
            // normally.
            Some(vec!["--background"]),
        ))
        // TODO(#10): Uncomment once `plugins.updater` is present in
        // tauri.conf.json (pubkey + endpoints). Registering without that
        // block panics at startup: "Error deserializing 'plugins.updater'".
        // See the implementation plan in `src-tauri/src/updater/mod.rs`
        // for the full step-by-step — Steps 1–3 (keypair + conf + CI) are
        // the prerequisite; this line is Step 4.
        //.plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        // External-URL opener (#322). Plain `<a target="_blank">`
        // links do nothing in a Tauri 2 WebView — the shell
        // plugin's `open()` call from a click handler delegates
        // to the OS browser. Capabilities also need
        // `shell:allow-open`; see `capabilities/default.json` +
        // `capabilities/settings.json`.
        .plugin(tauri_plugin_shell::init())
        // Platform detection (#272). Frontend uses `platform()`
        // from `@tauri-apps/plugin-os` to decide macOS-specific
        // UI affordances; replaces the deprecated
        // `navigator.platform` reads in `+page.svelte` and
        // `settings/+page.svelte`.
        .plugin(tauri_plugin_os::init())
        .setup(|app| {
            // The platform app-data directory is only resolvable from a
            // Tauri `App` handle, so state construction has to live in
            // `setup` rather than at the top of `run`. Tauri's own async
            // runtime drives the SQLite open + migrations.
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app-data dir: {e}"))?;

            // One-shot migration from the pre-#525 bundle identifier.
            // Runs before any directory creation so the old DB + models
            // are in place when the rest of setup looks for them.
            migrate_legacy_app_data_dir(&app_data_dir);

            let db_path = app_data_dir.join(DB_FILENAME);
            let models_dir = app_data_dir.join(MODELS_DIRNAME);

            // Pre-create the models directory so the picker has a
            // stable place to point users at, even before any model
            // has been added.
            if let Err(e) = std::fs::create_dir_all(&models_dir) {
                tracing::error!(error = ?e, path = %models_dir.display(), "failed to create models dir");
            }

            tracing::info!(
                db = %db_path.display(),
                models_dir = %models_dir.display(),
                "starting Hush"
            );

            let app_handle = app.handle().clone();
            // Enable live streaming of log events to the frontend (#532).
            // Must be done before build_default so any log events during
            // app-state construction are captured.
            debug_log.set_handle(app_handle.clone());
            let state = tauri::async_runtime::block_on(ipc::AppState::build_default(
                app_handle,
                &db_path,
                models_dir,
                debug_log,
            ))
            .map_err(|e| format!("build app state: {e:#}"))?;
            // Clone the audio Arc out before `manage` takes ownership of
            // `state` — the level-meter pump task below needs a handle
            // it can read from without going through `app.state()` on
            // every tick.
            let audio_for_pump = std::sync::Arc::clone(&state.audio);
            // Clone the shared PTT-combo handle out before `manage`
            // takes ownership of `state` — the listener thread reads
            // it on every key event so a Settings UI edit takes
            // effect without restarting the rdev thread.
            let ptt_combo_for_listener = std::sync::Arc::clone(&state.ptt_combo);
            let ptt_active_for_listener = std::sync::Arc::clone(&state.ptt_active);
            let ptt_spawned_for_listener = std::sync::Arc::clone(&state.ptt_listener_spawned);

            // Clone the meeting-manager handle out before `manage`
            // for the orphan-reconcile spawn below.
            let meeting_manager_for_reconcile = std::sync::Arc::clone(&state.meeting_manager);

            app.manage(state);

            // Orphan-session reconciliation (#249, #329). Sessions
            // left open by a previous process that exited without
            // `stop_manual` (kill, OS crash, panic) get their
            // `ended_at` stamped now so the panel doesn't render
            // them as still-active. Best-effort: a DB failure here
            // is logged inside the manager and doesn't block
            // startup.
            //
            // Spawned (not block_on'd) so the SELECT + UPDATEs
            // don't hold the synchronous setup hook open while the
            // first paint is waiting (#329). The pump tasks below
            // and the IPC handlers don't depend on this completing
            // — the meeting manager's own internal locking handles
            // any race between a reconcile-in-flight and a fresh
            // `meeting_start_manual`.
            tauri::async_runtime::spawn(async move {
                meeting_manager_for_reconcile
                    .reconcile_orphan_sessions()
                    .await;
            });

            // Hide-on-close for main and debug (#263, #543). Tauri
            // 2's default destroys the window on red-✕; without
            // an intercept the user would close the main window
            // expecting it to hide and find Hush had quit (tray
            // icon gone). The debug console window is included so
            // closing its red-✕ hides it rather than destroying
            // the webview — this prevents macOS from stranding
            // focus on the desktop (which looks like the main
            // window also closed) and keeps the window's log
            // buffer alive for the next open.
            //
            // Pairs with the `RunEvent::ExitRequested` interceptor
            // wired below (#328): on Linux/Windows the runtime's
            // default is to quit when the last window goes away,
            // so hiding all windows would otherwise drop the tray
            // icon along with the app. The interceptor blocks
            // every runtime-driven exit; the only paths that quit
            // are the tray's "Quit Hush" item and the macOS
            // app-menu's Quit item, both of which call
            // `request_user_quit` to set a flag the interceptor
            // honours.
            //
            // Done from setup so the handlers are wired before
            // any user interaction can fire CloseRequested. The
            // closures clone the window handle so they outlive
            // setup; that's the standard pattern Tauri expects.
            for label in ["main", "debug"] {
                if let Some(window) = app.get_webview_window(label) {
                    let win_clone = window.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            // Always prevent the destroy. If the
                            // subsequent `hide()` fails the window
                            // stays visible — that's a strictly
                            // better failure mode than letting
                            // Tauri destroy the only surface the
                            // user has for the window. The user
                            // can quit Hush entirely via tray /
                            // menu / ⌘Q if they actually wanted
                            // out. Pre-fix this had a
                            // belt-and-braces second
                            // `prevent_close()` in the failure
                            // arm; the second call is a no-op
                            // (#286 review #4 finding).
                            api.prevent_close();
                            if let Err(e) = win_clone.hide() {
                                tracing::warn!(
                                    label = %win_clone.label(),
                                    error = ?e,
                                    "hide-on-close failed; window remains visible"
                                );
                            }
                        }
                    });
                } else {
                    tracing::warn!(
                        label,
                        "hide-on-close: window not found at setup time; close defaults to destroy"
                    );
                }
            }

            // Borderless-window drag enablement (#427 Item 1).
            // On macOS, `decorations: false` windows have their
            // NSWindow movable styleMask bit stripped — Tauri's
            // `data-tauri-drag-region` and `startDragging()` then
            // become silent no-ops. Restore movability via
            // explicit AppKit calls so users can drag the popover
            // and HUD pill from any non-interactive area. See
            // `learnings.md` 2026-05-03 for the chase.
            #[cfg(target_os = "macos")]
            for label in ["menu-bar", "hud"] {
                if let Some(window) = app.get_webview_window(label) {
                    unlock_macos_window_drag(&window);
                }
            }

            // Hide the zoom (green) traffic-light button on the
            // main window. `maximizable: false` disables it but
            // leaves a greyed-out circle; removing it entirely is
            // cleaner.
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                hide_macos_zoom_button(&window);
            }

            // Background-launch behaviour (#268). When the
            // LaunchAgent fires Hush at login, we don't want to
            // pop the main window — every macOS tray utility
            // (Rectangle, Bartender, Alfred) starts silent. The
            // installer / autostart-toggle code passes
            // `--background` as a CLI arg; if present, hide the
            // main window and switch to Accessory activation
            // policy so the Dock icon doesn't appear either.
            //
            // The flag is passed via the autostart plugin's
            // `Some(vec!["--background"])` registration argument
            // (see the `tauri_plugin_autostart::init` call above).
            #[cfg(target_os = "macos")]
            if is_background_launch(std::env::args()) {
                if let Some(main_win) = app.get_webview_window("main") {
                    let _ = main_win.hide();
                }
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                tracing::info!(
                    "background launch: main window hidden, activation policy = Accessory"
                );
            }

            // LaunchAgent path reconciliation (#271). The autostart
            // plugin's `enable()` writes a `~/Library/LaunchAgents/`
            // plist that points to the binary's *absolute path* at
            // the time it was called. If the user moves Hush.app
            // afterwards (the natural ~/Downloads → /Applications
            // flow for a DMG-distributed app), the stale plist
            // points at the old path and the LaunchAgent fails
            // silently at the next login — Settings still shows
            // "Launch at Login: on" but Hush never actually starts.
            //
            // Fix: on every startup where autostart is currently
            // enabled, re-register. The plugin's `enable()` is
            // idempotent + cheap (writes a small plist file), so a
            // blind re-enable is simpler than parsing the existing
            // plist to detect a path mismatch — and gets the same
            // outcome (the plist now points at `current_exe()`).
            // No prompts, no UI flicker; LaunchAgents don't gate on
            // any TCC permission. The cost is one fs::write of
            // ~500 bytes at every launch.
            //
            // If `enable()` fails (e.g. read-only home, file-system
            // permission issue) we log at warn level and write a
            // flag into AppState. Settings → General reads the flag
            // and surfaces a "path is stale" warning row with a
            // retry button (#317). The user's session is otherwise
            // unaffected — only the next-login behaviour is
            // degraded.
            #[cfg(target_os = "macos")]
            {
                use tauri_plugin_autostart::ManagerExt;
                let mgr = app.autolaunch();
                let enabled = mgr.is_enabled().unwrap_or(false);
                if enabled {
                    match mgr.enable() {
                        Ok(()) => tracing::debug!(
                            "autostart: re-registered LaunchAgent with current binary path (#271)"
                        ),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "autostart: re-register failed; LaunchAgent path may be stale (#271)"
                            );
                            if let Some(state) = app.try_state::<ipc::AppState>() {
                                state.runtime_flags.autostart_path_stale.store(
                                    true,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                            }
                        }
                    }
                }
            }

            // HUD level-meter pump (#21). Reads the latest RMS from the
            // audio backend at ~30 Hz and emits `audio:level` so the HUD
            // page can animate a bar. Lives here (not in commands.rs)
            // because the pump's lifetime is the app's, not any single
            // dictation. The audio backend itself owns the level
            // computation in its callback; this task is purely a
            // cross-process push.
            //
            // Throttling: 33 ms ≈ 30 fps, matches the HUD's pulse
            // animation cadence and is well above the audio callback
            // rate (~100 Hz at 48 kHz / 480-frame chunks).
            //
            // Activity gate (#329). Pre-fix the pump emitted at 30 Hz
            // for the entire process lifetime — ~2.6M IPC emits/day
            // at idle with the HUD hidden and no recording. Now we
            // skip the emit when nothing is recording: the HUD only
            // shows during recording anyway, and a stale "0.0"
            // doesn't help any other listener. Cadence stays at 30 Hz
            // so the meter goes live within one tick of capture
            // start without a separate kickoff signal.
            let app_for_pump = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker =
                    tokio::time::interval(std::time::Duration::from_millis(33));
                loop {
                    ticker.tick().await;
                    if !audio_for_pump.is_recording() {
                        continue;
                    }
                    let level = audio_for_pump.current_level();
                    if let Err(e) = app_for_pump.emit("audio:level", level) {
                        // No listener attached yet (HUD window hidden) is
                        // not an error per se, but the trace level keeps
                        // it out of the default log unless someone is
                        // actively investigating.
                        tracing::trace!(error = ?e, "emit audio:level failed");
                    }
                }
            });

            // Meeting auto-start poller (#112). Watches the foreground
            // app every 3 s; on a transition into a Meeting-classified
            // app, if the user has opted in via Settings → Meeting, it
            // calls `meeting_manager.start_manual` automatically. See
            // `meeting/autostart.rs` for the decision logic and the
            // explicit list of what's deliberately deferred (auto-stop
            // on blur, "ask" mode, permission pre-check).
            let app_for_autostart = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                run_meeting_autostart_poller(app_for_autostart).await;
            });

            // Native macOS menu bar (no-op on other platforms).
            // Replaces Tauri's auto-generated minimal menu with one
            // that names the app "Hush", binds Settings… to ⌘,, and
            // surfaces the sidebar sections under View. See
            // `app_menu/mod.rs` for the wire shape.
            app_menu::apply(app.handle());

            // Status-bar / system-tray icon. Cross-platform: macOS
            // menu-bar extra, Windows system tray, Linux notification
            // area. Reuses the toggle-hotkey event channel for "Toggle
            // Recording" so the frontend's existing listener handles
            // start/stop. See `tray/mod.rs`.
            tray::install(app.handle());

            // Hotkey registration is best-effort: if the OS refuses the
            // shortcut (already in use, missing permission, Wayland
            // compositor without support) we log and continue so the rest
            // of the app — device list, button-driven dictation — keeps
            // working.
            if let Err(e) = hotkey::register_default(app.handle()) {
                tracing::error!(error = ?e, "failed to register default toggle hotkey");
            }
            // PTT runs through `rdev` on a dedicated thread (rdev's listen
            // is blocking and installs a low-level OS hook). On macOS the
            // first call triggers the Input Monitoring permission prompt.
            // On Wayland the listener exits with an error and we proceed
            // without PTT — toggle and button-driven dictation still work.
            // See `hotkey::ptt` module header for the full rationale.
            if let Err(e) = hotkey::register_ptt_listener(
                app.handle(),
                ptt_combo_for_listener,
                ptt_active_for_listener,
                ptt_spawned_for_listener,
            ) {
                tracing::error!(error = ?e, "failed to start PTT listener");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::commands::dictation::audio_list_sources,
            ipc::commands::system::show_main_window,
            ipc::commands::system::open_debug_window,
            ipc::commands::dictation::start_dictation,
            ipc::commands::dictation::stop_dictation,
            ipc::commands::history::history_list,
            ipc::commands::history::history_search,
            ipc::commands::history::history_export_row_csv,
            ipc::commands::export::history_export_bundle,
            ipc::commands::history::history_delete,
            ipc::commands::history::history_count,
            ipc::commands::history::history_clear,
            ipc::commands::history::get_dictation_stats,
            ipc::commands::dictionary::replacements_list,
            ipc::commands::dictionary::replacement_create,
            ipc::commands::dictionary::replacement_update,
            ipc::commands::dictionary::replacement_delete,
            ipc::commands::dictionary::vocabulary_list,
            ipc::commands::dictionary::vocabulary_create,
            ipc::commands::dictionary::vocabulary_update,
            ipc::commands::dictionary::vocabulary_delete,
            ipc::commands::models::model_list,
            ipc::commands::models::model_select,
            ipc::commands::models::model_download,
            ipc::commands::models::model_cancel_download,
            ipc::commands::models::model_remove,
            ipc::commands::system::get_first_run_completed,
            ipc::commands::system::mark_first_run_completed,
            ipc::commands::system::reset_first_run,
            ipc::commands::settings::get_hud_enabled,
            ipc::commands::settings::set_hud_enabled,
            ipc::commands::settings::get_sound_cues_enabled,
            ipc::commands::settings::set_sound_cues_enabled,
            ipc::commands::settings::get_sound_cue_start_enabled,
            ipc::commands::settings::set_sound_cue_start_enabled,
            ipc::commands::settings::get_sound_cue_complete_enabled,
            ipc::commands::settings::set_sound_cue_complete_enabled,
            ipc::commands::settings::preview_sound_cue,
            ipc::commands::settings::get_meeting_autostart_mode,
            ipc::commands::settings::set_meeting_autostart_mode,
            ipc::commands::settings::get_diarization_enabled,
            ipc::commands::settings::set_diarization_enabled,
            ipc::commands::settings::get_inference_threads,
            ipc::commands::settings::set_inference_threads,
            ipc::commands::settings::get_mic_gain_db,
            ipc::commands::settings::set_mic_gain_db,
            ipc::commands::diarizer::get_diarizer_model_status,
            ipc::commands::diarizer::download_diarizer_model,
            ipc::commands::diarizer::remove_diarizer_model,
            ipc::commands::system::get_autostart_path_status,
            ipc::commands::system::retry_autostart_registration,
            ipc::commands::system::check_for_updates,
            ipc::commands::system::get_app_version,
            ipc::commands::system::get_build_info,
            ipc::commands::ptt::ptt_get_config,
            ipc::commands::ptt::ptt_set_config,
            ipc::commands::permissions::open_macos_privacy_pane,
            ipc::commands::permissions::diagnose_macos_permissions,
            ipc::commands::permissions::reset_macos_permissions,
            ipc::commands::permissions::get_permission_health,
            ipc::commands::permissions::confirm_permission,
            ipc::commands::permissions::prime_screen_recording_permission,
            ipc::commands::permissions::request_microphone_permission,
            ipc::commands::permissions::request_input_monitoring_permission,
            ipc::commands::permissions::relaunch_app,
            ipc::commands::meeting::meeting_sessions_list,
            ipc::commands::meeting::meeting_sessions_search,
            ipc::commands::meeting::meeting_session_get,
            ipc::commands::meeting::meeting_session_delete,
            ipc::commands::meeting::meeting_session_export,
            ipc::commands::meeting::meeting_session_set_notes,
            ipc::commands::meeting::meeting_active_session,
            ipc::commands::meeting::meeting_start_manual,
            ipc::commands::meeting::meeting_stop_manual,
            ipc::commands::meeting::meeting_app_override_list,
            ipc::commands::meeting::meeting_app_override_upsert,
            ipc::commands::meeting::meeting_app_override_set_profile,
            ipc::commands::meeting::meeting_app_override_delete,
            ipc::commands::meeting::meeting_app_classifier_defaults,
            ipc::commands::updater::install_pending_update,
            ipc::commands::debug::get_log_entries,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Hush")
        .run(|_app_handle, event| {
            // Intercept the runtime's "all webviews are gone, time
            // to exit" event (#328). On Linux/Windows that's the
            // default behaviour; macOS dodges it via Accessory
            // mode but only on the background-launch path. Hush's
            // close-hide pattern leaves zero visible webviews
            // after a normal close, so without this interceptor
            // the tray icon would vanish and the user would have
            // to relaunch from the LaunchAgent / start menu /
            // .desktop entry to recover.
            //
            // The flag distinguishes user-initiated quit (tray's
            // "Quit Hush", macOS app-menu's Quit) from runtime-
            // driven exit. Both quit menu items call
            // `request_user_quit` which sets the flag synchronously
            // before invoking `app.exit(0)`; by the time the
            // resulting `ExitRequested` event lands here, the
            // flag is already set and we let the exit proceed.
            // The flag stays `true` once set (no reset) — the
            // process is on its way out and there's no
            // "consumer" pattern that would care.
            match event {
                tauri::RunEvent::ExitRequested { ref api, .. } => {
                    if !USER_QUIT_REQUESTED.load(Ordering::SeqCst) {
                        api.prevent_exit();
                    }
                }
                // Dock-icon click on macOS while the main window is hidden
                // (#590). macOS dispatches `applicationShouldHandleReopen`
                // which Tauri surfaces as `RunEvent::Reopen`. Without a
                // handler the click does nothing — users have to find the
                // tray icon to recover the window, which breaks the
                // standard Dock-bring-to-front expectation.
                //
                // The macOS HIG specifies this should re-show the main
                // window unconditionally. `has_visible_windows` is
                // intentionally ignored: hidden HUD / menu-bar / debug
                // windows count as "visible" to Tauri's bookkeeping
                // (they're alive, just `.hide()`'d), so the
                // visible-windows check would return true even when the
                // user explicitly closed the main window.
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen { .. } => {
                    crate::tray::show_main_window(_app_handle);
                }
                _ => {}
            }
        });
}

/// Foreground-app poller for Meeting Mode auto-start (#112).
///
/// Ticks every `MEETING_AUTOSTART_POLL_INTERVAL`. Snapshots the
/// active window via `active-win-pos-rs::get_active_window`, runs
/// it through the existing `AppClassifier`, and asks
/// [`meeting::AutostartDecision::decide`] whether to start a
/// session. On a `Start` verdict it calls
/// `meeting_manager.start_manual` with the default sources
/// (mic + system audio when supported by the platform).
///
/// Loop never exits during normal operation; it terminates when
/// the Tauri runtime tears down at app shutdown.
/// Production [`meeting::ForegroundAppProbe`] backed by
/// `active-win-pos-rs`. Returns `None` on no-active-window errors
/// (lock screen, full-screen game) so the poller treats those as
/// "no transition" and doesn't churn `last_kind` on transient gaps.
struct ActiveWinProbe;

impl meeting::ForegroundAppProbe for ActiveWinProbe {
    fn current_app_name(&self) -> Option<String> {
        active_win_pos_rs::get_active_window()
            .ok()
            .map(|w| w.app_name)
    }
}

async fn run_meeting_autostart_poller(app: tauri::AppHandle) {
    use meeting::ForegroundAppProbe;
    use tauri::Emitter;
    use tauri::Manager;
    let mut ticker = tokio::time::interval(MEETING_AUTOSTART_POLL_INTERVAL);
    let mut last_kind: Option<meeting::MeetingAppKind> = None;
    // Per-app profile auto-apply (#427 Item 5 / #457). Tracks the
    // last app whose profile we activated so we emit
    // `app:profile-activated` only on transitions, not every tick.
    // Reset to `None` when the user focuses an app without a
    // profile, so re-focusing the original app retriggers.
    let mut last_profile_app: Option<String> = None;

    // Classifier table is constant for the life of the process
    // (default rules don't pick up runtime overrides — that's a
    // known limitation called out at `manager.rs`'s
    // `with_overrides` doc-comment). Cache once instead of
    // allocating ~50 string entries every 3 s.
    static CLASSIFIER: std::sync::OnceLock<meeting::AppClassifier> = std::sync::OnceLock::new();
    let classifier = CLASSIFIER.get_or_init(meeting::AppClassifier::default_table);
    let probe = ActiveWinProbe;

    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<ipc::AppState>() else {
            // State hasn't been managed yet — race against
            // setup. Try again on the next tick.
            continue;
        };

        // Per-app profile auto-apply (#427 Item 5). Independent of
        // the autostart-mode gate below — profile auto-apply is its
        // own opt-in (the user added the override + populated the
        // dropdowns), shouldn't require autostart mode = Always.
        // Skipped while a session is active so a mid-dictation
        // focus change doesn't trip the source/model swap; the
        // event will fire on the next tick after the user stops.
        if state.meeting_manager.active_session_id().is_none() {
            if let Some(focused) = probe.current_app_name() {
                if last_profile_app.as_ref() != Some(&focused) {
                    // Look up the override row. List is small
                    // (~handful of entries) and the read is
                    // cheap; refreshing per-tick keeps the
                    // poller stateless about the override
                    // table, so a panel edit is observable on
                    // the next tick without a refresh hook.
                    if let Ok(rows) = state.data.meeting_app_overrides.list().await {
                        if let Some(row) = rows.iter().find(|r| r.app_name == focused) {
                            let has_profile = row.preferred_audio_source.is_some()
                                || row.preferred_model_id.is_some();
                            if has_profile {
                                #[derive(Clone, serde::Serialize)]
                                #[serde(rename_all = "camelCase")]
                                struct ProfileActivatedPayload<'a> {
                                    app_name: &'a str,
                                    preferred_audio_source: Option<&'a str>,
                                    preferred_model_id: Option<&'a str>,
                                }
                                if let Err(e) = app.emit(
                                    "app:profile-activated",
                                    ProfileActivatedPayload {
                                        app_name: &row.app_name,
                                        preferred_audio_source: row
                                            .preferred_audio_source
                                            .as_deref(),
                                        preferred_model_id: row.preferred_model_id.as_deref(),
                                    },
                                ) {
                                    tracing::warn!(
                                        error = ?e,
                                        app_name = %focused,
                                        "failed to emit app:profile-activated"
                                    );
                                }
                                last_profile_app = Some(focused);
                            } else {
                                // No profile on this app —
                                // reset memory so refocusing
                                // the previous profile-app
                                // re-emits.
                                last_profile_app = None;
                            }
                        } else {
                            last_profile_app = None;
                        }
                    }
                }
            }
        }

        let mode = ipc::decode_autostart_mode(
            state
                .runtime_flags
                .meeting_autostart_mode
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let session_active = state.meeting_manager.active_session_id().is_some();

        let outcome =
            meeting::evaluate_autostart_tick(&probe, classifier, last_kind, mode, session_active);

        match outcome {
            meeting::TickOutcome::ResetMemory => {
                last_kind = None;
            }
            meeting::TickOutcome::NoChange => {
                // Probe failure or transient gap — keep last_kind
                // unchanged.
            }
            meeting::TickOutcome::UpdateMemory { last_kind: k } => {
                last_kind = Some(k);
            }
            meeting::TickOutcome::Start {
                app_name,
                last_kind: k,
            } => {
                last_kind = Some(k);

                // Pick the default capture sources. Mic always;
                // system audio if the platform supports it.
                // Mirrors the panel's default selection for
                // manual starts.
                let mic_source = audio::AudioSource::default_microphone();
                // Linux / Windows builds today have only the mic
                // source — system-audio capture lands under
                // #106 / #107. The cfg-gated push below is the
                // only mutator, so on those platforms `sources`
                // would warn `unused_mut` (Ubuntu CI runs clippy
                // with `-D warnings`); the branchless
                // construction sidesteps it.
                #[cfg(target_os = "macos")]
                let sources = vec![mic_source, audio::AudioSource::SystemAudio];
                #[cfg(not(target_os = "macos"))]
                let sources = vec![mic_source];

                // Snapshot the foreground window's title for
                // #242 — second active-win call instead of
                // extending the `ForegroundAppProbe` trait keeps
                // the trait minimal (the autostart-decision logic
                // genuinely only needs the app name; title is
                // pure metadata for the persisted row). The OS
                // call is single-millisecond synchronous, paid
                // only when we're about to start a session.
                let app_title = active_win_pos_rs::get_active_window()
                    .ok()
                    .map(|w| w.title.trim().to_owned())
                    .filter(|t| !t.is_empty());
                if let Err(e) = state
                    .meeting_manager
                    .start_manual(sources, Some(app_name.clone()), app_title)
                    .await
                {
                    // Most likely cause: mic permission denied.
                    // Log and keep the poller running — flipping
                    // the toggle off is a single-click recovery
                    // in Settings → Meeting.
                    tracing::warn!(
                        app_name,
                        error = ?e,
                        "auto-start meeting session failed"
                    );
                } else {
                    tracing::info!(app_name, "auto-started meeting session");
                }
            }
        }
    }
}

/// Tick interval for the foreground-app poller. 3 s is a good
/// balance: fast enough that "I clicked into Zoom" feels instant,
/// slow enough that idle CPU is unnoticeable. The OS APIs we're
/// hitting (`active-win-pos-rs::get_active_window`) are a single
/// IPC each.
const MEETING_AUTOSTART_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn is_background_launch_recognises_flag() {
        let args = vec![
            "/Applications/Hush.app".to_owned(),
            "--background".to_owned(),
        ];
        assert!(is_background_launch(args.into_iter()));
    }

    #[test]
    fn is_background_launch_rejects_missing_flag() {
        let args = vec!["/Applications/Hush.app".to_owned()];
        assert!(!is_background_launch(args.into_iter()));
    }

    #[test]
    fn is_background_launch_rejects_partial_match() {
        // A flag like `--background-frobulator` shouldn't trigger
        // background mode by accident — the match is exact.
        let args = vec![
            "/Applications/Hush.app".to_owned(),
            "--background-frobulator".to_owned(),
        ];
        assert!(!is_background_launch(args.into_iter()));
    }

    #[test]
    fn is_background_launch_rejects_equals_form() {
        // We deliberately don't accept `--background=true` —
        // the autostart plugin always passes the bare flag.
        let args = vec![
            "/Applications/Hush.app".to_owned(),
            "--background=true".to_owned(),
        ];
        assert!(!is_background_launch(args.into_iter()));
    }
}

#[cfg(test)]
mod migration_tests {
    use super::{migrate_legacy_app_data_dir, LEGACY_BUNDLE_ID};
    use std::fs;

    /// `tempfile::tempdir()` would be cleaner, but the workspace doesn't
    /// pull tempfile in for the lib crate yet. A scoped path under the
    /// per-test target dir is good enough — drop on test exit.
    struct TempRoot(std::path::PathBuf);
    impl TempRoot {
        fn new(name: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "hush-bundle-rename-test-{}-{}",
                name,
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn migration_moves_legacy_dir_when_new_path_absent() {
        let root = TempRoot::new("moves");
        let old = root.0.join(LEGACY_BUNDLE_ID);
        let new = root.0.join("io.github.khawkins98.hush");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("hush.db"), b"db-bytes").unwrap();

        migrate_legacy_app_data_dir(&new);

        assert!(!old.exists(), "legacy dir should be gone");
        assert!(new.exists(), "new dir should exist after migration");
        assert_eq!(fs::read(new.join("hush.db")).unwrap(), b"db-bytes");
    }

    #[test]
    fn migration_is_noop_when_legacy_dir_absent() {
        let root = TempRoot::new("noop-absent");
        let new = root.0.join("io.github.khawkins98.hush");

        migrate_legacy_app_data_dir(&new);

        assert!(!new.exists(), "no migration → no new dir created");
    }

    #[test]
    fn migration_leaves_both_when_new_path_already_present() {
        // Re-install / re-run scenario: the user has launched the
        // renamed app at least once (so the new dir exists), and a
        // legacy dir still lingers from a pre-rename install. Don't
        // clobber the new dir with old data; warn and leave both alone.
        let root = TempRoot::new("conflict");
        let old = root.0.join(LEGACY_BUNDLE_ID);
        let new = root.0.join("io.github.khawkins98.hush");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("hush.db"), b"old").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("hush.db"), b"new").unwrap();

        migrate_legacy_app_data_dir(&new);

        assert!(
            old.exists(),
            "legacy dir should remain when new path is populated"
        );
        assert_eq!(fs::read(new.join("hush.db")).unwrap(), b"new");
    }
}
