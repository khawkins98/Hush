// Hotkey module — global push-to-talk and toggle-record shortcuts.
//
// Concept inspired by VoiceInk's KeyboardShortcuts-based hotkey handling.
// Reimplemented from observed public behaviour; no source code referenced. See §13.8 of the PRD.
//
// Responsibilities:
//   - Register a toggle-record shortcut via `tauri-plugin-global-shortcut`.
//   - Register a push-to-talk shortcut using `rdev` (key-down / key-up events).
//   - Emit `recording:start` and `recording:stop` Tauri events to drive audio capture.
//   - Allow the user to rebind both shortcuts from Settings.
//
// Note: global hotkeys under Wayland are compositor-dependent. GNOME is the primary
// supported target for Linux v1; other compositors degrade gracefully. See §10 of the PRD.

// TODO(#5): implement push-to-talk via rdev and toggle-record via tauri-plugin-global-shortcut
