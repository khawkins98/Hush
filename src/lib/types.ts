// Shared type definitions for the page components.
//
// These mirror the camelCase serde renames on the Rust side. Kept
// here (rather than inlined in each component) so the parent page and
// the panel children can hand the same shape back and forth without
// duplicate declarations drifting.

// `AudioDevice` (the bare device shape that backs the legacy
// `list_input_devices` IPC command) is intentionally NOT exported
// here. The frontend only needs `AudioSourceListing`, which carries
// the kind + capability flags the picker dispatches on; a parallel
// `AudioDevice` import would mean two mic shapes that could drift.
// The Rust-side `AudioDevice` struct still exists as the transport
// for the transitional `list_input_devices` command — its sole
// frontend-side consumer (the e2e mock's default) types its return
// value inline.

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
export type DictationResult = { text: string; foreground: ForegroundApp | null };
export type IpcError = { kind: string; message?: string };

export type HistoryEntry = {
  id: number;
  transcript: string;
  appName: string | null;
  windowTitle: string | null;
  model: string;
  durationMs: number | null;
  createdAt: string;
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

// Meeting Mode (Phase C scaffold; refs #33 / #109). Wire shapes
// mirror the Rust types in `src-tauri/src/meeting/mod.rs`. Today
// the panel reads these via `meeting_sessions_list` /
// `meeting_session_get` but the underlying repo is empty until
// the streaming pump (#110) starts inserting sessions.

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

// Best-effort diagnostic snapshot. macOS does not expose programmatic
// read access to the TCC permission state, so the backend can't
// truthfully say "microphone is granted / denied" — instead it returns
// the bundle id (so the user can find Hush in System Settings) and
// hint copy that points them at the right pane. `canReset` is the
// platform gate: true on macOS, false elsewhere, so the frontend can
// hide the section entirely on non-macOS builds.
export type MacosPermissionDiagnostic = {
  bundleId: string;
  microphoneHint: string;
  inputMonitoringHint: string;
  canReset: boolean;
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
