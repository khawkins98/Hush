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
  /// Backend → frontend (main): a mic source was lost mid-session
  /// and the pump has either fallen back to the system default or
  /// has no fallback. Payload: `{ sessionId, sourceKind, lostDevice,
  /// newDevice? }`. The meeting panel shows a banner with the lost
  /// device name and (if present) the fallback device name.
  AudioDeviceLost: "audio:device-lost",
  /// Backend → frontend (main): the original mic was detected on
  /// replug and the pump has swapped back. Payload: `{ sessionId,
  /// sourceKind, restoredDevice }`. Dismisses the device-lost banner.
  AudioDeviceRestored: "audio:device-restored",
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
  /// Frontend → all windows (broadcast): user picked an explicit
  /// theme (or reverted to System) in Settings → General →
  /// Appearance (#411 phase A). Payload is `"system" | "light"
  /// | "dark"`. Every window's root layout listens and re-applies
  /// the `data-theme` attribute on `<html>`. Canonical helpers
  /// live in `lib/theme.ts`.
  Theme: "hush:theme",
  /// Backend (profile auto-activate poller) → main window: a per-app
  /// audio profile (#427 Item 5) just activated because focus moved to
  /// an app with a populated profile. Fires only on transitions
  /// (the just-focused-app's profile differs from the last
  /// activation). Payload: `{ appName, preferredAudioSource,
  /// preferredModelId }`. Main window listener swaps the active
  /// source selector + invokes `model_select` for the model
  /// swap, then surfaces a transient "Switched to <app> profile."
  /// notice. Auto-apply is gated on `recording === false` so a
  /// mid-dictation focus change doesn't interrupt the active
  /// stream.
  AppProfileActivated: "app:profile-activated",
  /// Frontend → all windows (broadcast): user toggled the F5
  /// technical status line under the waveform (Settings → General
  /// → Advanced). Payload is `boolean`. The main window's
  /// ControlsSection listens and re-renders. Persistence is
  /// localStorage; this event only propagates *changes* across
  /// already-open windows. Canonical helpers live in
  /// `lib/status-line.ts`.
  StatusLine: "hush:status-line",
  /// Backend → settings window (debug tab): a new backend log
  /// entry is available. Payload is a `LogEntry` object. The
  /// DebugConsole component appends it to the in-page list.
  /// Subscribe *before* calling `get_log_entries` to guarantee no
  /// events are lost across the snapshot / live-stream gap (#532).
  LogEvent: "log:event",
  /// Backend → HUD window and main window: whisper.cpp inference
  /// progress during dictation transcription (integer 0–100).
  /// Throttled to every 5 percentage points in Rust to keep
  /// event-bus traffic low. The HUD shows "Processing… N%" in
  /// its label; the main window's RecordPanel renders a thin
  /// progress bar under the waveform during the transcribing
  /// phase (#566).
  TranscriptionProgress: "transcription:progress",
  /// Backend → all windows (broadcast): Hush's System Audio (Screen
  /// Recording TCC) permission was just confirmed via a real SCK
  /// probe after the user granted it in System Settings (#579).
  /// The main window listens to show the relaunch banner; other
  /// windows can use it to refresh permission state.
  ///
  /// Why a relaunch is needed: macOS caches the TCC deny in
  /// `mediaserverd`/`coreaudiod` for the lifetime of the current
  /// process — the grant takes effect only in a fresh process. See
  /// `learnings.md` for the full explanation.
  PermissionScreenRecordingGranted: "permission:screen-recording-granted",
} as const;
