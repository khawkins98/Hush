<!--
  Standalone Settings window — Phase 3 of the IA redesign.
  Loaded into the secondary `settings` Tauri window (label
  `settings`, configured in `tauri.conf.json`). Opened via:

    - macOS: ⌘, accelerator on the native menu (Hush → Settings…)
    - Sidebar: the "Settings ⌘," footer button on the main window
    - Backend: `settings_window::show()`

  This window is its own Svelte tree — it cannot share `$state`
  with the main window. Each panel hosted here owns its own state
  + IPC fetch/mutation, and the wider app reads back through the
  IPC commands themselves at use time (e.g. `start_dictation`
  pulls the latest replacements from the repo when it runs, so the
  main window doesn't need a live mirror).

  Cross-window invalidation today is **minimal**:

    - `model:download-done` is broadcast on every window, so the
      main window's "no model installed" banner refreshes after a
      download here without extra wiring.
    - Replacements / vocabulary changes are picked up by the
      transcription pipeline at the next dictation invocation —
      no UI on the main window currently shows them.

  If a future panel surfaces shared UI state, add a
  `settings:changed` Tauri event from the relevant IPC handler
  and have the main window listen.
-->
<script lang="ts">
  /* (`@tauri-apps/api/app` imports moved to AboutTab.svelte in
     #332 phase 1 final slice — appName / version / Tauri runtime
     are About-tab-only data.) */
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  /* (autostart + plugin-os imports moved to GeneralTab.svelte
     in #332 phase 1.) */
  import { onDestroy, onMount, tick } from "svelte";

  import AboutTab from "$lib/AboutTab.svelte";
  import GeneralTab from "$lib/GeneralTab.svelte";
  import MeetingTab from "$lib/MeetingTab.svelte";
  import ModelPickerPanel from "$lib/ModelPickerPanel.svelte";
  import PermissionsTab from "$lib/PermissionsTab.svelte";
  import ReplacementsTab from "$lib/ReplacementsTab.svelte";
  import VocabularyTab from "$lib/VocabularyTab.svelte";
  import {
    formatErrorDisplay,
    formatErrorMessage,
    type ErrorDisplay,
  } from "$lib/errors";
  import { Events } from "$lib/events";
  import { formatMb } from "$lib/format";
  import type {
    DownloadProgress,
    IpcError,
    ModelCard,
    ModelSelectNotice,
  } from "$lib/types";

  type SettingsTab =
    | "general"
    | "model"
    | "vocabulary"
    | "replacements"
    | "meeting"
    | "permissions"
    | "about";

  // Default landing tab. "general" matches the macOS Settings
  // convention; deep-links from the main window override via the
  // `settings:goto-tab` listener registered in `onMount`.
  let active = $state<SettingsTab>("general");

  // Same heuristic the main window uses — drives Right ⌘ vs Right
  // Ctrl in the PTT hotkey display, since the backend default
  // varies by platform (see `hotkey/ptt.rs::DEFAULT_PTT_KEY`).
  // Resolved asynchronously via `@tauri-apps/plugin-os` (#272) —
  // replaces a deprecated `navigator.platform` regex match. Defaults
  // to `false` until the IPC round-trip lands in onMount; only
  // affects the modifier-glyph copy in the PTT hotkey display, so
  // a one-frame default-then-correct flicker is imperceptible.
  /* (`isMacOS` lives in GeneralTab.svelte in #332 phase 1.) */

  const tabs: Array<{ key: SettingsTab; label: string; testId: string }> = [
    { key: "general", label: "General", testId: "settings-tab-general" },
    { key: "model", label: "Model", testId: "settings-tab-model" },
    { key: "vocabulary", label: "Vocabulary", testId: "settings-tab-vocabulary" },
    { key: "replacements", label: "Replacements", testId: "settings-tab-replacements" },
    { key: "meeting", label: "Meeting", testId: "settings-tab-meeting" },
    { key: "permissions", label: "Permissions", testId: "settings-tab-permissions" },
    { key: "about", label: "About", testId: "settings-tab-about" },
  ];

  // ---- Model picker state ------------------------------------------------
  // Bundle for the model picker. The six fields are read/written
  // together — load → display → click Download → progress events
  // mutate `downloading` → done event clears it + bumps `models` —
  // so they live in one struct rather than six top-level
  // declarations. Svelte 5 deep reactivity means we can mutate the
  // inner Maps directly (`modelFetch.downloading.set(...)`) without
  // the rebuild-and-reassign dance the previous shape needed.
  type ModelFetch = {
    models: ModelCard[];
    loaded: boolean;
    error: ErrorDisplay | null;
    restartNotice: ModelSelectNotice;
    /// In-flight download progress keyed by card id.
    downloading: Map<string, DownloadProgress>;
    /// Per-card failure message from the most recent attempt.
    /// Cleared on retry.
    failed: Map<string, string>;
  };
  let modelFetch = $state<ModelFetch>({
    models: [],
    loaded: false,
    error: null,
    restartNotice: null,
    downloading: new Map(),
    failed: new Map(),
  });

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;
  let unlistenGotoTab: UnlistenFn | null = null;
  /* (`unlistenUpdaterResult` moved to AboutTab.svelte in #332
     phase 1 final slice.) */
  /* (Permissions-tab window-focus listener handle moved to
     PermissionsTab.svelte in #332 phase 1.) */

  /* (General-tab state — autostart, HUD, sound cues, inference
     threads, first-run reset — moved to GeneralTab.svelte in
     #332 phase 1.) */

  // Diarization (#111). Default off — opt-in. When the toggle is
  // on AND the wespeaker .onnx model is present in the models
  // directory, the meeting pump labels utterances per-speaker
  // (Speaker 1, 2, …) instead of the source-derived "You" /
  // "Remote" tags. The toggle persists; runtime behaviour gates
  // on `FlagGatedDiarizer` reading the same atomic shared with
  // AppState.
  /* (Meeting-tab state — diarization, diarizer model status,
     app-classifier overrides, auto-start mode — moved to
     MeetingTab.svelte in #332 phase 1.) */

  /* (About-tab state — appName, appVersion, tauriVersion, the
     UpdateCheckResult union, updateCheck, updateChecking — moved
     to AboutTab.svelte in #332 phase 1 final slice.) */

  // (macOS permission diagnostic state + handlers moved to
  // `PermissionsTab.svelte` in #332 phase 1 — the Permissions tab
  // owns its own state, IPC, and lifecycle now.)

  // Error formatting: routed through `lib/errors.ts` (#205) so the
  // main and Settings windows share one source of truth.
  // Rich-shaped state uses `formatErrorDisplay`; the few string-
  // shaped surfaces in this file (`autostartError`,
  // `firstRunResetMessage`, per-card `downloadFailed`) use
  // `formatErrorMessage`.

  // ---- Loaders -----------------------------------------------------------

  async function loadModels(): Promise<void> {
    try {
      modelFetch.models = await invoke<ModelCard[]>("model_list");
      modelFetch.error = null;
    } catch (e) {
      modelFetch.error = formatErrorDisplay(e);
    } finally {
      modelFetch.loaded = true;
    }
  }


  /* (Meeting-tab handlers — auto-start mode, app overrides
     CRUD — moved to MeetingTab.svelte in #332 phase 1.) */

  // ---- Mutators ----------------------------------------------------------

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", { id: card.id });
      modelFetch.restartNotice = result.loaded ? "loaded" : "needs-restart";
      modelFetch.error = null;
      await loadModels();
    } catch (e) {
      modelFetch.error = formatErrorDisplay(e);
      if (typeof e === "object" && e !== null && "kind" in e) {
        const ipc = e as IpcError;
        if (ipc.kind === "model-not-downloaded") {
          modelFetch.restartNotice = "needs-download";
        }
      }
    }
  }

  async function downloadModel(card: ModelCard) {
    modelFetch.failed.delete(card.id);
    modelFetch.downloading.set(card.id, { received: 0, total: null });
    try {
      await invoke("model_download", { id: card.id });
    } catch (e) {
      modelFetch.failed.set(card.id, formatErrorMessage(e));
      modelFetch.downloading.delete(card.id);
    }
  }

  async function cancelDownload(card: ModelCard) {
    try {
      await invoke("model_cancel_download", { id: card.id });
    } catch (e) {
      console.warn("[hush] cancel download failed", e);
    }
    modelFetch.downloading.delete(card.id);
  }

  async function removeModel(card: ModelCard) {
    try {
      await invoke("model_remove", { id: card.id });
      await loadModels();
    } catch (e) {
      modelFetch.error = formatErrorDisplay(e);
    }
  }

  /* `openPrivacyPane` + `runMacosReset` moved to
     PermissionsTab.svelte in #332 phase 1. */

  // ---- Lifecycle ---------------------------------------------------------

  onMount(async () => {
    /* (Platform-glyph probe moved to GeneralTab.svelte in #332
       phase 1; the PTT-hotkey display is the only consumer of
       `isMacOS` and it now lives inside the tab.) */

    type DownloadProgressEvent = { id: string; bytesReceived: number; bytesTotal: number | null };
    type DownloadStatusEvent = { id: string; message: string | null };

    unlistenDownloadProgress = await listen<DownloadProgressEvent>(
      Events.ModelDownloadProgress,
      (e) => {
        modelFetch.downloading.set(e.payload.id, {
          received: e.payload.bytesReceived,
          total: e.payload.bytesTotal,
        });
      },
    );
    unlistenDownloadDone = await listen<DownloadStatusEvent>(Events.ModelDownloadDone, (e) => {
      modelFetch.downloading.delete(e.payload.id);
      void loadModels();
    });
    unlistenDownloadFailed = await listen<DownloadStatusEvent>(Events.ModelDownloadFailed, (e) => {
      modelFetch.failed.set(e.payload.id, e.payload.message ?? "Download failed.");
      modelFetch.downloading.delete(e.payload.id);
    });

    // Deep-link from the main window's "Open the Permissions
    // diagnostic" link / future menu items. Payload is the tab key
    // — silently ignored if it isn't one we know, so future tabs
    // added on the main window don't crash a stale settings build.

    /* Menu-driven `updater:result` listener moved to AboutTab.svelte
       in #332 phase 1 final slice. The race window between the
       menu's probe spawn and the AboutTab's onMount listener
       registration is well below the probe's network floor — see
       AboutTab's header comment for the full reasoning. */

    unlistenGotoTab = await listen<string>(Events.SettingsGotoTab, (e) => {
      const target = e.payload;
      if (
        target === "general" ||
        target === "model" ||
        target === "vocabulary" ||
        target === "replacements" ||
        target === "meeting" ||
        target === "permissions" ||
        target === "about"
      ) {
        active = target;
      }
    });

    await loadModels();

    /* Diarizer-download lifecycle listeners (#301) + Meeting-tab
       eager loads moved to MeetingTab.svelte in #332 phase 1;
       Permissions-tab window-focus listener moved to
       PermissionsTab.svelte; About-tab metadata + updater
       listener moved to AboutTab.svelte. */
  });

  /* (`onCheckForUpdates` + `loadAppMetadata` moved to
     AboutTab.svelte in #332 phase 1 final slice.) */

  onDestroy(() => {
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
    unlistenGotoTab?.();
    /* (Updater-result listener teardown moved to AboutTab.svelte;
       diarizer-listener teardown to MeetingTab.svelte; both in
       #332 phase 1.) */
  });
</script>

<main class="settings-window">
  <!--
    Window header: brand wordmark + tab strip. UX walkthrough flagged
    the previous bare-tab-strip layout as ambiguous when the user
    arrives via ⌘, with no animation — it read as a second sidebar
    rather than a Settings window. Adding "Settings" above the strip
    anchors the surface.
  -->
  <header class="settings-window-header">
    <h1 class="settings-window-title">Settings</h1>
    <nav class="settings-toolbar" aria-label="Settings categories">
      {#each tabs as tab (tab.key)}
      <button
        type="button"
        class="tab-button"
        class:active={active === tab.key}
        aria-current={active === tab.key ? "page" : undefined}
        data-testid={tab.testId}
        onclick={() => (active = tab.key)}
      >
        {tab.label}
      </button>
    {/each}
    </nav>
  </header>

  <section class="tab-body" aria-live="polite">
    {#if active === "general"}
      <GeneralTab />
    {:else if active === "model"}
      <ModelPickerPanel
        models={modelFetch.models}
        modelsLoaded={modelFetch.loaded}
        modelsError={modelFetch.error}
        modelsRestartNotice={modelFetch.restartNotice}
        downloading={modelFetch.downloading}
        downloadFailed={modelFetch.failed}
        {formatMb}
        onSelect={selectModel}
        onDownload={downloadModel}
        onCancel={cancelDownload}
        onRemove={removeModel}
      />
    {:else if active === "vocabulary"}
      <VocabularyTab />
    {:else if active === "replacements"}
      <ReplacementsTab />
    {:else if active === "meeting"}
      <MeetingTab />
    {:else if active === "permissions"}
      <PermissionsTab />
    {:else if active === "about"}
      <AboutTab />
    {/if}
  </section>
</main>

<style>
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: #f3f3f5;
    color: #0f0f0f;
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
      Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif,
      "Apple Color Emoji", "Segoe UI Emoji";
    -webkit-font-smoothing: antialiased;
    /* Native scrollbars + form-control rendering follow the OS
       light/dark mode; OS accent drives checkboxes / radios /
       range sliders. See main page for the full rationale. */
    color-scheme: light dark;
    accent-color: auto;
  }

  .settings-window {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }

  /* Window header (sticky) — anchors the surface so the tab strip
     reads as "tabs inside Settings" rather than "navigation
     somewhere else in the app." */
  .settings-window-header {
    position: sticky;
    top: 0;
    z-index: 1;
    background-color: #ececef;
    border-bottom: 1px solid #d8d8dc;
    flex-shrink: 0;
  }
  .settings-window-title {
    margin: 0;
    padding: 0.85rem 1.1rem 0.25rem;
    /* H1 must outsize H2 (`.tab-title` is 1.4rem). The previous
       1.15rem inverted the visual hierarchy and the eye landed
       on the tab title instead of the Settings anchor — defeated
       the whole point of adding the H1. */
    font-size: 1.55rem;
    font-weight: 700;
    color: #1a1a1a;
    letter-spacing: -0.015em;
  }

  .settings-toolbar {
    display: flex;
    gap: 0.25rem;
    padding: 0.4rem 0.75rem 0.6rem;
    background-color: transparent;
    overflow-x: auto;
    flex-shrink: 0;
  }

  .tab-button {
    padding: 0.4rem 0.85rem;
    border: 1px solid transparent;
    background-color: transparent;
    color: #333;
    font-family: inherit;
    font-size: 0.85rem;
    font-weight: 500;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  }
  .tab-button:hover { background-color: rgba(0, 0, 0, 0.06); }
  .tab-button.active {
    background-color: white;
    border-color: #d1d1d8;
    color: #2c3e8f;
    font-weight: 600;
  }
  .tab-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .tab-body {
    flex: 1;
    padding: 2rem 2.5rem;
    width: 100%;
    box-sizing: border-box;
    overflow-y: auto;
  }

  /* Inner panels were authored to centre themselves under a 36rem
     measure on the main window. The Settings window is wider; bump
     the inner max-width on the rendered panel sections so the
     model cards / lists breathe. */
  .tab-body :global(section.panel-models),
  .tab-body :global(section.panel-vocabulary),
  .tab-body :global(section.panel-replacements),
  .tab-body :global(section.panel-macos-diagnostic) {
    max-width: 44rem;
    margin-left: auto;
    margin-right: auto;
    /* The components ship with a left-border + padding-left for the
       per-section colour spine. Keep that; just expand the measure. */
  }

  /* Drop the sticky / sectional spacing the panels use on the main
     window — inside Settings each tab body owns the spacing. */
  .tab-body :global(.panel-models),
  .tab-body :global(.panel-vocabulary),
  .tab-body :global(.panel-replacements),
  .tab-body :global(.panel-macos-diagnostic) {
    margin-top: 0;
  }

  /* All per-tab CSS moved to its respective tab component in
     #332 phase 1 (PermissionsTab, VocabularyTab, ReplacementsTab,
     GeneralTab, MeetingTab, AboutTab). Shared card primitives
     hoist to a CSS module per #392 once that lands. */

  @media (prefers-color-scheme: dark) {
    :global(html), :global(body) {
      background-color: #1d1d1f;
      color: #e8e8e8;
    }
    .settings-window-header {
      background-color: #2a2a2d;
      border-bottom-color: #38383b;
    }
    .settings-window-title {
      color: #f0f0f0;
    }
    .tab-button { color: #d8d8d8; }
    .tab-button:hover { background-color: rgba(255, 255, 255, 0.06); }
    .tab-button.active {
      background-color: #1d1d1f;
      border-color: #38383b;
      color: #b8c8ff;
    }
  }
</style>
