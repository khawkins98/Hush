// Default mock bus for `npm run dev` (which runs `vite dev --mode mock`).
//
// Seeds `window.__hush_e2e.invoke` with realistic, lightly-stateful
// fake data so the plain-browser dev server is an interactive
// PLAYGROUND — populated history, granted permissions, working
// settings toggles, an installed model — with no Tauri backend.
//
// Relationship to `_mock.ts`:
//   - `_mock.ts` seeds a *minimal, deterministic* set for Playwright
//     tests (and throws on unmocked commands to catch drift).
//   - This file seeds a *populated, forgiving* set for hands-on UI
//     work (unknown commands fall back to `undefined` via a Proxy).
// Both route through the same `window.__hush_e2e.invoke` seam and the
// same stub modules; they just differ in intent. Keep command shapes
// in sync with `src/lib/types.ts` per the four-place IPC sync rule.
//
// `seedMockBus()` is called once from `core-stub.ts`. It no-ops if
// `window.__hush_e2e` already exists, so under Playwright (which sets
// it first via addInitScript) this seed is skipped entirely.

import type { ModelCard } from "../../../src/lib/types";

type Handler = (args?: Record<string, unknown>) => unknown;

function isoMinutesAgo(min: number): string {
  return new Date(Date.now() - min * 60_000).toISOString();
}

export function seedMockBus(): void {
  if (typeof window === "undefined") return;
  if ((window as unknown as { __hush_e2e?: unknown }).__hush_e2e) return;

  // ── lightly-stateful stores (so toggles/deletes stick per session) ──
  const settings: Record<string, unknown> = {
    hud: true,
    soundCues: false,
    cueStart: true,
    cueComplete: true,
    threads: 4,
    micGainDb: 0,
    autostart: "always",
    diarization: false,
    speakerIdentity: false,
  };

  let history: Array<Record<string, unknown>> = [
    { id: 5, transcript: "Let's ship the brand refresh and circle back on the icon export.", createdAt: isoMinutesAgo(4), ignored: false, model: "ggml-base.bin", durationMs: 5200, source: null },
    { id: 4, transcript: "Remember to self-host the Recursive font so it works offline.", createdAt: isoMinutesAgo(38), ignored: false, model: "ggml-base.bin", durationMs: 3400, source: null },
    { id: 3, transcript: "Quick note: the duotone needs higher contrast on the sidebar.", createdAt: isoMinutesAgo(95), ignored: false, model: "ggml-small.bin", durationMs: 2750, source: null },
    { id: 2, transcript: "Testing push-to-talk with the right control key.", createdAt: isoMinutesAgo(180), ignored: false, model: "ggml-base.bin", durationMs: 1900, source: null },
    { id: 1, transcript: "First transcription on the new build.", createdAt: isoMinutesAgo(1440), ignored: false, model: "ggml-base.bin", durationMs: 1200, source: null },
  ];

  const models: ModelCard[] = [
    { id: "base", displayName: "Whisper Base", filename: "ggml-base.bin", sizeMb: 142, speedRating: 4, accuracyRating: 2, description: "Fast, good for short dictation.", isDefault: true, isDownloaded: true, isSelected: true, expectedPath: "/mock/models/ggml-base.bin" },
    { id: "small", displayName: "Whisper Small", filename: "ggml-small.bin", sizeMb: 466, speedRating: 3, accuracyRating: 3, description: "Balanced speed and accuracy.", isDefault: false, isDownloaded: true, isSelected: false, expectedPath: "/mock/models/ggml-small.bin" },
    { id: "medium", displayName: "Whisper Medium", filename: "ggml-medium.bin", sizeMb: 1500, speedRating: 2, accuracyRating: 4, description: "Higher accuracy, slower.", isDefault: false, isDownloaded: false, isSelected: false, expectedPath: "/mock/models/ggml-medium.bin" },
  ];

  let replacements = [
    { id: 1, findText: "teh", replaceText: "the", sortOrder: 0 },
    { id: 2, findText: "wanna", replaceText: "want to", sortOrder: 1 },
  ];
  let vocabulary = [
    { id: 1, term: "SvelteKit" },
    { id: 2, term: "whisper.cpp" },
  ];

  const grantedHealth = {
    microphone: "granted",
    screenRecording: "granted",
    inputMonitoring: "granted",
  };

  const handlers: Record<string, Handler> = {
    // first-run + settings
    get_first_run_completed: () => true,
    mark_first_run_completed: () => undefined,
    reset_first_run: () => undefined,
    get_hud_enabled: () => settings.hud,
    set_hud_enabled: (a) => void (settings.hud = a?.enabled ?? settings.hud),
    get_sound_cues_enabled: () => settings.soundCues,
    set_sound_cues_enabled: (a) => void (settings.soundCues = a?.enabled ?? settings.soundCues),
    get_sound_cue_start_enabled: () => settings.cueStart,
    set_sound_cue_start_enabled: (a) => void (settings.cueStart = a?.enabled ?? settings.cueStart),
    get_sound_cue_complete_enabled: () => settings.cueComplete,
    set_sound_cue_complete_enabled: (a) => void (settings.cueComplete = a?.enabled ?? settings.cueComplete),
    preview_sound_cue: () => undefined,
    get_inference_threads: () => settings.threads,
    set_inference_threads: (a) => void (settings.threads = a?.threads ?? settings.threads),
    get_mic_gain_db: () => settings.micGainDb,
    set_mic_gain_db: (a) => void (settings.micGainDb = a?.db ?? settings.micGainDb),
    get_meeting_autostart_mode: () => settings.autostart,
    set_meeting_autostart_mode: (a) => void (settings.autostart = a?.mode ?? settings.autostart),
    get_diarization_enabled: () => settings.diarization,
    set_diarization_enabled: (a) => void (settings.diarization = a?.enabled ?? settings.diarization),
    get_speaker_identity_enabled: () => settings.speakerIdentity,
    set_speaker_identity_enabled: (a) => void (settings.speakerIdentity = a?.enabled ?? settings.speakerIdentity),
    speaker_list: () => [],
    get_diarizer_model_status: () => ({
      downloaded: true,
      displayName: "wespeaker ResNet34-LM",
      sizeMb: 26,
      sha256: "mock",
      expectedPath: "/mock/models/voxceleb_resnet34_LM.onnx",
      sourceUrl: "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
    }),

    // about / debug
    check_for_updates: () => ({ kind: "upToDate", current: "0.9.0" }),
    get_app_version: () => "0.9.0-mock",
    get_build_info: () => ({ version: "0.9.0-mock", tauriVersion: "2.11.1", buildTimestamp: Date.now() }),
    get_startup_timings: () => [],
    get_log_entries: () => [],
    get_log_dir: () => null,
    "plugin:app|version": () => "0.9.0-mock",
    "plugin:app|name": () => "Hush",
    "plugin:app|tauri_version": () => "2.11.1",

    // permissions (all granted → clean "ready" UI)
    get_permission_health: () => ({ health: grantedHealth }),
    get_macos_permission_diagnostics: () => ({
      bundleId: "io.github.khawkins98.hush",
      microphoneHint: "",
      inputMonitoringHint: "",
      canReset: false,
      statuses: grantedHealth,
    }),
    reset_macos_permissions: () => ({ anyReset: false, summary: "" }),
    confirm_permission: () => undefined,

    // audio
    audio_list_sources: () => [
      { kind: "microphone", id: "Built-in Microphone", name: "Built-in Microphone", isDefault: true, isSupported: true },
      { kind: "system-audio", id: "system", name: "System audio", isDefault: false, isSupported: true },
    ],

    // dictation
    start_dictation: () => undefined,
    stop_dictation: () => ({ text: "This is mock dictated text from the dev playground.", durationMs: 2300, foreground: { appName: "Hush", windowTitle: "Hush" } }),

    // history (stateful)
    history_list: () => history,
    history_search: (a) => {
      const q = String(a?.query ?? "").toLowerCase();
      return q ? history.filter((h) => String(h.transcript).toLowerCase().includes(q)) : history;
    },
    history_count: () => history.length,
    history_delete: (a) => void (history = history.filter((h) => h.id !== a?.id)),
    history_set_name: () => undefined,
    history_clear: () => {
      const n = history.length;
      history = [];
      return n;
    },
    get_dictation_stats: () => ({ sessionCount: history.length, wordCount: 134, totalRecordingMs: 47000, totalChars: 812 }),

    // replacements + vocabulary (stateful)
    replacements_list: () => replacements,
    replacement_create: (a) => {
      const row = { id: Date.now(), findText: String(a?.findText ?? ""), replaceText: String(a?.replaceText ?? ""), sortOrder: replacements.length };
      replacements = [...replacements, row];
      return row;
    },
    replacement_update: () => undefined,
    replacement_delete: (a) => void (replacements = replacements.filter((r) => r.id !== a?.id)),
    vocabulary_list: () => vocabulary,
    vocabulary_create: (a) => {
      const row = { id: Date.now(), term: String(a?.term ?? "") };
      vocabulary = [...vocabulary, row];
      return row;
    },
    vocabulary_update: () => undefined,
    vocabulary_delete: (a) => void (vocabulary = vocabulary.filter((v) => v.id !== a?.id)),

    // packs
    list_packs: () => [
      { slug: "dev-general", name: "Developer — General", description: "Common software-development terms and corrections.", vocabularyCount: 45, replacementCount: 16, enabled: false },
      { slug: "business", name: "Business", description: "Meeting-room language and workplace corrections.", vocabularyCount: 29, replacementCount: 7, enabled: false },
    ],
    enable_pack: () => undefined,
    disable_pack: () => undefined,

    // models (stateful selection)
    model_list: () => models,
    model_download: () => undefined,
    model_cancel_download: () => undefined,
    model_remove: () => undefined,
    model_select: (a) => {
      for (const m of models) m.isSelected = m.id === a?.id;
      return undefined;
    },

    // meeting sessions (unified history feed). Empty playground — no
    // recorded meetings and no active session.
    meeting_sessions_list: () => [],
    meeting_sessions_search: () => [],
    meeting_active_session: () => ({ active: null }),
    meeting_session_get: () => null,
    meeting_session_delete: () => undefined,
    meeting_session_set_name: () => undefined,
    meeting_session_export: () => undefined,
    meeting_start_manual: () => undefined,
    meeting_stop_manual: () => undefined,
    meeting_app_classifier_defaults: () => [],
    history_export_row_csv: () => undefined,
  };

  (window as unknown as { __hush_e2e: { invoke: Record<string, Handler> } }).__hush_e2e = {
    invoke: new Proxy(handlers, {
      // Forgiving: unknown commands don't throw (unlike the Playwright
      // stub). List-shaped commands fall back to `[]` so a missing
      // handler never crashes a `.map`; everything else to `undefined`.
      get: (target, prop: string) =>
        prop in target
          ? target[prop]
          : /(_list|_search|_defaults)$/.test(prop)
            ? () => []
            : () => undefined,
    }),
  };

  // eslint-disable-next-line no-console
  console.info("%c[hush] mock IPC active", "color:#f49e17;font-weight:bold", "— vite --mode mock. Fake data only; no real backend.");
}
