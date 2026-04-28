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
  import { getName, getTauriVersion, getVersion } from "@tauri-apps/api/app";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    disable as disableAutostart,
    enable as enableAutostart,
    isEnabled as isAutostartEnabled,
  } from "@tauri-apps/plugin-autostart";
  import { onDestroy, onMount, tick } from "svelte";

  import MacosDiagnosticPanel from "$lib/MacosDiagnosticPanel.svelte";
  import MeetingAppOverridesPanel from "$lib/MeetingAppOverridesPanel.svelte";
  import ModelPickerPanel from "$lib/ModelPickerPanel.svelte";
  import PttHotkeyEditor from "$lib/PttHotkeyEditor.svelte";
  import ReplacementsPanel from "$lib/ReplacementsPanel.svelte";
  import VocabularyPanel from "$lib/VocabularyPanel.svelte";
  import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
  import { formatMb } from "$lib/format";
  import type {
    DownloadProgress,
    IpcError,
    MacosPermissionDiagnostic,
    MacosPermissionResetResult,
    MeetingAppKind,
    MeetingAppOverride,
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
  const isMacOS = typeof navigator !== "undefined"
    && /Mac|iPhone|iPad/i.test(navigator.platform);

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
  let models = $state<ModelCard[]>([]);
  let modelsLoaded = $state(false);
  let modelsError = $state<ErrorDisplay | null>(null);
  let modelsRestartNotice = $state<ModelSelectNotice>(null);
  let downloading = $state<Map<string, DownloadProgress>>(new Map());
  let downloadFailed = $state<Map<string, string>>(new Map());

  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenDownloadFailed: UnlistenFn | null = null;
  let unlistenGotoTab: UnlistenFn | null = null;

  // ---- General-tab state ------------------------------------------------
  // Autostart toggle: queried on mount via the autostart plugin and
  // mirrored locally so the checkbox can show optimistic UI while the
  // plugin call is in flight.
  let autostartEnabled = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state<string | null>(null);
  // First-run reset: brief confirmation message replaces the button
  // label after a successful reset, then clears on a 3 s timer.
  let firstRunResetBusy = $state(false);
  let firstRunResetMessage = $state<string | null>(null);

  // ---- Vocabulary state --------------------------------------------------
  let vocabulary = $state<VocabularyTerm[]>([]);
  let vocabularyLoaded = $state(false);
  let vocabularyError = $state<ErrorDisplay | null>(null);
  let newVocab = $state("");
  let vocabInputEl = $state<HTMLInputElement | null>(null);

  // ---- Replacements state -----------------------------------------------
  let replacements = $state<ReplacementRule[]>([]);
  let replacementsLoaded = $state(false);
  let replacementsError = $state<ErrorDisplay | null>(null);
  let newFind = $state("");
  let newReplace = $state("");
  let findInputEl = $state<HTMLInputElement | null>(null);

  // ---- Meeting app classification overrides (Phase E, #112) -------------
  let appOverrides = $state<MeetingAppOverride[]>([]);
  let appOverridesLoaded = $state(false);
  let appOverridesError = $state<ErrorDisplay | null>(null);
  let newOverrideName = $state("");
  let newOverrideKind = $state<MeetingAppKind>("meeting");
  let overrideInputEl = $state<HTMLInputElement | null>(null);

  // ---- About tab --------------------------------------------------------
  // Version pulled from Tauri at runtime so the displayed value
  // tracks `tauri.conf.json` / `Cargo.toml` instead of a hardcoded
  // string that would silently rot. `getName` returns the
  // `productName` field, which is what users see in the menu bar.
  let appVersion = $state<string>("");
  let appName = $state<string>("Hush");
  let tauriVersion = $state<string>("");

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
      modelsError = formatErrorDisplay(e);
    } finally {
      modelsLoaded = true;
    }
  }

  async function loadVocabulary(): Promise<void> {
    try {
      vocabulary = await invoke<VocabularyTerm[]>("vocabulary_list");
      vocabularyError = null;
    } catch (e) {
      vocabularyError = formatErrorDisplay(e);
    } finally {
      vocabularyLoaded = true;
    }
  }

  async function loadReplacements(): Promise<void> {
    try {
      replacements = await invoke<ReplacementRule[]>("replacements_list");
      replacementsError = null;
    } catch (e) {
      replacementsError = formatErrorDisplay(e);
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

  async function loadAppOverrides(): Promise<void> {
    try {
      appOverrides = await invoke<MeetingAppOverride[]>(
        "meeting_app_override_list",
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    } finally {
      appOverridesLoaded = true;
    }
  }

  async function addAppOverride(e: Event) {
    e.preventDefault();
    const name = newOverrideName.trim();
    if (!name) return;
    try {
      const created = await invoke<MeetingAppOverride>(
        "meeting_app_override_upsert",
        { appName: name, kind: newOverrideKind },
      );
      // Replace any existing entry for this app (upsert) and resort
      // by app name so the rendered order matches the backend's
      // ORDER BY.
      appOverrides = [
        ...appOverrides.filter((o) => o.appName !== created.appName),
        created,
      ].sort((a, b) => a.appName.localeCompare(b.appName));
      newOverrideName = "";
      newOverrideKind = "meeting";
      appOverridesError = null;
      await tick();
      overrideInputEl?.focus();
    } catch (err) {
      appOverridesError = formatErrorDisplay(err);
    }
  }

  async function changeAppOverrideKind(
    override: MeetingAppOverride,
    kind: MeetingAppKind,
  ) {
    try {
      const updated = await invoke<MeetingAppOverride>(
        "meeting_app_override_upsert",
        { appName: override.appName, kind },
      );
      appOverrides = appOverrides.map((o) =>
        o.appName === updated.appName ? updated : o,
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    }
  }

  async function deleteAppOverride(override: MeetingAppOverride) {
    try {
      await invoke("meeting_app_override_delete", {
        appName: override.appName,
      });
      appOverrides = appOverrides.filter(
        (o) => o.appName !== override.appName,
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
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
      vocabularyError = formatErrorDisplay(err);
    }
  }

  async function deleteVocabulary(term: VocabularyTerm) {
    try {
      await invoke("vocabulary_delete", { id: term.id });
      vocabulary = vocabulary.filter((v) => v.id !== term.id);
      vocabularyError = null;
    } catch (e) {
      vocabularyError = formatErrorDisplay(e);
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
      replacementsError = formatErrorDisplay(err);
    }
  }

  async function deleteReplacement(rule: ReplacementRule) {
    try {
      await invoke("replacement_delete", { id: rule.id });
      replacements = replacements.filter((r) => r.id !== rule.id);
      replacementsError = null;
    } catch (e) {
      replacementsError = formatErrorDisplay(e);
    }
  }

  async function selectModel(card: ModelCard) {
    try {
      const result = await invoke<{ loaded: boolean }>("model_select", { id: card.id });
      modelsRestartNotice = result.loaded ? "loaded" : "needs-restart";
      modelsError = null;
      await loadModels();
    } catch (e) {
      modelsError = formatErrorDisplay(e);
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
      modelsError = formatErrorDisplay(e);
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
        target === "meeting" ||
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
      loadAutostartState(),
      loadAppMetadata(),
      loadAppOverrides(),
    ]);
  });

  // Pull build identity from Tauri. Failures are non-fatal — the
  // About tab just falls back to the default empty strings, which
  // render as "Hush" + an empty version line.
  async function loadAppMetadata(): Promise<void> {
    try {
      const [name, version, tauri] = await Promise.all([
        getName(),
        getVersion(),
        getTauriVersion(),
      ]);
      appName = name;
      appVersion = version;
      tauriVersion = tauri;
    } catch {
      // Ignored; the About tab just shows the static copy.
    }
  }

  // ---- General-tab handlers --------------------------------------------

  async function loadAutostartState(): Promise<void> {
    try {
      autostartEnabled = await isAutostartEnabled();
      autostartError = null;
    } catch (e) {
      // Plugin missing on this build / platform. Treat as disabled
      // and surface a single-line note rather than the raw error.
      autostartEnabled = false;
      autostartError = "Couldn't read autostart state on this platform.";
      console.warn("[hush] isAutostartEnabled failed", e);
    }
  }

  async function onAutostartToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    autostartBusy = true;
    autostartError = null;
    try {
      if (checked) await enableAutostart();
      else await disableAutostart();
      autostartEnabled = checked;
    } catch (err) {
      autostartError = formatError(err);
      // Re-read so the checkbox reverts to truth rather than the
      // optimistic state that didn't persist.
      await loadAutostartState();
    } finally {
      autostartBusy = false;
    }
  }

  async function onResetFirstRun() {
    firstRunResetBusy = true;
    try {
      await invoke("reset_first_run");
      firstRunResetMessage = "Welcome will show on next launch.";
      // Clear the confirmation after a moment so the button label
      // returns to its actionable state (in case the user changes
      // their mind in this same session).
      setTimeout(() => {
        firstRunResetMessage = null;
      }, 3000);
    } catch (e) {
      firstRunResetMessage = formatError(e);
    } finally {
      firstRunResetBusy = false;
    }
  }

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

      <section class="settings-group" aria-labelledby="settings-startup-heading">
        <h2 id="settings-startup-heading" class="group-heading">Startup</h2>
        <label class="toggle-row">
          <input
            type="checkbox"
            data-testid="settings-autostart-toggle"
            disabled={autostartBusy}
            checked={autostartEnabled}
            onchange={onAutostartToggle}
          />
          <span class="toggle-label">
            <span class="toggle-name">Launch Hush at login</span>
            <span class="toggle-desc">
              Hush opens automatically when you sign in. The window
              stays in the background — your hotkey still works.
            </span>
          </span>
        </label>
        {#if autostartError}
          <p class="settings-error">{autostartError}</p>
        {/if}
      </section>

      <section class="settings-group" aria-labelledby="settings-hotkeys-heading">
        <h2 id="settings-hotkeys-heading" class="group-heading">Hotkeys</h2>
        <p class="settings-row">
          <span class="row-label">Toggle recording</span>
          <span class="row-value">
            <span class="chord"><kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd></span>
            <span class="row-note">Customisable hotkey UI is future work.</span>
          </span>
        </p>
        <h3 class="subgroup-heading">Push-to-talk</h3>
        <PttHotkeyEditor {isMacOS} />
      </section>

      <section class="settings-group" aria-labelledby="settings-firstrun-heading">
        <h2 id="settings-firstrun-heading" class="group-heading">First-run welcome</h2>
        <p class="settings-row settings-row-stack">
          <button
            type="button"
            class="ghost"
            data-testid="settings-reset-first-run"
            disabled={firstRunResetBusy}
            onclick={onResetFirstRun}
          >
            {firstRunResetMessage ?? "Show welcome on next launch"}
          </button>
          <span class="row-note">
            Re-shows the permissions explainer the next time you open
            Hush. Doesn't affect any other state.
          </span>
        </p>
      </section>
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
    {:else if active === "meeting"}
      <h1 class="tab-title">Meeting</h1>
      <MeetingAppOverridesPanel
        overrides={appOverrides}
        overridesLoaded={appOverridesLoaded}
        overridesError={appOverridesError}
        bind:newAppName={newOverrideName}
        bind:newKind={newOverrideKind}
        bind:inputEl={overrideInputEl}
        onSubmit={addAppOverride}
        onChangeKind={changeAppOverrideKind}
        onDelete={deleteAppOverride}
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
      <section class="about-tab">
        <header class="about-header">
          <h2 class="about-name">{appName}</h2>
          {#if appVersion}
            <p class="about-version">Version {appVersion}</p>
          {/if}
        </header>

        <p class="about-blurb">
          Local-only voice-to-text. Hotkey-driven dictation plus
          long-running meeting capture, powered by whisper.cpp on
          your own hardware. No cloud, no telemetry.
        </p>

        <dl class="about-meta">
          <dt>License</dt>
          <dd>
            <a
              href="https://www.apache.org/licenses/LICENSE-2.0"
              target="_blank"
              rel="noopener noreferrer">Apache License 2.0</a
            >
          </dd>
          <dt>Source</dt>
          <dd>
            <a
              href="https://github.com/khawkins98/Hush"
              target="_blank"
              rel="noopener noreferrer">github.com/khawkins98/Hush</a
            >
          </dd>
          <dt>Report a bug</dt>
          <dd>
            <a
              href="https://github.com/khawkins98/Hush/issues/new"
              target="_blank"
              rel="noopener noreferrer">Open an issue</a
            >
          </dd>
          {#if tauriVersion}
            <dt>Tauri runtime</dt>
            <dd><code>{tauriVersion}</code></dd>
          {/if}
        </dl>

        <p class="about-credit">
          Built on
          <a
            href="https://github.com/ggerganov/whisper.cpp"
            target="_blank"
            rel="noopener noreferrer">whisper.cpp</a
          >,
          <a
            href="https://tauri.app"
            target="_blank"
            rel="noopener noreferrer">Tauri</a
          >, and
          <a
            href="https://svelte.dev"
            target="_blank"
            rel="noopener noreferrer">Svelte</a
          >.
        </p>
      </section>
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

  .settings-toolbar {
    display: flex;
    gap: 0.25rem;
    padding: 0.6rem 0.75rem;
    background-color: #ececef;
    border-bottom: 1px solid #d8d8dc;
    overflow-x: auto;
    flex-shrink: 0;
    /* Sticky inside the scrolling page so the tabs stay reachable
       once a tab body grows past the viewport. The settings window
       is the canonical case — General + Hotkeys + First-run reach
       beyond the default 520 px height — but the toolbar is also
       useful as an anchor on every tab. */
    position: sticky;
    top: 0;
    z-index: 1;
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

  .about-tab {
    max-width: 36rem;
    line-height: 1.5;
  }
  .about-header {
    margin-bottom: 1.25rem;
  }
  .about-name {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
  }
  .about-version {
    margin: 0.15rem 0 0;
    color: #666;
    font-size: 0.85rem;
  }
  .about-blurb {
    margin: 0 0 1.25rem;
    font-size: 0.95rem;
    color: #333;
  }
  .about-meta {
    display: grid;
    grid-template-columns: max-content 1fr;
    column-gap: 1rem;
    row-gap: 0.4rem;
    margin: 0 0 1.25rem;
    font-size: 0.9rem;
  }
  .about-meta dt {
    color: #666;
    font-weight: 500;
  }
  .about-meta dd {
    margin: 0;
  }
  .about-meta code {
    font-family:
      ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85em;
    color: #444;
  }
  .about-credit {
    margin: 0;
    color: #666;
    font-size: 0.85rem;
  }
  .about-tab a {
    color: #396cd8;
  }
  @media (prefers-color-scheme: dark) {
    .about-version,
    .about-meta dt,
    .about-credit {
      color: #9a9a9a;
    }
    .about-blurb {
      color: #d8d8d8;
    }
    .about-meta code {
      color: #b8b8b8;
    }
    .about-tab a {
      color: #6a8cf0;
    }
  }

  .settings-group {
    margin: 0 0 1.75rem;
    max-width: 44rem;
  }
  .group-heading {
    margin: 0 0 0.6rem;
    font-size: 0.78rem;
    font-weight: 600;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .subgroup-heading {
    margin: 1rem 0 0.5rem;
    font-size: 0.85rem;
    font-weight: 600;
    color: #444;
  }
  @media (prefers-color-scheme: dark) {
    .subgroup-heading { color: #d0d0d0; }
  }

  .toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
    cursor: pointer;
  }
  .toggle-row input[type="checkbox"] {
    margin-top: 0.2rem;
    flex-shrink: 0;
  }
  .toggle-label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  .toggle-name {
    font-weight: 600;
    color: #222;
  }
  .toggle-desc {
    font-size: 0.82rem;
    color: #666;
    line-height: 1.4;
  }

  .settings-row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
    margin: 0 0 0.5rem;
    padding: 0.55rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .settings-row-stack {
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
  }
  .row-label {
    font-weight: 500;
    color: #333;
  }
  .row-value {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.2rem;
    color: #555;
  }
  /* Inline-flex chord wrapper keeps `<kbd> + <kbd> + <kbd>` on one
     line as a single flex item inside the column-flex `.row-value`,
     so the chord doesn't stack vertically next to the `.row-note`
     beneath it. Without this, each `<kbd>` and the `+` separators
     were treated as siblings and stacked. */
  .chord {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .row-note {
    display: block;
    font-size: 0.75rem;
    color: #888;
    text-align: right;
  }
  .settings-row-stack .row-note {
    text-align: left;
  }
  .settings-row-stack .row-note {
    text-align: left;
  }
  .settings-error {
    margin: 0.4rem 0 0;
    color: #8a1f1f;
    font-size: 0.85rem;
  }
  button.ghost {
    padding: 0.4em 0.85em;
    font-size: 0.85rem;
    font-weight: 500;
    background-color: white;
    border: 1px solid #d1d1d8;
    border-radius: 6px;
    cursor: pointer;
    color: #2c3e8f;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #f4f5fa;
    border-color: #b8c1d8;
  }
  button.ghost:disabled {
    opacity: 0.6;
    cursor: not-allowed;
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
    .toggle-row,
    .settings-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .toggle-name { color: #e8e8e8; }
    .toggle-desc { color: #a8a8a8; }
    .row-label { color: #d8d8d8; }
    .row-value { color: #b0b0b0; }
    .row-note { color: #888; }
    .group-heading { color: #888; }
    button.ghost {
      background-color: #2a2a2d;
      border-color: #38383b;
      color: #b8c8ff;
    }
    button.ghost:hover:not(:disabled) {
      background-color: #38383b;
      border-color: #4a4a4d;
    }
    kbd {
      background-color: #2a2a2d;
      border-color: #4a4a4d;
      color: #d8d8d8;
    }
  }
</style>
