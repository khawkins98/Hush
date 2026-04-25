// Domain modules. Exposed at the crate root so integration tests and the
// IPC layer (TODO(#9)) can address them by their public surface, and so
// dead-code warnings do not fire on items that are wired up in a later PR.
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
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![])
        .run(tauri::generate_context!())
        .expect("error while running Hush");
}
