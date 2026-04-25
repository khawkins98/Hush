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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialise tracing here so service-construction errors (e.g. the
    // whisper model failing to load on startup) reach `RUST_LOG` consumers
    // before the Tauri event loop starts. `try_init` rather than `init` so
    // re-runs in tests (`cargo tauri dev`-restart-cycle) do not panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let state = ipc::AppState::build_default();

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
        .manage(state)
        .setup(|app| {
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Hush");
}
