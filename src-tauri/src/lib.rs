// Domain modules. Exposed at the crate root so integration tests and the
// IPC layer can address them by their public surface.
pub mod audio;
pub mod db;
pub mod dictionary;
pub mod history;
pub mod hotkey;
pub mod ipc;
pub mod transcription;
pub mod updater;

use tauri::Manager;

/// Filename for the app's SQLite database, stored in the platform's
/// per-app data directory (e.g. `~/Library/Application Support/Hush/`
/// on macOS).
const DB_FILENAME: &str = "hush.db";

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
        .plugin(tauri_plugin_opener::init())
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
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

            tracing::info!(path = %db_path.display(), "opening database");

            let state = tauri::async_runtime::block_on(ipc::AppState::build_default(&db_path))
                .map_err(|e| format!("build app state: {e:#}"))?;
            app.manage(state);

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
            if let Err(e) = hotkey::register_ptt_listener(app.handle()) {
                tracing::error!(error = ?e, "failed to start PTT listener");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::commands::list_input_devices,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hush");
}
