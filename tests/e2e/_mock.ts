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

  await page.addInitScript((overrideStrings) => {
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
      // Meeting auto-start mode (Settings → Meeting). Default
      // matches the backend's "off" default; specs that exercise
      // the dropdown override per-test.
      get_meeting_autostart_mode: () => "off",
      set_meeting_autostart_mode: () => undefined,
      // Manual update probe (#223). Default to "up to date" so
      // specs that don't override get a stable result if the
      // user clicks the button.
      check_for_updates: () => ({
        kind: "upToDate",
        current: "0.1.0",
      }),
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
      open_macos_privacy_pane: () => undefined,
      prime_screen_recording_permission: () => undefined,
      open_settings: () => undefined,
      diagnose_macos_permissions: () => ({
        bundleId: "com.khawkins.hush",
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
      // override `meeting_sessions_list` per-test.
      meeting_sessions_list: () => [],
      meeting_session_get: () => {
        throw { kind: "settings", message: "meeting session not found (default mock)" };
      },
      meeting_session_delete: () => undefined,
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
        };
      },
      meeting_app_override_delete: () => undefined,
    };

    // Rebuild override functions from their stringified source. The
    // double-arrow guards let test overrides be either `() => x` or
    // `function(args) { ... }` — `new Function` accepts both.
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
  }, overrideEntries);
}

/**
 * Click into one of the main-window sidebar sections (Phase 1 IA
 * redesign). Specs targeting Meetings / History / Configuration
 * panels should call this after `page.goto("/")` so the panel is
 * actually rendered before locating it. Dictation is the default
 * landing tab — specs hitting it can skip this helper.
 */
export async function gotoSection(
  page: Page,
  section: "dictation" | "meetings" | "history" | "configuration",
): Promise<void> {
  await page.locator(`[data-testid="nav-${section}"]`).click();
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
