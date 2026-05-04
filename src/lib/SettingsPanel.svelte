<!--
  Settings panel — extracted from `routes/settings/+page.svelte`
  in #479 slice 2 so the same content can render either inside
  the standalone Settings window (legacy, slice 3 deletes it) or
  inline as the third sidebar panel on the main page.

  Owns:
    - Tab strip + active-tab state (`activeTab`, bindable so the
      orchestrator can deep-link from menus / palette / banners).
    - Model picker state + lifecycle (model_list / model_select /
      model_download / model_remove + the 3 download-progress
      Tauri event listeners).
    - The `settings:goto-tab` Tauri-event listener so cross-window
      deep-links still flip the active tab when the panel is
      hosted in either context.

  Each tab's per-tab state still lives inside its dedicated
  `*Tab.svelte` (GeneralTab, VocabularyTab, etc., per the #332
  phase 1 split). This panel is the host shell.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import { SvelteMap } from "svelte/reactivity";

  import DebugTab from "./DebugTab.svelte";
  import GeneralTab from "./GeneralTab.svelte";
  import MeetingTab from "./MeetingTab.svelte";
  import ModelPickerPanel from "./ModelPickerPanel.svelte";
  import PermissionsTab from "./PermissionsTab.svelte";
  import ReplacementsTab from "./ReplacementsTab.svelte";
  import VocabularyTab from "./VocabularyTab.svelte";
  import {
    formatErrorDisplay,
    formatErrorMessage,
    type ErrorDisplay,
  } from "./errors";
  import { Events } from "./events";
  import { formatMb } from "./format";
  import { readDebugConsoleEnabled } from "./debug-console";
  import type {
    DiarizerModelStatus,
    DownloadProgress,
    IpcError,
    ModelCard,
    ModelSelectNotice,
  } from "./types";

  export type SettingsTab =
    | "general"
    | "model"
    | "vocabulary"
    | "replacements"
    | "meeting"
    | "permissions"
    | "debug";

  type Props = {
    /// Which tab is showing. Bindable so the parent can deep-link
    /// in (e.g. ⌘K palette's "Open Settings: Permissions" sets
    /// this to `"permissions"`).
    activeTab?: SettingsTab;
    /// Called when a model is successfully hot-loaded into the
    /// backend slot (`model_select` returned `loaded: true`). The
    /// parent uses this to clear stale `TranscriptionUnavailable`
    /// error banners that may have been set before a model was
    /// explicitly picked.
    onModelLoaded?: () => void;
  };

  let { activeTab = $bindable("general"), onModelLoaded }: Props = $props();

  // Debug tab is conditionally shown: only when the developer
  // console toggle (Settings → General → Advanced → Developer
  // console) is on. Read localStorage on mount so the tab
  // persists across Settings opens within the same session.
  let debugConsoleEnabled = $state(false);

  function onDebugConsoleChange(enabled: boolean) {
    debugConsoleEnabled = enabled;
    if (!enabled && activeTab === "debug") {
      activeTab = "general";
    }
  }

  // Compute visible tabs reactively so the Debug tab appears /
  // disappears without a page reload.
  const baseTabs: Array<{ key: SettingsTab; label: string; testId: string }> = [
    { key: "general", label: "General", testId: "settings-tab-general" },
    { key: "model", label: "Model", testId: "settings-tab-model" },
    { key: "vocabulary", label: "Vocabulary", testId: "settings-tab-vocabulary" },
    { key: "replacements", label: "Replacements", testId: "settings-tab-replacements" },
    { key: "meeting", label: "Meeting", testId: "settings-tab-meeting" },
    { key: "permissions", label: "Permissions", testId: "settings-tab-permissions" },
    { key: "debug", label: "Debug", testId: "settings-tab-debug" },
  ];

  let tabs = $derived(
    debugConsoleEnabled ? baseTabs : baseTabs.filter((t) => t.key !== "debug"),
  );

  type ModelFetch = {
    models: ModelCard[];
    loaded: boolean;
    error: ErrorDisplay | null;
    restartNotice: ModelSelectNotice;
    // SvelteMap rather than plain Map: per-card mutations
    // (`.set` / `.delete`) trigger reactivity. A plain Map inside
    // `$state(...)` looks reactive at type level but Svelte 5's
    // proxy doesn't intercept Map operations, so a `Cancel` /
    // `download-done` mutation only repainted on the next unrelated
    // re-render (e.g. tab switch). See docs.svelte.dev → reactive
    // built-ins.
    downloading: SvelteMap<string, DownloadProgress>;
    failed: SvelteMap<string, string>;
  };

  let modelFetch = $state<ModelFetch>({
    models: [],
    loaded: false,
    error: null,
    restartNotice: null,
    downloading: new SvelteMap(),
    failed: new SvelteMap(),
  });

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;
  let unlistenGotoTab: UnlistenFn | null = null;

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

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", {
        id: card.id,
      });
      modelFetch.restartNotice = result.loaded ? "loaded" : "needs-restart";
      modelFetch.error = null;
      if (result.loaded) {
        onModelLoaded?.();
      }
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

  onMount(async () => {
    type DownloadProgressEvent = {
      id: string;
      bytesReceived: number;
      bytesTotal: number | null;
    };
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
    unlistenDownloadDone = await listen<DownloadStatusEvent>(
      Events.ModelDownloadDone,
      async (e) => {
        modelFetch.downloading.delete(e.payload.id);
        void loadModels();
        // Auto-bundle the wespeaker (speaker diarization) download
        // sequentially after a Whisper model finishes (#478). The
        // user is already committing to a model download in the
        // first-run flow; tacking on the 26 MB wespeaker here means
        // speaker labels work out of the box for any future mic
        // session without a separate Settings → Meeting →
        // Speakers click later.
        //
        // Best-effort: failures fall through silently (logged but
        // not surfaced). The user lands in the existing "model
        // not downloaded" state in MeetingTab where the manual
        // Download button is the explicit retry. The wespeaker id
        // is excluded so a wespeaker download completing doesn't
        // try to re-trigger itself.
        if (e.payload.id !== "wespeaker-resnet34-lm") {
          try {
            const status = await invoke<DiarizerModelStatus>(
              "get_diarizer_model_status",
            );
            if (!status.downloaded) {
              await invoke("download_diarizer_model");
            }
          } catch (err) {
            console.warn(
              "[hush] auto-bundle wespeaker download failed; user can retry from Settings → Meeting → Speakers",
              err,
            );
          }
        }
      },
    );
    unlistenDownloadFailed = await listen<DownloadStatusEvent>(
      Events.ModelDownloadFailed,
      (e) => {
        modelFetch.failed.set(
          e.payload.id,
          e.payload.message ?? "Download failed.",
        );
        modelFetch.downloading.delete(e.payload.id);
      },
    );

    // Cross-window / cross-context deep link to a specific tab.
    // Same listener works whether this panel is hosted in the
    // standalone window or inline on the main page; the parent's
    // `bind:activeTab` keeps the state visible to whichever
    // shell needs to react.
    unlistenGotoTab = await listen<string>(Events.SettingsGotoTab, (e) => {
      const target = e.payload;
      if (
        target === "general" ||
        target === "model" ||
        target === "vocabulary" ||
        target === "replacements" ||
        target === "meeting" ||
        target === "permissions" ||
        (target === "debug" && debugConsoleEnabled)
      ) {
        activeTab = target;
      }
    });

    debugConsoleEnabled = readDebugConsoleEnabled();
    await loadModels();
  });

  onDestroy(() => {
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
    unlistenGotoTab?.();
  });
</script>

<div class="settings-panel">
  <header class="settings-panel-header">
    <h1 class="settings-panel-title">Settings</h1>
    <nav class="settings-panel-toolbar" aria-label="Settings categories">
      {#each tabs as tab (tab.key)}
        <button
          type="button"
          class="tab-button"
          class:active={activeTab === tab.key}
          aria-current={activeTab === tab.key ? "page" : undefined}
          data-testid={tab.testId}
          onclick={() => (activeTab = tab.key)}
        >
          {tab.label}
        </button>
      {/each}
    </nav>
  </header>

  <section class="tab-body" aria-live="polite">
    {#if activeTab === "general"}
      <GeneralTab {onDebugConsoleChange} />
    {:else if activeTab === "model"}
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
    {:else if activeTab === "vocabulary"}
      <VocabularyTab />
    {:else if activeTab === "replacements"}
      <ReplacementsTab />
    {:else if activeTab === "meeting"}
      <MeetingTab />
    {:else if activeTab === "permissions"}
      <PermissionsTab />
    {:else if activeTab === "debug"}
      <DebugTab />
    {/if}
  </section>
</div>

<style>
  .settings-panel {
    display: flex;
    flex-direction: column;
    /* No fixed height — the host (`<main class="settings-window">`
       on /settings, `<main class="app-main">` on the inline shell)
       owns the scrolling container. */
  }

  .settings-panel-header {
    position: sticky;
    top: 0;
    z-index: 1;
    background-color: var(--bg-sidebar);
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .settings-panel-title {
    margin: 0;
    padding: 0.85rem 1.1rem 0.25rem;
    font-size: 1.55rem;
    font-weight: 700;
    color: var(--text-primary);
    letter-spacing: -0.015em;
  }

  .settings-panel-toolbar {
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
    color: var(--text-secondary);
    font-family: inherit;
    font-size: 0.85rem;
    font-weight: 500;
    border-radius: 6px;
    cursor: pointer;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  }
  .tab-button:hover {
    background-color: var(--accent-subtle);
    color: var(--text-primary);
  }
  .tab-button.active {
    background-color: var(--bg-surface);
    border-color: var(--border);
    color: var(--accent);
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

  /* Inner panels were originally tuned for the standalone window's
     720 px width; match that here so the model cards / lists
     breathe inside the inline panel too. */
  .tab-body :global(section.panel-models),
  .tab-body :global(section.panel-vocabulary),
  .tab-body :global(section.panel-replacements),
  .tab-body :global(section.panel-macos-diagnostic) {
    max-width: 44rem;
    margin-left: auto;
    margin-right: auto;
  }
  .tab-body :global(.panel-models),
  .tab-body :global(.panel-vocabulary),
  .tab-body :global(.panel-replacements),
  .tab-body :global(.panel-macos-diagnostic) {
    margin-top: 0;
  }
</style>
