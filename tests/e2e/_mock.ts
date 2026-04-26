// Default mock state for Playwright e2e tests. Test specs import
// `installMocks(page)` from here and pass per-test overrides on top
// of the defaults.
//
// Why a single shared default rather than per-test wholesale mocks:
// the dictation page calls a half-dozen `invoke`s on mount alone
// (history list, replacements list, vocabulary list, model list,
// settings get, list_input_devices, get_first_run_completed). If
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
      open_macos_privacy_pane: () => undefined,
      diagnose_macos_permissions: () => ({
        bundleId: "com.khawkins.hush",
        microphoneHint: "Mocked microphone hint.",
        inputMonitoringHint: "Mocked input monitoring hint.",
        canReset: false,
      }),
      reset_macos_permissions: () => ({
        anyReset: false,
        summary: "Mocked reset (e2e — no real tccutil call).",
      }),

      // ---- audio sources ----
      // `list_input_devices` is the legacy command; `audio_list_sources`
      // is the picker-shaped one that includes the system-audio entry.
      // Both stay mocked for one transitional release while the picker
      // migration soaks.
      list_input_devices: () => [
        { id: "Built-in Microphone", name: "Built-in Microphone", isDefault: true },
      ],
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
