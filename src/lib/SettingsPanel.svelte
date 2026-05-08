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
  <nav class="settings-sidebar" aria-label="Settings categories">
    <span class="settings-sidebar-title">Settings</span>
    {#each tabs as tab (tab.key)}
      <button
        type="button"
        class="settings-sidebar-btn"
        class:active={activeTab === tab.key}
        aria-current={activeTab === tab.key ? "page" : undefined}
        data-testid={tab.testId}
        onclick={() => (activeTab = tab.key)}
      >
        {tab.label}
      </button>
    {/each}
  </nav>

  <section class="settings-content" aria-live="polite">
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
    flex-direction: row;
    align-items: flex-start;
  }

  /* Left sidebar nav — sticks in place while content scrolls in the
     outer `.app-main` scroll container. Same left-chrome accent
     indicator style as the main SidebarNav. */
  .settings-sidebar {
    width: 140px;
    flex-shrink: 0;
    position: sticky;
    top: 0;
    max-height: 100vh;
    background-color: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    padding: 0.85rem 0 2rem;
    display: flex;
    flex-direction: column;
  }

  .settings-sidebar-title {
    display: block;
    padding: 0 1.1rem 0.75rem;
    font-size: 1.4rem;
    font-weight: 700;
    color: var(--text-primary);
    letter-spacing: -0.015em;
  }

  .settings-sidebar-btn {
    appearance: none;
    background: transparent;
    border: none;
    border-left: 3px solid transparent;
    padding: 0.5rem 1rem;
    width: 100%;
    text-align: left;
    font-size: 0.875rem;
    font-family: inherit;
    font-weight: 500;
    color: var(--text-muted);
    cursor: pointer;
    white-space: nowrap;
    transition: color 120ms ease, border-color 120ms ease;
  }
  .settings-sidebar-btn:hover {
    color: var(--text-primary);
  }
  .settings-sidebar-btn.active {
    color: var(--accent);
    border-left-color: var(--accent);
    font-weight: 600;
  }
  .settings-sidebar-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -3px;
  }

  .settings-content {
    flex: 1;
    padding: 2rem 2.5rem;
    width: 100%;
    box-sizing: border-box;
    min-width: 0;
  }

  /* Inner panels tuned for ~560 px available width; centred so
     model cards / lists breathe inside the inline panel too. */
  .settings-content :global(section.panel-models),
  .settings-content :global(section.panel-vocabulary),
  .settings-content :global(section.panel-replacements),
  .settings-content :global(section.panel-macos-diagnostic) {
    max-width: 44rem;
    margin-left: auto;
    margin-right: auto;
  }
  .settings-content :global(.panel-models),
  .settings-content :global(.panel-vocabulary),
  .settings-content :global(.panel-replacements),
  .settings-content :global(.panel-macos-diagnostic) {
    margin-top: 0;
  }
</style>
