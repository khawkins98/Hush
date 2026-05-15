// Domain modules. Exposed at the crate root so integration tests and the
// IPC layer can address them by their public surface.
pub mod app_menu;

// Replace macOS libmalloc's hold-forever freelist with mimalloc, which
// aggressively madvises freed pages back to the OS (#636). The `override`
// feature intercepts C/C++ malloc/free (whisper.cpp, ORT) too, not just
// Rust allocations. Without this, Physical Footprint stays pinned after
// meeting stop even though Drop fires correctly.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
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

/// Maximum time the graceful-quit coordinator waits for an active
/// dictation recording to stop before exiting anyway (#798).
const GRACEFUL_QUIT_DICTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Maximum time the graceful-quit coordinator waits for an active
/// meeting session to flush its tail and close before exiting anyway
/// (#798, #846). Sized to cover the streaming-finish timeout
/// (`STREAMING_FINISH_TIMEOUT` = 5 s × N sources) plus DB close overhead.
const GRACEFUL_QUIT_MEETING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Public helper called from the tray + app-menu Quit handlers.
/// Sets the flag synchronously, then spawns an async shutdown
/// coordinator that gracefully stops any active dictation recording
/// and/or meeting session before calling `app_handle.exit(0)` (#798,
/// #846).
///
/// The coordinator runs with bounded timeouts so a stuck session can
/// never block the quit indefinitely. The synchronous flag-set still
/// happens before the coordinator calls `exit`, so the
/// `RunEvent::ExitRequested` handler will find the flag set and allow
/// the exit to proceed exactly as before.
pub fn request_user_quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    USER_QUIT_REQUESTED.store(true, Ordering::SeqCst);
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        graceful_quit_coordinator(app_handle).await;
    });
}

/// Async coordinator for graceful quit. Stops active recordings with
/// bounded timeouts, then exits. Called only from `request_user_quit`.
async fn graceful_quit_coordinator<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    let state = match app.try_state::<ipc::AppState>() {
        Some(s) => s,
        None => {
            // AppState not yet registered — nothing to clean up.
            app.exit(0);
            return;
        }
    };

    // Stop active dictation audio capture (best-effort). We discard
    // the captured audio — the user is quitting, not submitting text.
    if state.audio.is_recording() {
        let audio = std::sync::Arc::clone(&state.audio);
        match tokio::time::timeout(
            GRACEFUL_QUIT_DICTATION_TIMEOUT,
            tokio::task::spawn_blocking(move || audio.stop()),
        )
        .await
        {
            Ok(Ok(Ok(_captured))) => {
                tracing::info!("graceful quit: dictation audio stopped");
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(error = ?e, "graceful quit: dictation audio stop error");
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    error = ?e,
                    "graceful quit: dictation audio stop task panicked"
                );
            }
            Err(_elapsed) => {
                tracing::warn!("graceful quit: dictation audio stop timed out");
            }
        }
    }

    // Stop active meeting session (flush tail, close DB row). This is
    // the core of #846 — without a graceful stop the pump task is
    // aborted by Drop, the tail flush is skipped, and the DB row's
    // `ended_at` is never set.
    if state.meeting_manager.active_session_id().is_some() {
        match tokio::time::timeout(
            GRACEFUL_QUIT_MEETING_TIMEOUT,
            state.meeting_manager.stop_manual(),
        )
        .await
        {
            Ok(Ok(())) => {
                tracing::info!("graceful quit: meeting session stopped");
            }
            Ok(Err(e)) => {
                tracing::warn!(error = ?e, "graceful quit: meeting stop error");
            }
            Err(_elapsed) => {
                tracing::warn!("graceful quit: meeting stop timed out");
            }
        }
    }

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

/// Bundle id used for filesystem locations. Mirrors `tauri.conf.json`'s
/// `identifier` field — kept as a const here so the pre-Tauri tracing
/// init can resolve `~/Library/Logs/<id>/` without depending on
/// `AppHandle::path()` (which only resolves inside the `setup` hook).
///
/// Gated to macOS because the only consumer (`resolve_log_dir`) is
/// also macOS-only. Ungated, Linux/Windows clippy under
/// `-D dead_code` rejects the const since the non-macOS
/// `resolve_log_dir` stub returns `None` without using it.
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "io.github.khawkins98.hush";

/// Resolve the on-disk log directory for the file appender.
///
/// macOS-only by design: the project is macOS-primary, so we don't
/// litter Linux/Windows filesystems with a logs dir for those
/// compile-only paths. Returns `None` on non-macOS so the caller
/// skips the file layer cleanly.
///
/// Path: `~/Library/Logs/io.github.khawkins98.hush/`. This is the
/// macOS HIG-recommended location for app-specific logs and shows
/// up in Console.app under "Reports" → the bundle id.
#[cfg(target_os = "macos")]
pub(crate) fn resolve_log_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = std::path::PathBuf::from(home)
        .join("Library")
        .join("Logs")
        .join(BUNDLE_ID);
    Some(dir)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn resolve_log_dir() -> Option<std::path::PathBuf> {
    None
}

/// Wire up the tracing layers (stderr fmt, optional on-disk file fmt,
/// in-memory DebugLogLayer) and return a guard that must outlive the
/// process for the non-blocking file writer to flush cleanly on
/// shutdown.
///
/// Returns `None` for the guard when the file layer is unavailable
/// (non-macOS, `HUSH_LOG_FILE=off`, or log-dir creation failed) — the
/// other two layers still init in those cases.
///
/// The two registry chains (with-file vs. without-file) are spelled
/// out separately on purpose: tracing-subscriber's `Layered<...>` type
/// changes shape with every `.with(...)`, so an `Option<Layer>` doesn't
/// compose cleanly without trait-object boxing that adds its own
/// type-system pain. Two short branches are easier to read than one
/// clever one.
fn init_tracing(
    debug_log: crate::debug_log::DebugLogState,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    fn env_filter() -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    }

    let stderr_layer = tracing_subscriber::fmt::layer().with_filter(env_filter());
    let debug_layer = crate::debug_log::DebugLogLayer::new(debug_log);

    // Opt-out via env var so CI / one-off binaries don't accumulate
    // logs in the user's Library. Default-on for normal runs, where
    // post-hoc grepping is the whole point of having this layer at all.
    let want_file_log = !matches!(
        std::env::var("HUSH_LOG_FILE").as_deref(),
        Ok(v) if v.eq_ignore_ascii_case("off") || v == "0"
    );

    let file_appender = if want_file_log {
        build_file_appender()
    } else {
        None
    };

    if let Some((appender, dir)) = file_appender {
        let (non_blocking, guard) = tracing_appender::non_blocking(appender);
        let file_layer = tracing_subscriber::fmt::layer()
            // ANSI colours don't render in tail / less; turn them
            // off so the file is plain text, copy-pasteable into
            // bug reports.
            .with_ansi(false)
            .with_writer(non_blocking)
            .with_filter(env_filter());
        let _ = tracing_subscriber::registry()
            .with(stderr_layer)
            .with(file_layer)
            .with(debug_layer)
            .try_init();
        // Print the path so a user grepping for "where did logs
        // go" sees it even before the first tracing event reaches
        // the in-app console.
        eprintln!("hush: writing daily-rolling logs to {}", dir.display());
        Some(guard)
    } else {
        let _ = tracing_subscriber::registry()
            .with(stderr_layer)
            .with(debug_layer)
            .try_init();
        None
    }
}

/// Resolve the log dir, ensure it exists, and build a daily-rolling
/// appender pointed at it. Returns `None` if the dir can't be
/// resolved (non-macOS) or created (permissions, disk full).
fn build_file_appender() -> Option<(
    tracing_appender::rolling::RollingFileAppender,
    std::path::PathBuf,
)> {
    let dir = resolve_log_dir()?;
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "hush: could not create log dir {}: {e}; file logging disabled",
            dir.display()
        );
        return None;
    }
    // Daily rotation with prefix `hush.log`. `tracing_appender` rolls
    // by appending the date, so files look like `hush.log.2026-05-07`.
    // No automatic retention — files accumulate; if that becomes a
    // pain we can plug in `RollingFileAppender::builder().max_log_files(...)`
    // (added in tracing-appender 0.2.4).
    let appender = tracing_appender::rolling::daily(&dir, "hush.log");
    Some((appender, dir))
}

/// Check and strip the macOS quarantine xattr on the first post-DMG
/// launch, then exec()-restart so TCC sees a clean process identity.
///
/// Returns immediately (as a no-op) when:
/// - Running after a strip (`HUSH_QUARANTINE_STRIPPED` is set in env).
/// - No quarantine xattr is present (non-DMG installs).
///
/// The `current_exe()` resolution failure propagates as an `Err` so the
/// `setup` hook aborts cleanly; `exec()` failure is logged and silently
/// swallowed (the quarantine xattr was already stripped so the next fresh
/// relaunch presents a clean identity).
///
/// Gated to macOS because other platforms have no quarantine concept.
#[cfg(target_os = "macos")]
fn handle_quarantine_strip() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("HUSH_QUARANTINE_STRIPPED").is_ok() {
        tracing::info!(
            im_status = ?crate::permissions::read_all().input_monitoring,
            "returned from quarantine-strip exec() restart"
        );
        return Ok(());
    }
    if crate::permissions::strip_app_quarantine() {
        tracing::info!("quarantine stripped; exec-restarting app to establish clean TCC identity");
        // exec() replaces the current process image (same PID, no fork).
        // Unlike spawn()+exit(), this bypasses LaunchServices so the
        // quarantine events daemon cannot re-add the xattr before the
        // first instruction of the new image — the root cause of the
        // infinite restart loop observed on macOS 26 (learnings.md
        // 2026-05-13 "infinite restart loop"). HUSH_QUARANTINE_STRIPPED=1
        // is baked into the replacement image's environment; if exec()
        // fails and falls through, the next fresh relaunch will re-enter
        // this function and try again (the xattr is already gone, so
        // `strip_app_quarantine` returns false — no loop).
        use std::os::unix::process::CommandExt as _;
        let exe =
            std::env::current_exe().map_err(|e| format!("quarantine restart: get exe: {e}"))?;
        let err = std::process::Command::new(&exe)
            .args(std::env::args_os().skip(1))
            .env("HUSH_QUARANTINE_STRIPPED", "1")
            .exec();
        // exec() only returns on failure.
        tracing::warn!(
            error = %err,
            "quarantine-strip restart via exec() failed; continuing with quarantined identity"
        );
    }
    Ok(())
}

/// Wire all window-level setup: hide-on-close for main/debug,
/// macOS drag enablement for menu-bar/hud, zoom-button removal for
/// main, background-launch hide + Accessory policy, and LaunchAgent
/// path reconciliation.
///
/// Called once from the `setup` hook after `AppState` is managed.
/// Accesses `AppState` internally via `app.try_state()` so callers
/// do not need to hold an `ipc::AppState` borrow across the `&mut App`
/// call. Every sub-step is best-effort.
fn setup_windows<R: tauri::Runtime>(app: &mut tauri::App<R>) {
    // Hide-on-close for main and debug (#263, #543). Tauri 2's default
    // destroys the window on red-✕; without an intercept the user would
    // close the main window expecting it to hide and find Hush had quit.
    // The debug console window is included so closing its red-✕ hides
    // it rather than destroying the webview — this prevents macOS from
    // stranding focus on the desktop and keeps the window's log buffer
    // alive for the next open.
    //
    // Pairs with the `RunEvent::ExitRequested` interceptor in `run()`
    // (#328): on Linux/Windows the runtime's default is to quit when the
    // last window goes away, so hiding all windows would otherwise drop
    // the tray icon along with the app. The interceptor blocks every
    // runtime-driven exit; the only exit paths are the tray's "Quit
    // Hush" item and the macOS app-menu's Quit item, both of which call
    // `request_user_quit` to set a flag the interceptor honours.
    for label in ["main", "debug"] {
        if let Some(window) = app.get_webview_window(label) {
            let win_clone = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // Always prevent the destroy. If the subsequent
                    // `hide()` fails the window stays visible — that's a
                    // strictly better failure mode than letting Tauri
                    // destroy the only surface the user has for the
                    // window. The user can quit via tray / menu / ⌘Q if
                    // they actually wanted out.
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

    // Borderless-window drag enablement (#427 Item 1). On macOS,
    // `decorations: false` windows have their NSWindow movable styleMask
    // bit stripped — Tauri's `data-tauri-drag-region` and
    // `startDragging()` then become silent no-ops. Restore movability via
    // explicit AppKit calls so users can drag the popover and HUD pill
    // from any non-interactive area.
    #[cfg(target_os = "macos")]
    for label in ["menu-bar", "hud"] {
        if let Some(window) = app.get_webview_window(label) {
            unlock_macos_window_drag(&window);
        }
    }

    // Hide the zoom (green) traffic-light button on the main window.
    // `maximizable: false` disables it but leaves a greyed-out circle;
    // removing it entirely is cleaner.
    #[cfg(target_os = "macos")]
    if let Some(window) = app.get_webview_window("main") {
        hide_macos_zoom_button(&window);
    }

    // Background-launch behaviour (#268). When the LaunchAgent fires
    // Hush at login, we don't want to pop the main window. If
    // `--background` is present, hide the main window and switch to
    // Accessory activation policy so the Dock icon doesn't appear.
    #[cfg(target_os = "macos")]
    if is_background_launch(std::env::args()) {
        if let Some(main_win) = app.get_webview_window("main") {
            let _ = main_win.hide();
        }
        app.set_activation_policy(tauri::ActivationPolicy::Accessory);
        tracing::info!("background launch: main window hidden, activation policy = Accessory");
    }

    // LaunchAgent path reconciliation (#271). Re-register on every
    // startup where autostart is currently enabled so the plist always
    // points at `current_exe()`. Cheap (one ~500-byte fs::write) and
    // idempotent. If `enable()` fails, store a flag in AppState — the
    // Settings panel reads it and surfaces a "path is stale" warning row
    // with a retry button (#317).
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
                        state
                            .runtime_flags
                            .autostart_path_stale
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

/// Spawn the long-running background tasks that run for the full app
/// lifetime:
/// 1. Orphan-session reconciliation — stamps `ended_at` on sessions
///    left open by a previous crash or kill.
/// 2. HUD level-meter pump — emits `audio:level` at ~30 Hz while
///    recording is active.
/// 3. Per-app profile auto-activation poller — detects foreground-app
///    changes and applies per-app source/model overrides.
/// 4. (macOS only) Event-driven meeting auto-start — listens for
///    CoreAudio HAL device-active notifications.
///
/// All spawns are fire-and-forget via `tauri::async_runtime::spawn`.
/// Called once from the `setup` hook after `AppState` is managed.
/// Takes a concrete `tauri::AppHandle` rather than a generic `App<R>`
/// because the async task helpers (`run_profile_autoactivate_poller`,
/// `run_meeting_detection_task`) are defined with the default Wry
/// runtime and are not generic.
fn spawn_background_tasks(handle: tauri::AppHandle, state: &ipc::AppState) {
    // Orphan-session reconciliation (#249, #329). Sessions left open by a
    // previous process that exited without `stop_manual` (kill, OS crash,
    // panic) get their `ended_at` stamped now. Spawned (not block_on'd)
    // so the SELECT + UPDATEs don't hold the synchronous setup hook open
    // while the first paint is waiting (#329).
    let meeting_manager = std::sync::Arc::clone(&state.meeting_manager);
    tauri::async_runtime::spawn(async move {
        meeting_manager.reconcile_orphan_sessions().await;
    });

    // HUD level-meter pump (#21). Reads the latest RMS from the audio
    // backend at ~30 Hz and emits `audio:level` so the HUD page can
    // animate a bar. Activity-gated (#329): skip the emit when nothing is
    // recording so we don't emit ~2.6M IPC events/day at idle.
    let audio = std::sync::Arc::clone(&state.audio);
    let handle_for_pump = handle.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(33));
        loop {
            ticker.tick().await;
            if !audio.is_recording() {
                continue;
            }
            let level = audio.current_level();
            if let Err(e) = handle_for_pump.emit("audio:level", level) {
                // No listener attached yet (HUD window hidden) is not an
                // error per se, but trace keeps it out of the default log.
                tracing::trace!(error = ?e, "emit audio:level failed");
            }
        }
    });

    // Per-app profile auto-activation poller (#427 / #457). Watches the
    // foreground app on a 3-second tick; emits `app:profile-activated`
    // when a per-app override matches so the frontend can update its
    // source/model dropdowns.
    let handle_for_profile = handle.clone();
    tauri::async_runtime::spawn(async move {
        run_profile_autoactivate_poller(handle_for_profile).await;
    });

    // Event-driven meeting auto-start (#665). Listens for CoreAudio HAL
    // property changes on `kAudioDevicePropertyDeviceIsRunningSomewhere`
    // instead of polling every 3 s. macOS only; other platforms compile
    // clean with no equivalent HAL API.
    #[cfg(target_os = "macos")]
    {
        let handle_for_detection = handle.clone();
        tauri::async_runtime::spawn(async move {
            run_meeting_detection_task(handle_for_detection).await;
        });
    }
}

/// Register the global toggle hotkey and the PTT rdev listener.
///
/// Both registrations are best-effort: failures are logged but do not
/// prevent startup — device-list and button-driven dictation keep
/// working even if the OS refuses the shortcut.
///
/// PTT is skipped when Input Monitoring is `NotDetermined` (calling
/// CGEventTapCreate in that state causes macOS 26 to auto-create a TCC
/// Deny entry, making the grant unreachable without a dev-reset — see
/// learnings.md 2026-05-13) or `Denied` (user explicitly revoked IM).
/// On non-macOS platforms `NotApplicable` lets PTT start unconditionally.
///
/// Takes a concrete `tauri::AppHandle` (same rationale as
/// `spawn_background_tasks` — the hotkey module's registration
/// functions expect the default Wry runtime).
fn register_hotkeys(handle: tauri::AppHandle, state: &ipc::AppState) {
    if let Err(e) = hotkey::register_default(&handle) {
        let msg = format!("{e:#}");
        tracing::error!(error = ?e, "failed to register default toggle hotkey");
        if let Ok(mut guard) = state.hotkey_toggle_error.lock() {
            *guard = Some(msg);
        }
    }

    let im_status = crate::permissions::read_all().input_monitoring;
    tracing::info!(
        status = ?im_status,
        "input monitoring status at startup (IOHIDCheckAccess)"
    );
    if im_status == crate::permissions::PermissionStatus::Granted
        || im_status == crate::permissions::PermissionStatus::NotApplicable
    {
        if let Err(e) = hotkey::register_ptt_listener(
            &handle,
            std::sync::Arc::clone(&state.ptt_combo),
            std::sync::Arc::clone(&state.ptt_active),
            std::sync::Arc::clone(&state.ptt_listener_spawned),
        ) {
            tracing::error!(error = ?e, "failed to start PTT listener");
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing here so service-construction errors (database
    // open, whisper model load) reach `RUST_LOG` consumers before the
    // Tauri event loop starts.
    //
    // We compose three layers:
    //   1. fmt → stderr (`RUST_LOG`-filtered, the dev-loop default).
    //   2. fmt → daily-rolling file in
    //      `~/Library/Logs/io.github.khawkins98.hush/` so post-hoc
    //      grepping is possible. macOS-only because the project is
    //      macOS-primary; on other platforms this layer is skipped.
    //      Disable with `HUSH_LOG_FILE=off` (e.g. when running CI or
    //      a one-off binary that shouldn't litter ~/Library/Logs).
    //   3. DebugLogLayer — captures events into a ring buffer and
    //      forwards them to the frontend via the `log:event` Tauri
    //      event once the AppHandle is available (#532). Same shape
    //      as the in-app Debug Console; this is additive, not a
    //      replacement.
    //
    // `try_init` rather than `init` so re-runs in tests
    // (`cargo tauri dev`-restart-cycle) do not panic.
    let debug_log = crate::debug_log::DebugLogState::new();
    let _file_log_guard = init_tracing(debug_log.clone());

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
            // Strip the macOS quarantine xattr on the first post-DMG launch
            // and exec()-restart for a clean TCC identity. Must run before
            // any real initialization — TCC identity is baked in at process
            // launch time from the quarantine state.
            #[cfg(target_os = "macos")]
            handle_quarantine_strip()?;

            // Log TCC identity + all permission statuses to the persistent
            // log. The cdhash shown here is what macOS uses as the TCC row
            // key — compare it between a "grant works" and a "grant doesn't
            // stick" launch to diagnose identity mismatches.
            #[cfg(target_os = "macos")]
            crate::permissions::log_tcc_identity("startup");

            // The platform app-data dir is only resolvable from a Tauri
            // `App` handle, so state construction lives in `setup` rather
            // than at the top of `run`. Tauri's own async runtime drives the
            // SQLite open + migrations.
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app-data dir: {e}"))?;

            // One-shot migration from the pre-#525 bundle identifier. Runs
            // before any directory creation so the old DB + models are in
            // place when the rest of setup looks for them.
            migrate_legacy_app_data_dir(&app_data_dir);

            let db_path = app_data_dir.join(DB_FILENAME);
            let models_dir = app_data_dir.join(MODELS_DIRNAME);

            // Pre-create the models directory so the picker has a stable
            // place to point users at, even before any model has been added.
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
            app.manage(state);

            // Wire windows first (needs &mut app), then obtain the managed
            // state reference for the remaining helpers.
            setup_windows(app);
            let state = app.state::<ipc::AppState>();
            spawn_background_tasks(app.handle().clone(), state.inner());
            app_menu::apply(app.handle());
            tray::install(app.handle());
            register_hotkeys(app.handle().clone(), state.inner());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::commands::dictation::audio_list_sources,
            ipc::commands::system::show_main_window,
            ipc::commands::system::open_debug_window,
            ipc::commands::system::get_log_dir,
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
            ipc::commands::dictionary::list_packs,
            ipc::commands::dictionary::enable_pack,
            ipc::commands::dictionary::disable_pack,
            ipc::commands::dictionary::get_language_style,
            ipc::commands::dictionary::set_language_style,
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
            ipc::commands::system::get_startup_timings,
            ipc::commands::system::open_url,
            ipc::commands::system::reveal_log_dir,
            ipc::commands::ptt::ptt_get_config,
            ipc::commands::ptt::ptt_set_config,
            ipc::commands::ptt::get_toggle_hotkey_status,
            ipc::commands::permissions::open_macos_privacy_pane,
            ipc::commands::permissions::diagnose_macos_permissions,
            ipc::commands::permissions::reset_macos_permissions,
            ipc::commands::permissions::get_permission_health,
            ipc::commands::permissions::confirm_permission,
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
                tauri::RunEvent::ExitRequested { ref api, .. }
                    if !USER_QUIT_REQUESTED.load(Ordering::SeqCst) =>
                {
                    api.prevent_exit();
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

/// Per-app profile auto-activation poller (#427 / #457).
///
/// Ticks every [`PROFILE_AUTOACTIVATE_POLL_INTERVAL`]. When the foreground
/// app has a per-app override row with a preferred audio source or model,
/// it emits `app:profile-activated` so the frontend can update its
/// source/model dropdowns. Only fires on transitions (app change), not every
/// tick. Paused while a meeting session is active — a mid-session focus
/// change shouldn't trigger a source/model swap.
///
/// Independent of meeting auto-start mode: the user doesn't need to enable
/// Always to benefit from per-app profiles.
async fn run_profile_autoactivate_poller(app: tauri::AppHandle) {
    use tauri::Emitter;
    use tauri::Manager;
    let mut ticker = tokio::time::interval(PROFILE_AUTOACTIVATE_POLL_INTERVAL);
    // Tracks the last app whose profile we activated so we emit
    // `app:profile-activated` only on transitions, not every tick.
    // Reset to `None` when the user focuses an app without a profile,
    // so re-focusing the original app retriggers.
    let mut last_profile_app: Option<String> = None;

    loop {
        ticker.tick().await;
        let Some(state) = app.try_state::<ipc::AppState>() else {
            continue;
        };

        // Skip while a session is active.
        if state.meeting_manager.active_session_id().is_some() {
            continue;
        }

        let Some(focused) = active_win_pos_rs::get_active_window()
            .ok()
            .map(|w| w.app_name)
        else {
            continue;
        };

        if last_profile_app.as_ref() == Some(&focused) {
            continue;
        }

        let Ok(rows) = state.data.meeting_app_overrides.list().await else {
            continue;
        };

        if let Some(row) = rows.iter().find(|r| r.app_name == focused) {
            let has_profile =
                row.preferred_audio_source.is_some() || row.preferred_model_id.is_some();
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
                        preferred_audio_source: row.preferred_audio_source.as_deref(),
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
                // No profile on this app — reset memory so refocusing
                // the previous profile-app re-emits.
                last_profile_app = None;
            }
        } else {
            last_profile_app = None;
        }
    }
}

/// Tick interval for the per-app profile auto-activation poller.
/// 3 s is a good balance: fast enough that "I clicked into an app with
/// a profile" feels instant, slow enough that idle CPU is unnoticeable.
const PROFILE_AUTOACTIVATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Event-driven meeting auto-start task (#665, macOS only).
///
/// Listens for CoreAudio HAL property changes on
/// `kAudioDevicePropertyDeviceIsRunningSomewhere` instead of polling the
/// foreground app every 3 s. When any input device activates the task:
/// 1. Checks the user's mode (must be `Always`).
/// 2. Ensures no session is already active.
/// 3. Reads the frontmost app and classifies it as Meeting/Other/Media.
/// 4. On `Meeting` classification, calls `start_manual`.
///
/// A `session_emitted` bool prevents duplicate starts within one
/// mic-activation cycle (the HAL may re-fire while the session is starting
/// up). Resets when the mic goes quiet.
#[cfg(target_os = "macos")]
async fn run_meeting_detection_task(app: tauri::AppHandle) {
    use meeting::mic_camera_monitor::{evaluate_mic_state, MicCameraMonitor, MicStateOutcome};
    use tauri::Manager;

    let mut monitor = MicCameraMonitor::new();

    let mut session_emitted = false;
    // On the first iteration skip the wait so that if the mic is already
    // active when Hush launches (e.g. a Zoom call is already in progress)
    // we evaluate and auto-start immediately rather than waiting for the
    // next HAL notification.
    let mut is_first_iteration = true;

    tracing::info!("meeting detection task started");

    loop {
        if is_first_iteration {
            is_first_iteration = false;
        } else {
            // Wait for any HAL property change notification.
            monitor.wait_for_change().await;
            tracing::debug!("meeting detection: HAL notification received");
        }
        // Re-enumerate input devices after every wake (and once at startup).
        // Handles hot-plug / unplug: stale DeviceListenerHandles are dropped
        // (which unregisters their CoreAudio listeners) and new handles are
        // installed for any freshly discovered input devices.
        monitor.refresh_devices();

        let Some(state) = app.try_state::<ipc::AppState>() else {
            tracing::warn!("meeting detection: AppState not yet available, skipping");
            continue;
        };

        let mic_active = monitor.is_any_device_active();
        let mode = ipc::decode_autostart_mode(
            state
                .runtime_flags
                .meeting_autostart_mode
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        let session_active = state.meeting_manager.active_session_id().is_some();

        // Rebuild classifier each tick from user overrides so that settings
        // changes take effect on the next HAL event without a restart (#812).
        let classifier = {
            let overrides = state
                .data
                .meeting_app_overrides
                .list()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|o: crate::meeting::MeetingAppOverride| (o.app_name, o.kind))
                .collect::<Vec<_>>();
            meeting::AppClassifier::with_overrides(overrides)
        };

        let frontmost_app = active_win_pos_rs::get_active_window()
            .ok()
            .map(|w| w.app_name);
        let app_kind = frontmost_app
            .as_deref()
            .map(|name| classifier.classify(name))
            .unwrap_or(meeting::MeetingAppKind::Other);

        tracing::debug!(
            mic_active,
            mode = ?mode,
            session_active,
            session_emitted,
            frontmost_app = frontmost_app.as_deref().unwrap_or("<none>"),
            app_kind = ?app_kind,
            "meeting detection: evaluating state"
        );

        let outcome = evaluate_mic_state(&meeting::mic_camera_monitor::MicStateInputs {
            mic_is_active: mic_active,
            mode,
            session_active,
            session_emitted,
            frontmost_app_kind: app_kind,
            frontmost_app_name: frontmost_app.clone().unwrap_or_default(),
        });

        tracing::debug!(outcome = ?outcome, "meeting detection: outcome");

        match outcome {
            MicStateOutcome::Start { app_name } => {
                session_emitted = true;

                let mic_source = audio::AudioSource::default_microphone();
                // Try mic + system-audio first. If system-audio tap fails
                // (permission denied, CoreAudio already in use, SCK helper
                // not running) degrade to mic-only so the user at least
                // gets partial transcription rather than a silent failure
                // (#807).
                let full_sources = vec![mic_source, audio::AudioSource::SystemAudio];

                // Snapshot the window title for the persisted session row.
                let app_title = active_win_pos_rs::get_active_window()
                    .ok()
                    .map(|w| w.title.trim().to_owned())
                    .filter(|t| !t.is_empty());

                // Load vocabulary prompt + replacement rules at auto-start
                // time (#913). Same snapshot semantics as the manual path in
                // `meeting_start_manual`: if the user edits their dictionary
                // mid-session the change takes effect on the next session.
                let vocab_prompt =
                    crate::ipc::commands::dictation::load_vocabulary_prompt(&state).await;
                let replacement_rules = std::sync::Arc::new(
                    crate::ipc::commands::dictation::load_replacement_rules(&state).await,
                );
                let dict_opts = crate::meeting::SessionDictOpts {
                    vocab_prompt,
                    replacement_rules,
                };

                let start_result = state
                    .meeting_manager
                    .start_manual(
                        full_sources,
                        Some(app_name.clone()),
                        app_title.clone(),
                        dict_opts.clone(),
                    )
                    .await;
                let start_result = if start_result.is_err() {
                    tracing::warn!(
                        app_name,
                        "auto-start with system-audio failed, retrying mic-only"
                    );
                    let mic_only = vec![audio::AudioSource::default_microphone()];
                    state
                        .meeting_manager
                        .start_manual(mic_only, Some(app_name.clone()), app_title, dict_opts)
                        .await
                } else {
                    start_result
                };

                if let Err(e) = start_result {
                    tracing::warn!(
                        app_name,
                        error = ?e,
                        "auto-start meeting session failed"
                    );
                    // Don't hold `session_emitted = true` on failure —
                    // the next HAL event should retry.
                    session_emitted = false;
                } else {
                    tracing::info!(app_name, "auto-started meeting session");
                    // Show the recording HUD — the session-started event
                    // (emitted by SessionManager::start_manual) tells the
                    // frontend to refresh, but the HUD is Tauri/window-
                    // specific so it must be driven here rather than inside
                    // the manager. Same logic as `meeting_start_manual`.
                    if state
                        .runtime_flags
                        .hud_enabled
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        crate::hud::show_async(&app);
                        if let Err(e) = crate::hud::set_state(
                            &app,
                            crate::hud::HudState::Recording {
                                started_at_ms: crate::hud::now_unix_ms(),
                            },
                        ) {
                            tracing::warn!(
                                error = ?e,
                                "emit hud:state(recording) failed for auto-start"
                            );
                        }
                    }
                }
            }
            MicStateOutcome::AutoStop => {
                // Mic went quiet while we hold an auto-started session.
                // Stop the session so users aren't left with a ghost
                // recording after their call ends. Uses the same helper
                // as the manual Stop button so transcribers and diarizer
                // are rebuilt in the background, ready for the next call.
                session_emitted = false;
                tracing::info!("meeting detection: mic inactive — auto-stopping session");
                if let Err(e) = crate::ipc::commands::meeting::stop_meeting_and_rebuild_transcriber(
                    &app, &state,
                )
                .await
                {
                    tracing::warn!(
                        error = ?e,
                        "meeting detection: auto-stop failed"
                    );
                }
            }
            MicStateOutcome::ResetSessionEmitted => {
                // Mic went quiet — reset so the next activation can start
                // a new session.
                session_emitted = false;
            }
            MicStateOutcome::Idle => {}
        }
    }
}

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
