// Shared type definitions for the page components.
//
// These mirror the camelCase serde renames on the Rust side. Kept
// here (rather than inlined in each component) so the parent page and
// the panel children can hand the same shape back and forth without
// duplicate declarations drifting.

// Discriminator for `AudioSource` and `AudioSourceListing`. Mirrors
// the kebab-case serde tag on the Rust enum so the wire shape matches
// without bespoke conversion at the boundary.
export type AudioSourceKind = "microphone" | "system-audio";

// Wire shape of one audio source the user can pick from. Mic devices
// always have `isSupported: true`; the system-audio entry mirrors the
// backend's `supports_system_audio()` capability check, so a platform
// that hasn't shipped ScreenCaptureKit / WASAPI loopback / PulseAudio
// monitor support yet renders the option as disabled with a "coming
// soon" affordance instead of letting the user pick it and hit a
// runtime error.
export type AudioSourceListing = {
  kind: AudioSourceKind;
  id: string;
  name: string;
  isDefault: boolean;
  isSupported: boolean;
};

// Argument shape for `start_dictation`. Wraps the `AudioSource` Rust
// enum's serde representation: `{ kind: "microphone", deviceId: ... }`
// or `{ kind: "system-audio" }`. The default-mic case can pass `null`
// instead of a wrapped object — the backend falls back to the system
// default microphone.
export type AudioSource =
  | { kind: "microphone"; deviceId: string | null }
  | { kind: "system-audio" };
export type ForegroundApp = { appName: string; windowTitle: string };
export type DictationResult = {
  text: string;
  foreground: ForegroundApp | null;
  /// Wall-clock length of the captured audio, in milliseconds.
  /// Surfaced in ResultBlock so the user always sees "Recorded
  /// for X.Xs" — including when transcription found nothing
  /// recognisable. `null` only when the capture format was
  /// degenerate; never seen in practice.
  durationMs: number | null;
};
export type KnownIpcError =
  | { kind: "audio"; message: string }
  | { kind: "audio-device-lost"; message: string }
  | { kind: "transcription"; message: string }
  | { kind: "transcription-unavailable" }
  | { kind: "clipboard"; message: string }
  | { kind: "settings"; message: string }
  | { kind: "history"; message: string }
  | { kind: "replacements"; message: string }
  | { kind: "meeting-sessions"; message: string }
  | { kind: "meeting-session-active" }
  | { kind: "permission-denied"; message: string }
  | { kind: "updater-unavailable" }
  | { kind: "internal"; message: string };

// Keep an unknown-string fallback so the frontend still renders a
// reasonable error box when a newer backend adds a kind this build
// doesn't know yet.
export type IpcError = KnownIpcError | { kind: string; message?: string };
export type KnownIpcErrorKind = KnownIpcError["kind"];

// Tagged result from the manual "Check for updates" probe. AboutTab
// and the e2e mocks share this shape; keeping it here avoids the tab
// drifting from the mock default.
export type UpdateCheckResult =
  | { kind: "upToDate"; current: string }
  | {
      kind: "updateAvailable";
      current: string;
      latest: string;
      releaseUrl: string;
    }
  | { kind: "checkFailed"; reason: string };

export type HistoryEntry = {
  id: number;
  transcript: string;
  appName: string | null;
  windowTitle: string | null;
  model: string;
  durationMs: number | null;
  createdAt: string;
  /** True for recordings that were too short to transcribe. */
  ignored: boolean;
};

// Aggregate dictation stats (#293). Powers the "you've dictated N
// words across M sessions" tile bar above the History list. All
// four numbers are derived from a single SQL pass on the history
// table; the IPC name is `get_dictation_stats`.
//
// `totalChars` doubles as the keystrokes-saved approximation —
// every character spoken is one keystroke not typed. The UI
// labels the value as "~N keystrokes" so the imprecision (it
// under-counts modifier keys + autocorrect) reads as honest.
//
// Time saved is derived in the frontend from `wordCount`:
// `totalMinutes = wordCount / 40` (40 wpm baseline).
export type DictationStats = {
  sessionCount: number;
  wordCount: number;
  totalRecordingMs: number;
  totalChars: number;
};

export type ReplacementRule = {
  id: number;
  findText: string;
  replaceText: string;
  sortOrder: number;
};

export type VocabularyTerm = {
  id: number;
  term: string;
};

export type PackStatus = {
  slug: string;
  name: string;
  description: string;
  vocabularyCount: number;
  replacementCount: number;
  enabled: boolean;
};

export type LanguageStyle = 'american' | 'british' | 'oxford';

// Meeting Mode (refs #33 / #109). Wire shapes mirror the Rust
// types in `src-tauri/src/meeting/mod.rs`. Sessions are populated
// by the SessionManager chunking pump; the panel renders an empty
// state until the user runs their first meeting.

export type MeetingAppKind = "meeting" | "media" | "other";

export type MeetingSession = {
  id: number;
  appName: string;
  appKind: MeetingAppKind;
  startedAt: string;
  endedAt: string | null;
  speakerCount: number | null;
  utteranceCount: number;
  notes: string | null;
  /// Audio sources captured at session-open time (#242). Kind
  /// labels in pick order — currently `"mic"` and/or `"system"`.
  /// Persisted via migration 0004; `null` for legacy rows
  /// created before the migration ran.
  sources: string[] | null;
  /// Active window's title at session-open (#242 follow-up).
  /// Useful when `appName` is uninformative (e.g. a browser
  /// hosting YouTube / Meet / Notion); the panel renders this
  /// as a subtitle when distinct from `appName`. `null` when
  /// the OS query couldn't resolve a title (lock screen,
  /// fullscreen game) or when the row pre-dates migration
  /// 0005.
  appTitle: string | null;
};

export type PersistedUtterance = {
  id: number;
  sessionId: number;
  startedAtMs: number;
  endedAtMs: number;
  speakerLabel: string | null;
  text: string;
  isFinal: boolean;
};

export type MeetingSessionDetail = {
  session: MeetingSession;
  utterances: PersistedUtterance[];
  /// In-flight partial utterances from the streaming pump (#108
  /// PR3+). Empty for closed sessions and for sessions whose
  /// pump hasn't produced a partial yet. The frontend renders
  /// these alongside `utterances` with an italic / reduced-opacity
  /// treatment to distinguish in-flight revisions from settled
  /// finals (#108 PR4). Wire shape is `transcription::Utterance`
  /// from the Rust side — same fields as `PersistedUtterance`
  /// minus `id` / `sessionId`.
  currentPartials: StreamingUtterance[];
};

/// In-flight (non-final) utterance the streaming pump produces.
/// Mirrors `crate::transcription::Utterance` on the Rust side. No
/// DB id — partials live in memory only and never get persisted.
/// `isFinal` is always `false` on the partials surfaced through
/// `MeetingSessionDetail.currentPartials`; the frontend can rely
/// on that without a runtime check.
export type StreamingUtterance = {
  text: string;
  startedAtMs: number;
  endedAtMs: number;
  isFinal: boolean;
  speakerLabel: string | null;
};

// Snapshot of which meeting session (if any) is currently active.
// `active === null` means no session is in flight; the panel renders
// the Start button. A non-null id means a session is open; the panel
// renders the Stop button + a live "session in progress" line.
export type ActiveMeetingSession = {
  active: number | null;
};

// User-supplied per-app classifier override (Phase E, #112). The
// classifier consults these before the static defaults — an entry
// here with the same `appName` as a default wins.
export type MeetingAppOverride = {
  appName: string;
  kind: MeetingAppKind;
  createdAt: string;
  // Per-app audio profile (#427 Item 5 foundation slice). `null`
  // means "use the global default"; populated values pin a
  // preferred audio source / Whisper model to this app for a
  // future foreground-watcher iteration to apply on focus. The
  // Settings panel surface for these fields ships in a follow-up
  // PR; this slice only adds the storage + read path.
  preferredAudioSource?: string | null;
  preferredModelId?: string | null;
};

// Built-in classification table entry (#320). Mirrors the Rust
// `BuiltinAppEntry` struct returned by
// `meeting_app_classifier_defaults`. The Settings panel renders
// these read-only so users can see what's already covered before
// adding a redundant override.
export type BuiltinAppEntry = {
  appName: string;
  kind: MeetingAppKind;
};

// Mirrors `ModelCard` on the Rust side. `metadata` is flattened by
// serde so all the catalog fields land at the top level.
export type ModelCard = {
  id: string;
  displayName: string;
  filename: string;
  sizeMb: number;
  speedRating: number;
  accuracyRating: number;
  description: string;
  isDefault: boolean;
  isDownloaded: boolean;
  isSelected: boolean;
  expectedPath: string;
};

// Notice pill shown after the user picks a model. Three flavours:
//   - "loaded"         : backend hot-swapped; ready to record now.
//   - "needs-download" : selection persisted but the model file is
//                        not on disk yet — user has to Download.
//   - "needs-restart"  : the file is on disk but hot-swap returned
//                        false (whisper feature off, or some other
//                        backend reason). Restart picks it up. Rare
//                        in practice; covers the edge case so the
//                        message stays accurate.
//   - null             : no notice currently visible.
export type ModelSelectNotice = "loaded" | "needs-download" | "needs-restart" | null;

// Live grant state for one TCC permission. The backend reads these
// programmatically (AVFoundation, CoreGraphics, IOKit) without
// triggering OS prompts. `not-applicable` is the non-macOS sentinel.
export type PermissionStatus =
  | "granted"
  | "denied"
  | "not-determined"
  | "not-applicable";

export type PermissionStatuses = {
  microphone: PermissionStatus;
  screenRecording: PermissionStatus;
  inputMonitoring: PermissionStatus;
};

// Diagnostic snapshot. `statuses` is the live grant state; the hint
// copy stays even when everything is granted so the user has the
// recovery copy if permissions later get revoked. `canReset` gates
// the in-app reset button — true on macOS only.
export type MacosPermissionDiagnostic = {
  bundleId: string;
  microphoneHint: string;
  inputMonitoringHint: string;
  canReset: boolean;
  statuses: PermissionStatuses;
};

// Outcome of running `tccutil reset` across the three TCC categories
// Hush touches (Microphone, ListenEvent, Accessibility). `anyReset`
// is true if at least one category had an entry to clear; `summary`
// is the user-facing message the UI surfaces verbatim.
export type MacosPermissionResetResult = {
  anyReset: boolean;
  summary: string;
};

// Per-card transient state for the model download flow. The two
// parallel `Map<id, …>`s keep per-row status independent of the
// catalog array's order.
export type DownloadProgress = { received: number; total: number | null };

// Push-to-talk configuration. `combo` is the canonical key list
// (e.g. `["RightMeta"]` or `["RightMeta", "RightShift"]`). Each
// entry is a `PttKey` enum variant name from the backend, in
// canonical sorted order. `enabled` mirrors the persisted toggle.
// `listenerRunning` is a runtime signal: when the user toggles
// Enabled ON in a session that started with PTT off, the rdev
// listener can't be started mid-session — the UI shows a "restart
// Hush" hint when listenerRunning is false but enabled is true.
export type PttConfig = {
  combo: string[];
  enabled: boolean;
  listenerRunning: boolean;
};

// Result of `get_toggle_hotkey_status` (#904). `error` is null when the
// Ctrl+⌥+H registration succeeded; otherwise holds the error message.
// Mirrors the Rust return type `Option<String>` from ptt.rs.
export type ToggleHotkeyStatus = string | null;

// Status of the wespeaker speaker-embedding model on disk.
// Returned by `get_diarizer_model_status` (#304); read by Settings
// → Meeting → Speakers on mount + after every download lifecycle
// event so the UI can render "model not installed", "downloading",
// or "ready" states accurately.
//
// Mirrors the Rust `DiarizeModelStatus` struct in
// `src-tauri/src/ipc/commands/mod.rs` — keep field names + types
// in sync per the four-place IPC sync rule (CLAUDE.md).
export type DiarizerModelStatus = {
  downloaded: boolean;
  /// Catalog display name ("wespeaker ResNet34-LM"). Added in
  /// #351 for the Speakers panel's installed-model details.
  displayName: string;
  sizeMb: number;
  sha256: string;
  expectedPath: string;
  /// Upstream URL the model was downloaded from. Linked from the
  /// Speakers panel so the user can read the model card (#351).
  sourceUrl: string;
};

// Format selectors for the per-row meeting export popover (#357
// phase 3b). Lowercase tokens are deliberately chosen to match the
// backend's `MeetingExportFormat` serde shape — the IPC accepts
// these strings verbatim.
export type MeetingExportFormat = "text" | "csv" | "json";

// Source-of-rows choice for the bulk-export options dialog (#357
// phase 3c). `"auto"` mirrors whichever filter chip is active in
// the panel; the explicit kinds force a scope regardless of chip.
// Lowercase tokens match the backend's `ExportKind` serde shape.
export type BundleKind = "auto" | "dictation" | "meetings" | "both";

// User-confirmed selection from `ExportOptionsDialog`.
export type BundleSelection = {
  kind: BundleKind;
  meetingFormat: MeetingExportFormat;
};

// Three-state permission health (#378). `confirmed` = currently
// granted; `stale` = was granted, OS now reports false (cert /
// bundle-id rotation invalidated the TCC entry); `not-granted` =
// no prior grant on record. `not-applicable` is the non-macOS
// branch. Lowercase + kebab-case matches the Rust enum's serde
// shape.
export type PermissionHealth =
  | "confirmed"
  | "stale"
  | "not-granted"
  | "not-applicable";

export type PermissionsHealth = {
  microphone: PermissionHealth;
  screenRecording: PermissionHealth;
  inputMonitoring: PermissionHealth;
};

export type PermissionHealthResponse = {
  health: PermissionsHealth;
};
