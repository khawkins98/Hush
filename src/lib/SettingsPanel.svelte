<!--
  Settings panel — inline in the main window's third sidebar slot.
  Extracted from `routes/settings/+page.svelte` in #479; the
  standalone Settings window was removed in the same PR.

  Owns:
    - Tab strip + active-tab state (`activeTab`, bindable so the
      orchestrator can deep-link from menus / palette / banners).
    - Model picker state + lifecycle (model_list / model_select /
      model_download / model_remove + the 3 download-progress
      Tauri event listeners).
    - The `settings:goto-tab` Tauri-event listener so the active
      tab can be flipped programmatically from menus or the
      command palette.

  Each tab's per-tab state still lives inside its dedicated
  `*Tab.svelte` (GeneralTab, VocabularyTab, etc., per the #332
  phase 1 split). This panel is the host shell.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import DebugTab from "./DebugTab.svelte";
  import GeneralTab from "./GeneralTab.svelte";
  import MeetingTab from "./MeetingTab.svelte";
  import ModelPickerPanel from "./ModelPickerPanel.svelte";
  import PermissionsTab from "./PermissionsTab.svelte";
  import ReplacementsTab from "./ReplacementsTab.svelte";
  import VocabularyTab from "./VocabularyTab.svelte";
  import { formatErrorDisplay } from "./errors";
  import { Events } from "./events";
  import { formatMb } from "./format";
  import { nav } from "$lib/state/nav.svelte";
  import { diarizer } from "$lib/state/diarizer.svelte";
  import { models } from "$lib/state/models.svelte";
  import type { SettingsTab } from "$lib/settings-tabs";
  import type { IpcError, ModelCard } from "./types";

  // Re-export so callers that import the type from here still compile.
  export type { SettingsTab };

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

  function onDebugConsoleChange(enabled: boolean) {
    nav.setDebugConsoleEnabled(enabled);
  }

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;
  let unlistenGotoTab: UnlistenFn | null = null;

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", {
        id: card.id,
      });
      models.restartNotice = result.loaded ? "loaded" : "needs-restart";
      models.error = null;
      if (result.loaded) {
        onModelLoaded?.();
      }
      await models.loadModels();
    } catch (e) {
      models.error = formatErrorDisplay(e);
      if (typeof e === "object" && e !== null && "kind" in e) {
        const ipc = e as IpcError;
        if (ipc.kind === "model-not-downloaded") {
          models.restartNotice = "needs-download";
        }
      }
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
        models.downloading.set(e.payload.id, {
          received: e.payload.bytesReceived,
          total: e.payload.bytesTotal,
        });
      },
    );
    unlistenDownloadDone = await listen<DownloadStatusEvent>(
      Events.ModelDownloadDone,
      async (e) => {
        models.downloading.delete(e.payload.id);
        void models.loadModels();
        await diarizer.maybeAutoDownload(e.payload.id);
      },
    );
    unlistenDownloadFailed = await listen<DownloadStatusEvent>(
      Events.ModelDownloadFailed,
      (e) => {
        models.failed.set(
          e.payload.id,
          e.payload.message ?? "Download failed.",
        );
        models.downloading.delete(e.payload.id);
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
        (target === "debug" && nav.debugConsoleEnabled)
      ) {
        activeTab = target;
      }
    });

    await models.loadModels();
  });

  onDestroy(() => {
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
    unlistenGotoTab?.();
  });
</script>

<div class="settings-panel">
  <section class="settings-content" aria-live="polite">
    {#if activeTab === "general"}
      <GeneralTab {onDebugConsoleChange} />
    {:else if activeTab === "model"}
      <ModelPickerPanel
        models={models.models}
        modelsLoaded={models.loaded}
        modelsError={models.error}
        modelsRestartNotice={models.restartNotice}
        downloading={models.downloading}
        downloadFailed={models.failed}
        {formatMb}
        onSelect={selectModel}
        onDownload={(card) => models.downloadModel(card)}
        onCancel={(card) => models.cancelDownload(card)}
        onRemove={(card) => models.removeModel(card)}
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
    width: 100%;
  }

  .settings-content {
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
