// Shared type definitions for the page components.
//
// These mirror the camelCase serde renames on the Rust side. Kept
// here (rather than inlined in each component) so the parent page and
// the panel children can hand the same shape back and forth without
// duplicate declarations drifting.

export type AudioDevice = { id: string; name: string; isDefault: boolean };
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

export type MacosPermissionDiagnostic = {
  bundleId: string;
  microphoneHint: string;
  inputMonitoringHint: string;
  canReset: boolean;
};

export type MacosPermissionResetResult = {
  anyReset: boolean;
  summary: string;
};

// Per-card transient state for the model download flow. The two
// parallel `Map<id, …>`s keep per-row status independent of the
// catalog array's order.
export type DownloadProgress = { received: number; total: number | null };
