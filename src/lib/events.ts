/**
 * Centralised Tauri event names.
 *
 * Each entry is the literal string the backend uses with
 * `app_handle.emit(…)` or `Window::emit(…)`. Importing from here
 * (rather than typing the string at every call site) means a typo
 * is a TypeScript error instead of a silent listener that never
 * fires, and a rename touches one line instead of grep-and-pray.
 *
 * The constants are an immutable object literal rather than an
 * enum so call sites can pass them straight to `listen` / `emit`
 * without `as string` ceremony.
 *
 * Backend-side: these names are also baked into Rust strings.
 * Keep the two sides in sync; the four-place IPC sync rule from
 * `CLAUDE.md` applies the same way to events.
 */
export const Events = {
  /// Backend → frontend: hotkey toggle (`Ctrl+⌥/Alt+H` by default)
  /// fired. Frontend dispatches start-vs-stop based on its own
  /// recording state. See `+page.svelte`.
  HotkeyToggle: "hotkey:toggle",
  /// Backend → frontend: push-to-talk key down. Start dictation.
  HotkeyPttPress: "hotkey:ptt-press",
  /// Backend → frontend: push-to-talk key up. Stop dictation.
  HotkeyPttRelease: "hotkey:ptt-release",
  /// Frontend → frontend (broadcast): the main window's recording
  /// state changed. The HUD window listens to drive its visibility.
  UiRecordingState: "ui:recording-state",
  /// Backend → HUD window: the audio capture's RMS level for the
  /// last frame, in 0..1. Drives the HUD pill's level meter.
  AudioLevel: "audio:level",
  /// Backend → frontend (main): user picked a section from the
  /// native macOS menu bar. Payload is `"dictation" | "meetings" |
  /// "history"`. Sidebar uses this to flip the active tab.
  MenuGotoSection: "menu:goto-section",
  /// Frontend → settings window (broadcast): which tab to surface
  /// when Settings opens (or, if it's already open, switch to).
  SettingsGotoTab: "settings:goto-tab",
  /// Backend → all windows (broadcast): a model download is in
  /// flight. Payload is `{ id, received, total | null }`. The
  /// settings window's picker drives the progress bar.
  ModelDownloadProgress: "model:download-progress",
  /// Backend → all windows (broadcast): a model download finished
  /// successfully. Payload is `{ id }`. Main window uses this to
  /// clear its "no model installed" banner.
  ModelDownloadDone: "model:download-done",
  /// Backend → all windows (broadcast): a model download failed.
  /// Payload is `{ id, error }`. Settings window surfaces the
  /// error chip on the matching card.
  ModelDownloadFailed: "model:download-failed",
  /// Backend → frontend (main): a meeting-mode source (mic /
  /// system-audio) stopped capturing mid-session — TCC revoke,
  /// device unplug, etc. Payload is `{ kind: "microphone" |
  /// "system-audio", reason }`. Surfaces a struck-through chip
  /// in the active-session source line.
  MeetingSourceFailed: "meeting:source-failed",
  /// Backend → settings window: result of a Check for Updates
  /// probe fired from the macOS menu (#265). Payload is the
  /// `UpdateCheckResult` tagged union. The Settings About tab
  /// listens for this to render the result inline when the
  /// probe was triggered from the menu rather than the in-tab
  /// button.
  UpdaterResult: "updater:result",
  /// Backend → HUD window: lifecycle state for the recording
  /// HUD overlay (#291). Payload is `"recording"` or
  /// `"processing"`. Drives the HUD's render branch so the user
  /// sees a continuous "still working" signal across the
  /// transcription gap, instead of the HUD vanishing before
  /// the clipboard is updated.
  HudState: "hud:state",
} as const;
