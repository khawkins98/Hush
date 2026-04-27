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
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount, tick } from "svelte";

  import MacosDiagnosticPanel from "$lib/MacosDiagnosticPanel.svelte";
  import ModelPickerPanel from "$lib/ModelPickerPanel.svelte";
  import ReplacementsPanel from "$lib/ReplacementsPanel.svelte";
  import VocabularyPanel from "$lib/VocabularyPanel.svelte";
  import { formatMb } from "$lib/format";
  import type {
    DownloadProgress,
    IpcError,
    MacosPermissionDiagnostic,
    MacosPermissionResetResult,
    ModelCard,
    ModelSelectNotice,
    ReplacementRule,
    VocabularyTerm,
  } from "$lib/types";

  type SettingsTab =
    | "general"
    | "model"
    | "vocabulary"
    | "replacements"
    | "permissions"
    | "about";

  // Default landing tab. "general" matches the macOS Settings
  // convention; deep-links from the main window override via the
  // `settings:goto-tab` listener registered in `onMount`.
  let active = $state<SettingsTab>("general");

  const tabs: Array<{ key: SettingsTab; label: string; testId: string }> = [
    { key: "general", label: "General", testId: "settings-tab-general" },
    { key: "model", label: "Model", testId: "settings-tab-model" },
    { key: "vocabulary", label: "Vocabulary", testId: "settings-tab-vocabulary" },
    { key: "replacements", label: "Replacements", testId: "settings-tab-replacements" },
    { key: "permissions", label: "Permissions", testId: "settings-tab-permissions" },
    { key: "about", label: "About", testId: "settings-tab-about" },
  ];

  // ---- Model picker state ------------------------------------------------
  let models = $state<ModelCard[]>([]);
  let modelsLoaded = $state(false);
  let modelsError = $state<string | null>(null);
  let modelsRestartNotice = $state<ModelSelectNotice>(null);
  let downloading = $state<Map<string, DownloadProgress>>(new Map());
  let downloadFailed = $state<Map<string, string>>(new Map());

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;
  let unlistenGotoTab: UnlistenFn | null = null;

  // ---- Vocabulary state --------------------------------------------------
  let vocabulary = $state<VocabularyTerm[]>([]);
  let vocabularyLoaded = $state(false);
  let vocabularyError = $state<string | null>(null);
  let newVocab = $state("");
  let vocabInputEl = $state<HTMLInputElement | null>(null);

  // ---- Replacements state -----------------------------------------------
  let replacements = $state<ReplacementRule[]>([]);
  let replacementsLoaded = $state(false);
  let replacementsError = $state<string | null>(null);
  let newFind = $state("");
  let newReplace = $state("");
  let findInputEl = $state<HTMLInputElement | null>(null);

  // ---- macOS permission diagnostic --------------------------------------
  let macosDiagnostic = $state<MacosPermissionDiagnostic | null>(null);
  let macosDiagnosticOpen = $state(true); // open by default in the dedicated tab
  let macosResetMessage = $state<string | null>(null);
  let macosResetting = $state(false);

  // ---- Error formatting (subset of main window's helper) ----------------
  function formatError(e: unknown): string {
    if (typeof e === "object" && e !== null && "kind" in e) {
      const ipc = e as IpcError;
      if (ipc.kind === "transcription") {
        return `Transcription failed: ${ipc.message ?? "unknown"}.`;
      }
      return ipc.message ? `${ipc.kind}: ${ipc.message}` : ipc.kind;
    }
    return String(e);
  }

  // ---- Loaders -----------------------------------------------------------

  async function loadModels(): Promise<void> {
    try {
      models = await invoke<ModelCard[]>("model_list");
      modelsError = null;
    } catch (e) {
      modelsError = formatError(e);
    } finally {
      modelsLoaded = true;
    }
  }

  async function loadVocabulary(): Promise<void> {
    try {
      vocabulary = await invoke<VocabularyTerm[]>("vocabulary_list");
      vocabularyError = null;
    } catch (e) {
      vocabularyError = formatError(e);
    } finally {
      vocabularyLoaded = true;
    }
  }

  async function loadReplacements(): Promise<void> {
    try {
      replacements = await invoke<ReplacementRule[]>("replacements_list");
      replacementsError = null;
    } catch (e) {
      replacementsError = formatError(e);
    } finally {
      replacementsLoaded = true;
    }
  }

  async function loadMacosDiagnostic(): Promise<void> {
    try {
      const res = await invoke<MacosPermissionDiagnostic>(
        "diagnose_macos_permissions",
      );
      macosDiagnostic = res.canReset ? res : null;
    } catch {
      macosDiagnostic = null;
    }
  }

  // ---- Mutators ----------------------------------------------------------

  async function addVocabulary(e: Event) {
    e.preventDefault();
    const term = newVocab.trim();
    if (!term) return;
    try {
      const created = await invoke<VocabularyTerm>("vocabulary_create", { term });
      vocabulary = [...vocabulary, created];
      newVocab = "";
      vocabularyError = null;
      await tick();
      vocabInputEl?.focus();
    } catch (err) {
      vocabularyError = formatError(err);
    }
  }

  async function deleteVocabulary(term: VocabularyTerm) {
    try {
      await invoke("vocabulary_delete", { id: term.id });
      vocabulary = vocabulary.filter((v) => v.id !== term.id);
      vocabularyError = null;
    } catch (e) {
      vocabularyError = formatError(e);
    }
  }

  async function addReplacement(e: Event) {
    e.preventDefault();
    const find = newFind.trim();
    const replace = newReplace;
    if (!find) return;
    try {
      const created = await invoke<ReplacementRule>("replacement_create", {
        findText: find,
        replaceText: replace,
        sortOrder: replacements.length,
      });
      replacements = [...replacements, created];
      newFind = "";
      newReplace = "";
      replacementsError = null;
      await tick();
      findInputEl?.focus();
    } catch (err) {
      replacementsError = formatError(err);
    }
  }

  async function deleteReplacement(rule: ReplacementRule) {
    try {
      await invoke("replacement_delete", { id: rule.id });
      replacements = replacements.filter((r) => r.id !== rule.id);
      replacementsError = null;
    } catch (e) {
      replacementsError = formatError(e);
    }
  }

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", { id: card.id });
      modelsRestartNotice = result.loaded ? "loaded" : "needs-restart";
      modelsError = null;
      await loadModels();
    } catch (e) {
      const formatted = formatError(e);
      modelsError = formatted;
      if (typeof e === "object" && e !== null && "kind" in e) {
        const ipc = e as IpcError;
        if (ipc.kind === "model-not-downloaded") {
          modelsRestartNotice = "needs-download";
        }
      }
    }
  }

  async function downloadModel(card: ModelCard) {
    downloadFailed = new Map(downloadFailed);
    downloadFailed.delete(card.id);
    downloading = new Map(downloading);
    downloading.set(card.id, { received: 0, total: null });
    try {
      await invoke("model_download", { id: card.id });
    } catch (e) {
      const failed = new Map(downloadFailed);
      failed.set(card.id, formatError(e));
      downloadFailed = failed;
      const next = new Map(downloading);
      next.delete(card.id);
      downloading = next;
    }
  }

  async function cancelDownload(card: ModelCard) {
    try {
      await invoke("model_cancel_download", { id: card.id });
    } catch (e) {
      console.warn("[hush] cancel download failed", e);
    }
    const next = new Map(downloading);
    next.delete(card.id);
    downloading = next;
  }

  async function removeModel(card: ModelCard) {
    try {
      await invoke("model_remove", { id: card.id });
      await loadModels();
    } catch (e) {
      modelsError = formatError(e);
    }
  }

  async function openPrivacyPane(target: "microphone" | "input-monitoring") {
    try {
      await invoke("open_macos_privacy_pane", { target });
    } catch (e) {
      console.warn("[hush] open privacy pane failed", e);
    }
  }

  async function runMacosReset() {
    macosResetting = true;
    macosResetMessage = null;
    try {
      const res = await invoke<MacosPermissionResetResult>(
        "reset_macos_permissions",
      );
      macosResetMessage = res.summary;
    } catch (e) {
      macosResetMessage = formatError(e);
    } finally {
      macosResetting = false;
    }
  }

  // ---- Lifecycle ---------------------------------------------------------

  onMount(async () => {
    type DownloadProgressEvent = { id: string; bytesReceived: number; bytesTotal: number | null };
    type DownloadStatusEvent = { id: string; message: string | null };

    unlistenDownloadProgress = await listen<DownloadProgressEvent>(
      "model:download-progress",
      (e) => {
        const next = new Map(downloading);
        next.set(e.payload.id, {
          received: e.payload.bytesReceived,
          total: e.payload.bytesTotal,
        });
        downloading = next;
      },
    );
    unlistenDownloadDone = await listen<DownloadStatusEvent>("model:download-done", (e) => {
      const next = new Map(downloading);
      next.delete(e.payload.id);
      downloading = next;
      void loadModels();
    });
    unlistenDownloadFailed = await listen<DownloadStatusEvent>("model:download-failed", (e) => {
      const failed = new Map(downloadFailed);
      failed.set(e.payload.id, e.payload.message ?? "Download failed.");
      downloadFailed = failed;
      const next = new Map(downloading);
      next.delete(e.payload.id);
      downloading = next;
    });

    // Deep-link from the main window's "Open the Permissions
    // diagnostic" link / future menu items. Payload is the tab key
    // — silently ignored if it isn't one we know, so future tabs
    // added on the main window don't crash a stale settings build.
    unlistenGotoTab = await listen<string>("settings:goto-tab", (e) => {
      const target = e.payload;
      if (
        target === "general" ||
        target === "model" ||
        target === "vocabulary" ||
        target === "replacements" ||
        target === "permissions" ||
        target === "about"
      ) {
        active = target;
      }
    });

    await Promise.all([
      loadModels(),
      loadVocabulary(),
      loadReplacements(),
      loadMacosDiagnostic(),
    ]);
  });

  onDestroy(() => {
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
    unlistenGotoTab?.();
  });
</script>

<main class="settings-window">
  <header class="settings-toolbar" aria-label="Settings categories">
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
  </header>

  <section class="tab-body" aria-live="polite">
    {#if active === "general"}
      <h1 class="tab-title">General</h1>
      <p class="placeholder">
        Hotkey, autostart, and first-run controls will live here in a
        future PR. For now Hush uses sensible defaults: toggle hotkey
        is <kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd>.
      </p>
    {:else if active === "model"}
      <ModelPickerPanel
        {models}
        {modelsLoaded}
        {modelsError}
        {modelsRestartNotice}
        {downloading}
        {downloadFailed}
        {formatMb}
        onSelect={selectModel}
        onDownload={downloadModel}
        onCancel={cancelDownload}
        onRemove={removeModel}
      />
    {:else if active === "vocabulary"}
      <VocabularyPanel
        {vocabulary}
        {vocabularyLoaded}
        {vocabularyError}
        bind:newVocab
        bind:inputEl={vocabInputEl}
        onSubmit={addVocabulary}
        onDelete={deleteVocabulary}
      />
    {:else if active === "replacements"}
      <ReplacementsPanel
        {replacements}
        {replacementsLoaded}
        {replacementsError}
        bind:newFind
        bind:newReplace
        bind:inputEl={findInputEl}
        onSubmit={addReplacement}
        onDelete={deleteReplacement}
      />
    {:else if active === "permissions"}
      {#if macosDiagnostic}
        <h1 class="tab-title">Permissions</h1>
        <ul class="perm-status-list" aria-label="Permission status summary">
          {#each [
            { key: "microphone", label: "Microphone", status: macosDiagnostic.statuses.microphone, why: "Required for dictation." },
            { key: "screenRecording", label: "Screen Recording", status: macosDiagnostic.statuses.screenRecording, why: "Required for system-audio capture in meetings." },
            { key: "inputMonitoring", label: "Input Monitoring", status: macosDiagnostic.statuses.inputMonitoring, why: "Optional — only used if you opt into push-to-talk via HUSH_PTT_ENABLE=1." },
          ] as row (row.key)}
            <li class="perm-row" data-perm={row.key} data-status={row.status}>
              <span class="perm-dot" aria-hidden="true"></span>
              <span class="perm-name">{row.label}</span>
              <span class="perm-status-label">
                {#if row.status === "granted"}Granted
                {:else if row.status === "denied"}Denied
                {:else if row.status === "not-determined"}Not yet granted
                {:else}Not applicable
                {/if}
              </span>
              <span class="perm-why">{row.why}</span>
            </li>
          {/each}
        </ul>
        <p class="perm-recovery-intro">
          Need to fix something? The diagnostic below has the
          recovery details and a one-click <code>tccutil reset</code>.
        </p>
        <MacosDiagnosticPanel
          {macosDiagnostic}
          bind:macosDiagnosticOpen
          {macosResetMessage}
          {macosResetting}
          onOpenPrivacyPane={openPrivacyPane}
          onReset={runMacosReset}
        />
      {:else}
        <h1 class="tab-title">Permissions</h1>
        <p class="placeholder">
          Permission diagnostics are macOS-only. There's nothing
          actionable to surface on this platform.
        </p>
      {/if}
    {:else if active === "about"}
      <h1 class="tab-title">About</h1>
      <p class="placeholder">
        Hush — local-only voice-to-text. Hotkey-driven dictation +
        long-running meeting capture, all powered by whisper.cpp on
        your own hardware. No cloud, no telemetry.
      </p>
    {/if}
  </section>
</main>

<style>
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: #f3f3f5;
    color: #0f0f0f;
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  .settings-window {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }

  .settings-toolbar {
    display: flex;
    gap: 0.25rem;
    padding: 0.6rem 0.75rem;
    background-color: #ececef;
    border-bottom: 1px solid #d8d8dc;
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
    outline: 2px solid #6a8cf0;
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

  .tab-title {
    margin: 0 0 0.75rem;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }

  .placeholder {
    margin: 0;
    color: #666;
    font-size: 0.95rem;
    line-height: 1.5;
    max-width: 36rem;
  }

  .perm-status-list {
    list-style: none;
    margin: 0 0 1.5rem;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
    max-width: 44rem;
  }
  .perm-row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    grid-template-areas:
      "dot name status"
      "dot why  why";
    gap: 0.15rem 0.6rem;
    align-items: center;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .perm-dot {
    grid-area: dot;
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 50%;
    background-color: #b8b8c0;
    box-shadow: 0 0 0 2px rgba(184, 184, 192, 0.18);
  }
  .perm-row[data-status="granted"] .perm-dot {
    background-color: #2eaa53;
    box-shadow: 0 0 0 2px rgba(46, 170, 83, 0.18);
  }
  .perm-row[data-status="denied"] .perm-dot {
    background-color: #d83a3a;
    box-shadow: 0 0 0 2px rgba(216, 58, 58, 0.18);
  }
  .perm-row[data-status="not-determined"] .perm-dot {
    background-color: #e8a72b;
    box-shadow: 0 0 0 2px rgba(232, 167, 43, 0.18);
  }
  .perm-name {
    grid-area: name;
    font-weight: 600;
    color: #222;
  }
  .perm-status-label {
    grid-area: status;
    font-size: 0.78rem;
    font-weight: 500;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .perm-row[data-status="granted"] .perm-status-label { color: #2a6b3c; }
  .perm-row[data-status="denied"] .perm-status-label { color: #8a1f1f; }
  .perm-row[data-status="not-determined"] .perm-status-label { color: #8a5a00; }
  .perm-why {
    grid-area: why;
    font-size: 0.82rem;
    color: #666;
  }
  .perm-recovery-intro {
    margin: 0 0 1rem;
    font-size: 0.85rem;
    color: #555;
    max-width: 44rem;
  }
  .perm-recovery-intro code {
    background-color: #f0f0f3;
    padding: 0.1em 0.35em;
    border-radius: 4px;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.92em;
  }

  @media (prefers-color-scheme: dark) {
    .perm-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .perm-name { color: #e8e8e8; }
    .perm-why { color: #a8a8a8; }
    .perm-recovery-intro { color: #b0b0b0; }
    .perm-recovery-intro code {
      background-color: #38383b;
      color: #d8d8d8;
    }
  }

  kbd {
    display: inline-block;
    padding: 0.05em 0.35em;
    border: 1px solid #d1d1d8;
    border-radius: 4px;
    background-color: #fafafa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.85em;
  }

  @media (prefers-color-scheme: dark) {
    :global(html), :global(body) {
      background-color: #1d1d1f;
      color: #e8e8e8;
    }
    .settings-toolbar {
      background-color: #2a2a2d;
      border-bottom-color: #38383b;
    }
    .tab-button { color: #d8d8d8; }
    .tab-button:hover { background-color: rgba(255, 255, 255, 0.06); }
    .tab-button.active {
      background-color: #1d1d1f;
      border-color: #38383b;
      color: #b8c8ff;
    }
    .placeholder { color: #a8a8a8; }
    kbd {
      background-color: #2a2a2d;
      border-color: #4a4a4d;
      color: #d8d8d8;
    }
  }
</style>
