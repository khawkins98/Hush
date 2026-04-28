// Domain modules. Exposed at the crate root so integration tests and the
// IPC layer can address them by their public surface.
pub mod app_menu;
pub mod audio;
pub mod db;
pub mod diarization;
pub mod dictionary;
pub mod history;
pub mod hotkey;
pub mod hud;
pub mod ipc;
pub mod macos_perms;
pub mod meeting;
pub mod repository;
pub mod settings;
pub mod settings_window;
pub mod transcription;
pub mod tray;
pub mod updater;

use tauri::{Emitter, Manager};

/// Filename for the app's SQLite database, stored in the platform's
/// per-app data directory (e.g. `~/Library/Application Support/Hush/`
/// on macOS).
const DB_FILENAME: &str = "hush.db";

/// Subdirectory under the platform app-data dir where the model
/// picker scans for downloaded GGUF files. Auto-download (when it
/// lands) will write here; for now users put files here manually.
const MODELS_DIRNAME: &str = "models";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing here so service-construction errors (database
    // open, whisper model load) reach `RUST_LOG` consumers before the
    // Tauri event loop starts. `try_init` rather than `init` so re-runs
    // in tests (`cargo tauri dev`-restart-cycle) do not panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    tauri::Builder::default()
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
            Some(vec![]),
        ))
        // Updater plugin is deferred until #10 — registering it without a
        // `plugins.updater` block in tauri.conf.json (pubkey + endpoints)
        // panics at startup with "Error deserializing 'plugins.updater'".
        // We leave the dep + module stub in place so #10 can wire the
        // signing key and endpoints in one focused PR; until then, no
        // plugin is registered.
        //.plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // The platform app-data directory is only resolvable from a
            // Tauri `App` handle, so state construction has to live in
            // `setup` rather than at the top of `run`. Tauri's own async
            // runtime drives the SQLite open + migrations.
            let app_data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("resolve app-data dir: {e}"))?;
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
            let state = tauri::async_runtime::block_on(ipc::AppState::build_default(
                app_handle,
                &db_path,
                models_dir,
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
            app.manage(state);

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
            // rate (~100 Hz at 48 kHz / 480-frame chunks). At idle we
            // still tick — `current_level()` returns `0.0` while not
            // recording, the emit is cheap, and any HUD listeners get
            // a clean idle baseline.
            let app_for_pump = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker =
                    tokio::time::interval(std::time::Duration::from_millis(33));
                loop {
                    ticker.tick().await;
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
            ipc::commands::audio_list_sources,
            ipc::commands::open_settings,
            ipc::commands::start_dictation,
            ipc::commands::stop_dictation,
            ipc::commands::history_list,
            ipc::commands::history_search,
            ipc::commands::history_delete,
            ipc::commands::history_count,
            ipc::commands::replacements_list,
            ipc::commands::replacement_create,
            ipc::commands::replacement_update,
            ipc::commands::replacement_delete,
            ipc::commands::vocabulary_list,
            ipc::commands::vocabulary_create,
            ipc::commands::vocabulary_update,
            ipc::commands::vocabulary_delete,
            ipc::commands::models::model_list,
            ipc::commands::models::model_select,
            ipc::commands::models::model_download,
            ipc::commands::models::model_cancel_download,
            ipc::commands::models::model_remove,
            ipc::commands::get_first_run_completed,
            ipc::commands::mark_first_run_completed,
            ipc::commands::reset_first_run,
            ipc::commands::ptt_get_config,
            ipc::commands::ptt_set_config,
            ipc::commands::macos::open_macos_privacy_pane,
            ipc::commands::macos::diagnose_macos_permissions,
            ipc::commands::macos::reset_macos_permissions,
            ipc::commands::meeting::meeting_sessions_list,
            ipc::commands::meeting::meeting_session_get,
            ipc::commands::meeting::meeting_session_delete,
            ipc::commands::meeting::meeting_session_set_notes,
            ipc::commands::meeting::meeting_active_session,
            ipc::commands::meeting::meeting_start_manual,
            ipc::commands::meeting::meeting_stop_manual,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hush");
}
