// Default mock state for Playwright e2e tests. Test specs import
// `installMocks(page)` from here and pass per-test overrides on top
// of the defaults.
//
// Why a single shared default rather than per-test wholesale mocks:
// the dictation page calls a half-dozen `invoke`s on mount alone
// (history list, replacements list, vocabulary list, model list,
// settings get, audio_list_sources, get_first_run_completed). If
// every test redeclared all of them the fixtures would drift; the
// shared default gives every spec a working app baseline and lets
// each spec override only the commands its assertions care about.

import type { Page } from "@playwright/test";

import type { DiarizerModelStatus, UpdateCheckResult } from "../../src/lib/types";

const DEFAULT_DIARIZER_MODEL_STATUS = {
  downloaded: true,
  displayName: "wespeaker ResNet34-LM",
  sizeMb: 26,
  sha256:
    "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068",
  expectedPath:
    "/Users/test/Library/Application Support/com.hush.dev/models/voxceleb_resnet34_LM.onnx",
  sourceUrl:
    "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
} satisfies DiarizerModelStatus;

const DEFAULT_UPDATE_CHECK_RESULT = {
  kind: "upToDate",
  current: "0.1.0",
} satisfies UpdateCheckResult;

export interface InvokeOverrides {
  [cmd: string]: (args: Record<string, unknown> | undefined) => unknown;
}

/**
 * Inject the e2e stub bus and a default set of `invoke` handlers.
 * Call this BEFORE `page.goto(...)` — the stubs are read on first
 * `invoke` from app code, so they need to exist before navigation.
 *
 * Per-test overrides win over defaults. Setting an override to a
 * function that throws lets a test simulate an IPC failure for that
 * specific command without disturbing the others.
 */
export async function installMocks(
  page: Page,
  overrides: InvokeOverrides = {},
): Promise<void> {
  // The handlers map is serialised across the bridge as `[cmd, fn]`
  // pairs because Playwright cannot transfer functions directly —
  // the rebuilt object on the page side gives the stub real
  // callables.
  const overrideEntries: Array<[string, string]> = Object.entries(overrides).map(
    ([k, v]) => [k, v.toString()],
  );

  await page.addInitScript(
    ({
      overrideStrings,
      defaultDiarizerModelStatus,
      defaultUpdateCheckResult,
    }: {
      overrideStrings: Array<[string, string]>;
      defaultDiarizerModelStatus: DiarizerModelStatus;
      defaultUpdateCheckResult: UpdateCheckResult;
    }) => {
      const defaults: Record<string, (args?: unknown) => unknown> = {
      // ---- first-run / settings ----
      get_first_run_completed: () => true,
      mark_first_run_completed: () => undefined,
      reset_first_run: () => undefined,
      // HUD-overlay-enabled toggle (Settings → General). Default
      // matches the backend's "on by default" behaviour so the
      // checkbox renders checked. Specs that exercise the toggle
      // override per-test.
      get_hud_enabled: () => true,
      set_hud_enabled: () => undefined,
      get_sound_cues_enabled: () => false,
      set_sound_cues_enabled: () => undefined,
      // Per-event sound-cue sub-toggles (#463). Default true to
      // mirror the backend's "fire everything the master allows"
      // behaviour for installs without an explicit row.
      get_sound_cue_start_enabled: () => true,
      set_sound_cue_start_enabled: () => undefined,
      get_sound_cue_complete_enabled: () => true,
      set_sound_cue_complete_enabled: () => undefined,
      // Preview-cue button (#498). Default no-op — specs that
      // don't exercise the preview button just need the IPC to
      // be present so the Playwright mock-completeness check
      // passes. The real handler bypasses the toggle gate and
      // calls `audio_cues::play_bytes` directly.
      preview_sound_cue: () => undefined,
      // Whisper inference threads (Settings → General → Performance,
      // #255). Default 4 mirrors the backend default.
      get_inference_threads: () => 4,
      set_inference_threads: () => undefined,
      get_mic_gain_db: () => 0,
      set_mic_gain_db: () => undefined,
      // Debug log console (#532). Default returns empty array so
      // the DebugTab renders an empty (but valid) log view.
      get_log_entries: () => [],
      // Meeting auto-start mode (Settings → Meeting). Default
      // matches the backend's "off" default; specs that exercise
      // the dropdown override per-test.
      get_meeting_autostart_mode: () => "off",
      set_meeting_autostart_mode: () => undefined,
      // Diarization toggle (Settings → Meeting → Speakers, #111).
      // Default matches the backend's "off" default; specs that
      // exercise the toggle override per-test.
      get_diarization_enabled: () => false,
      set_diarization_enabled: () => undefined,
      // Diarizer model status (#301). Default is "downloaded" so
      // the toggle is interactable in specs that don't care about
      // the missing-model state. Specs that exercise the download
      // affordance flip this to `downloaded: false`. Field shape
      // mirrors `DiarizerModelStatus` in `src/lib/types.ts` —
      // keep them in sync per the four-place IPC sync rule.
      get_diarizer_model_status: () => ({ ...defaultDiarizerModelStatus }),
      download_diarizer_model: () => undefined,
      // Remove the installed model (#351). No-op default; specs
      // that exercise the click override per-test.
      remove_diarizer_model: () => undefined,
      // Manual update probe (#223). Default to "up to date" so
      // specs that don't override get a stable result if the
      // user clicks the button.
      check_for_updates: () => ({ ...defaultUpdateCheckResult }),
      // App version string for the debug issue-report generator.
      get_app_version: () => "0.0.0-test",
      // Auto-update install (#10). Default to the typed
      // not-configured gate-error (#497) so specs that don't
      // override see the friendly fallback copy + manual
      // release-notes link rather than the download-progress UI.
      // Specs that exercise the install success / failure
      // branches override per-test.
      install_pending_update: () => {
        throw { kind: "updater-unavailable" };
      },
      ptt_get_config: () => ({
        combo: ["RightMeta"],
        enabled: false,
        listenerRunning: false,
      }),
      ptt_set_config: () => undefined,
      // Autostart plugin commands. The plugin's JS layer routes
      // through `plugin:autostart|<verb>` commands. The settings
      // window's General tab calls these on mount + toggle.
      "plugin:autostart|is_enabled": () => false,
      "plugin:autostart|enable": () => undefined,
      "plugin:autostart|disable": () => undefined,
      // LaunchAgent path-staleness flag (#317). Default `false` so
      // the warning row stays hidden in the success path; specs
      // that exercise the warning override `stale: true` per-test.
      get_autostart_path_status: () => ({ stale: false }),
      retry_autostart_registration: () => true,
      // App-info plugin commands. The Settings → About tab calls
      // `getName` / `getVersion` / `getTauriVersion` from
      // `@tauri-apps/api/app`, all of which dispatch through these
      // `plugin:app|<verb>` IPCs. Test values mirror the real
      // package metadata so the rendered copy is exercised.
      "plugin:app|name": () => "Hush",
      "plugin:app|version": () => "0.1.0",
      "plugin:app|tauri_version": () => "2.10.3",
      // `@tauri-apps/plugin-os::platform()` — drives the PTT
      // modifier-glyph copy in `+page.svelte` and
      // `settings/+page.svelte`. Tests run macOS-flavoured copy
      // since that's the project's design target.
      "plugin:os|platform": () => "macos",
      // `@tauri-apps/plugin-shell::open()` — used by the
      // `openExternal` helper (#322) for every external link in
      // the app. Default no-op so specs that don't care about
      // link clicks pass through; specs that exercise a link
      // override with a recording handler.
      "plugin:shell|open": () => undefined,
      open_macos_privacy_pane: () => undefined,
      prime_screen_recording_permission: () => undefined,
      // First-run wizard inline-grant IPCs (#511). Default no-op
      // for mic (real IPC fires the OS dialog asynchronously and
      // returns immediately) and `true` for IM (real IPC blocks
      // until the user responds, returning the resulting bool).
      // Specs exercising the wizard grant flow override per-test
      // to flip the diagnose_macos_permissions response between
      // calls so the polled UI walks through not-determined →
      // granted.
      request_microphone_permission: () => undefined,
      request_input_monitoring_permission: () => true,
      open_settings: () => undefined,
      show_main_window: () => undefined,
      open_debug_window: () => undefined,
      diagnose_macos_permissions: () => ({
        bundleId: "io.github.khawkins98.hush",
        microphoneHint: "Mocked microphone hint.",
        inputMonitoringHint: "Mocked input monitoring hint.",
        canReset: false,
        statuses: {
          microphone: "not-applicable",
          screenRecording: "not-applicable",
          inputMonitoring: "not-applicable",
        },
      }),
      reset_macos_permissions: () => ({
        anyReset: false,
        summary: "Mocked reset (e2e — no real tccutil call).",
      }),
      // Three-state permission health (#378). Default to all
      // not-applicable so the panel renders the same neutral
      // shape as the diagnostic mock above. Specs that exercise
      // the traffic-light states override per-test.
      get_permission_health: () => ({
        health: {
          microphone: "not-applicable",
          screenRecording: "not-applicable",
          inputMonitoring: "not-applicable",
        },
      }),
      confirm_permission: () => undefined,

      // ---- audio sources ----
      // `audio_list_sources` is the picker-shaped enumeration: every
      // mic plus the system-audio entry, with capability flags for
      // disabled rendering on platforms that don't support an entry.
      audio_list_sources: () => [
        {
          kind: "microphone",
          id: "Built-in Microphone",
          name: "Built-in Microphone",
          isDefault: true,
          isSupported: true,
        },
        {
          kind: "system-audio",
          id: "system",
          name: "System audio",
          isDefault: false,
          // Defaults to false so e2e tests render the "coming soon"
          // disabled state by default. Specs that exercise the
          // shipped-on-this-platform path should override.
          isSupported: false,
        },
      ],

      // ---- dictation lifecycle ----
      // start_dictation / stop_dictation: toggle-hotkey and PTT path.
      // meeting_start_manual / meeting_stop_manual: UI button path (below).
      start_dictation: () => undefined,
      stop_dictation: () => ({
        text: "hello world",
        foreground: { appName: "Hush", windowTitle: "Hush" },
      }),

      // ---- history ----
      history_list: () => [],
      history_search: () => [],
      history_delete: () => undefined,
      history_count: () => 0,
      history_clear: () => 0,
      // Dictation stats (#293). Default to all-zeros so the
      // stats bar's `sessionCount === 0` guard hides it on
      // baseline mocks; specs that want the bar visible
      // override per-test.
      get_dictation_stats: () => ({
        sessionCount: 0,
        wordCount: 0,
        totalRecordingMs: 0,
        totalChars: 0,
      }),

      // ---- replacements ----
      replacements_list: () => [],
      replacement_create: (args: unknown) => {
        const a = args as { findText: string; replaceText: string; sortOrder: number };
        return { id: 1, findText: a.findText, replaceText: a.replaceText, sortOrder: a.sortOrder };
      },
      replacement_update: () => undefined,
      replacement_delete: () => undefined,

      // ---- vocabulary ----
      vocabulary_list: () => [],
      vocabulary_create: (args: unknown) => {
        const a = args as { term: string };
        return { id: 1, term: a.term };
      },
      vocabulary_update: () => undefined,
      vocabulary_delete: () => undefined,

      // ---- model picker ----
      // Field shape mirrors `ModelCard` on the Rust side, which flattens
      // `ModelMetadata` and applies `#[serde(rename_all = "camelCase")]`.
      // Keep this in sync with `src-tauri/src/transcription/catalog.rs`
      // and `src-tauri/src/ipc/commands.rs::ModelCard` — a stale field
      // name here surfaces as `undefined` in the page component, which
      // the Playwright suite may not catch unless a spec asserts on the
      // value. Round-5 review caught a regression where the mock had
      // `sizeBytes`/`sizeLabel`/`speed`/`accuracy` while Rust serialised
      // `sizeMb`/`speedRating`/`accuracyRating`. Don't repeat that.
      model_list: () => [
        {
          id: "whisper-base",
          displayName: "Whisper Base",
          filename: "ggml-base.bin",
          sizeMb: 142,
          speedRating: 9,
          accuracyRating: 6,
          description: "Default. Fast, decent for dictation.",
          isDefault: true,
          downloadUrl: "https://example.test/ggml-base.bin",
          sha256: "abc123",
          isDownloaded: true,
          isSelected: true,
          expectedPath: "/tmp/models/ggml-base.bin",
        },
      ],
      model_select: () => ({ loaded: true }),
      model_download: () => undefined,
      model_cancel_download: () => undefined,
      model_remove: () => undefined,

      // ---- meeting mode (Phase C scaffold; refs #33 / #109) ----
      // Empty list by default so the panel renders the "no sessions
      // yet" placeholder. Specs that exercise populated states
      // override `meeting_sessions_list` per-test. The search-aware
      // sibling (#357 phase 2 step 3) defaults to the same empty
      // shape — specs that exercise cross-stream search override
      // it per-test alongside `history_search`.
      meeting_sessions_list: () => [],
      meeting_sessions_search: () => [],
      // Per-row dictation CSV export (#357 phase 3a). The IPC
      // accepts `{ id, path }` and writes the file server-side; in
      // tests we just no-op so specs can assert the click reached
      // the mock layer without needing a writable disk path.
      history_export_row_csv: () => undefined,
      // Bulk Export filtered (#357 phase 3c-1). Default returns
      // the shape the frontend expects without writing anything.
      // Specs that exercise the toast copy override per-test.
      history_export_bundle: () => ({
        directory: "/Users/test/Desktop",
        written: 0,
      }),
      meeting_session_get: () => {
        throw { kind: "settings", message: "meeting session not found (default mock)" };
      },
      meeting_session_delete: () => undefined,
      // Per-row meeting export (#357 phase 3b). Accepts `{ id,
      // format, path }`; same no-op default as the dictation
      // sibling. Specs that exercise the popover flow override
      // per-test.
      meeting_session_export: () => undefined,
      meeting_session_set_notes: () => undefined,
      meeting_active_session: () => ({ active: null }),
      meeting_start_manual: () => ({
        id: 1,
        appName: "manual",
        appKind: "other",
        startedAt: "2026-04-26T15:00:00Z",
        endedAt: null,
        speakerCount: null,
        utteranceCount: 0,
        notes: null,
        sources: ["mic", "system"],
        appTitle: null,
      }),
      meeting_stop_manual: () => undefined,

      // ---- Phase E (#112) per-app classifier overrides ----
      // Empty list by default so the Meeting tab renders the "no
      // overrides yet" placeholder. Specs that need pre-populated
      // rows override `meeting_app_override_list` per-test.
      meeting_app_override_list: () => [],
      meeting_app_override_upsert: (args) => {
        const { appName, kind } = (args ?? {}) as {
          appName: string;
          kind: string;
        };
        return {
          appName,
          kind,
          createdAt: "2026-04-28T00:00:00Z",
          // #427 Item 5 foundation slice — new schema columns are
          // returned by the IPC. Default to null since the panel
          // UI that populates them hasn't shipped yet.
          preferredAudioSource: null,
          preferredModelId: null,
        };
      },
      meeting_app_override_set_profile: (args) => {
        const {
          appName,
          preferredAudioSource = null,
          preferredModelId = null,
        } = (args ?? {}) as {
          appName: string;
          preferredAudioSource?: string | null;
          preferredModelId?: string | null;
        };
        return {
          appName,
          // Default-mock returns "meeting" since the existing
          // `meeting_app_override_upsert` mock does the same;
          // specs that need a different kind override per-test.
          kind: "meeting",
          createdAt: "2026-04-28T00:00:00Z",
          preferredAudioSource,
          preferredModelId,
        };
      },
      meeting_app_override_delete: () => undefined,
      // Built-in classification table (#320). Default mock returns
      // a small representative subset — full table is ~70 entries
      // and most tests don't care about the exact contents. Specs
      // that exercise the redundant-override warning override per-
      // test with a known entry.
      meeting_app_classifier_defaults: () => [
        { appName: "us.zoom.xos", kind: "meeting" },
        { appName: "Zoom.exe", kind: "meeting" },
        { appName: "com.microsoft.teams2", kind: "meeting" },
        { appName: "Teams.exe", kind: "meeting" },
        { appName: "com.spotify.client", kind: "media" },
        { appName: "Spotify.exe", kind: "media" },
      ],
    };

    // Rebuild override functions from their stringified source. The
    // double-arrow guards let test overrides be either `() => x` or
    // `function(args) { ... }` — `new Function` accepts both.
    //
    // ⚠️ CLOSURE CAPTURE LIMITATION: Override functions are serialised via
    // `.toString()` before being sent across the Playwright bridge, then
    // reconstructed with `new Function(...)` inside the page's JS context.
    // This means they run in a completely fresh scope — any module-level
    // constants or variables from the test file (e.g. `DEFAULT_SESSION_ID`)
    // will NOT be defined when the function executes. All values inside
    // override functions must be literals, not references to outer variables.
    //
    // ✅ OK:  meeting_session_get: () => ({ session: { id: 1, ... } })
    // ❌ BAD: meeting_session_get: () => ({ session: { id: DEFAULT_SESSION_ID } })
    //        → ReferenceError: DEFAULT_SESSION_ID is not defined (runtime failure)
    const overrides: Record<string, (args?: unknown) => unknown> = {};
    for (const [name, src] of overrideStrings as Array<[string, string]>) {
      // eslint-disable-next-line no-new-func
      overrides[name] = new Function(`return (${src});`)();
    }

    const handlers = { ...defaults, ...overrides };
    // The stub treats sync return values as resolved promises.
    const wrapped: Record<string, (args?: unknown) => Promise<unknown>> = {};
    for (const [k, v] of Object.entries(handlers)) {
      wrapped[k] = async (args) => v(args);
    }
    (window as unknown as { __hush_e2e: { invoke: typeof wrapped } }).__hush_e2e = {
      invoke: wrapped,
    };
    },
    {
      overrideStrings: overrideEntries,
      defaultDiarizerModelStatus: DEFAULT_DIARIZER_MODEL_STATUS,
      defaultUpdateCheckResult: DEFAULT_UPDATE_CHECK_RESULT,
    },
  );
}

/**
 * Click into one of the main-window sidebar sections.
 *
 * Phase 1 of #357 collapsed the sidebar from three sections
 * (Dictation/Meetings/History) to two (Dictation/History) — meeting
 * sessions surface in the unified History feed once Phase 2 lands.
 *
 * The legacy `"meetings"` token is still accepted by this helper so
 * older specs don't break overnight; it routes to History (the
 * destination meetings will eventually live in). Specs that
 * specifically depend on the old meetings panel UI are kept
 * checked-in but skipped via `test.skip` until Phase 2 reintroduces
 * the surface.
 */
export async function gotoSection(
  page: Page,
  section: "dictation" | "meetings" | "history" | "configuration",
): Promise<void> {
  // #479 slice 1: dictation + history are now mutually-exclusive
  // panels driven by the left sidebar. Click the matching sidebar
  // item to swap the active panel. "meetings" still maps to
  // History (legacy alias). "configuration" is the old Settings
  // tab strip — opens the standalone Settings window in slice 1.
  const target =
    section === "meetings"
      ? "history"
      : section === "configuration"
        ? "settings"
        : section;
  await page.locator(`[data-testid="sidebar-nav-${target}"]`).click();
  if (target !== "settings") {
    await page.locator(`#${target}-section`).waitFor({ state: "visible" });
  }
}

/**
 * Fire a Tauri event from the test side. Use to simulate
 * backend-emitted events like `audio:level`,
 * `model:download-progress`, `hotkey:toggle`.
 *
 * Returns a `Promise<void>` that resolves once the event has been
 * dispatched to all attached listeners on the page.
 */
export async function fireEvent<T>(
  page: Page,
  name: string,
  payload: T,
): Promise<void> {
  await page.evaluate(
    ([n, p]) => {
      const bus = (window as unknown as {
        __hush_e2e_event_bus?: { fire: (name: string, payload: unknown) => void };
      }).__hush_e2e_event_bus;
      if (!bus) {
        throw new Error("[hush-e2e] event bus not initialised — did you import event-stub.ts?");
      }
      bus.fire(n, p);
    },
    [name, payload] as const,
  );
}
