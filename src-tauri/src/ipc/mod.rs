// IPC module — Tauri command handlers exposed to the frontend.
//
// Responsibilities:
//   - Re-export all `#[tauri::command]` functions from the domain modules.
//   - Capture the foreground app name and window title via `active-win-pos-rs` at recording start.
//   - Write the final transcription string to the system clipboard via `tauri-plugin-clipboard-manager`.
//   - Fire a "Ready to paste" native notification via `tauri-plugin-notification`.
//
// All OS-touching behaviour is behind trait objects so unit tests can mock at this seam.
// See §13.5 of the PRD.

// TODO(#7): wire up Tauri commands for audio, transcription, history, dictionary, and settings
