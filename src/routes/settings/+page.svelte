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
  let unlistenUpdaterResult: UnlistenFn | null = null;
  /// Handle for the window-focus listener wired in onMount. Stored
  /// so onDestroy can remove it; without removal a closed-then-
  /// reopened Settings window would accumulate listeners.
  let settingsFocusHandler: ((this: Window, ev: FocusEvent) => void) | null = null;

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

  // HUD-overlay-enabled toggle. Defaults to true on the backend
  // (the recording HUD is on by default — first-time users
  // benefit from the visual cue that the mic is hot). Power
  // users who'd rather not see the floating pill can flip it off
  // here. Optimistically updated on click; on failure the
  // checkbox snaps back to the persisted value.
  let hudEnabled = $state(true);
  let hudBusy = $state(false);
  let hudError = $state<string | null>(null);

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

  // Meeting auto-start mode dropdown. Backend serde encoding is
  // kebab-case ("off" / "always") so the values bind directly to
  // the `<option>` strings without further mapping.
  type MeetingAutostartMode = "off" | "always";
  let meetingAutostartMode = $state<MeetingAutostartMode>("off");
  let meetingAutostartBusy = $state(false);
  let meetingAutostartError = $state<string | null>(null);

  // ---- About tab --------------------------------------------------------
  // Version pulled from Tauri at runtime so the displayed value
  // tracks `tauri.conf.json` / `Cargo.toml` instead of a hardcoded
  // string that would silently rot. `getName` returns the
  // `productName` field, which is what users see in the menu bar.
  let appVersion = $state<string>("");
  let appName = $state<string>("Hush");
  let tauriVersion = $state<string>("");

  // Manual "Check for updates" probe (#223). Tagged-union result
  // from the Rust side; the dialog renders one of three branches
  // (up to date / update available / failed). `null` means the
  // user hasn't clicked Check yet — common state. Cleared on tab
  // change is overkill; the result is small and harmless to keep.
  type UpdateCheckResult =
    | { kind: "upToDate"; current: string }
    | { kind: "updateAvailable"; current: string; latest: string; releaseUrl: string }
    | { kind: "checkFailed"; reason: string };
  let updateCheck = $state<UpdateCheckResult | null>(null);
  let updateChecking = $state(false);

  // ---- macOS permission diagnostic --------------------------------------
  let macosDiagnostic = $state<MacosPermissionDiagnostic | null>(null);
  let macosDiagnosticOpen = $state(true); // open by default in the dedicated tab
  let macosResetMessage = $state<string | null>(null);
  let macosResetting = $state(false);

  // Error formatting: routed through `lib/errors.ts` (#205) so the
  // main and Settings windows share one source of truth.
  // Rich-shaped state uses `formatErrorDisplay`; the few string-
  // shaped surfaces in this file (`autostartError`,
  // `firstRunResetMessage`, `macosResetMessage`, per-card
  // `downloadFailed`) use `formatErrorMessage`.

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

  /**
   * Track whether a refresh is in flight so the manual Refresh
   * button can show a "Checking…" affordance and disable while
   * the IPC is round-tripping. AVFoundation / CoreGraphics /
   * IOKit reads complete in single-digit milliseconds, but the
   * disabled-flicker is a deliberate hint that the click did
   * something even on a fast machine.
   */
  let macosDiagnosticRefreshing = $state(false);
  async function loadMacosDiagnostic(): Promise<void> {
    macosDiagnosticRefreshing = true;
    try {
      const res = await invoke<MacosPermissionDiagnostic>(
        "diagnose_macos_permissions",
      );
      macosDiagnostic = res.canReset ? res : null;
    } catch {
      macosDiagnostic = null;
    } finally {
      macosDiagnosticRefreshing = false;
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

  async function loadMeetingAutostartMode(): Promise<void> {
    try {
      meetingAutostartMode = await invoke<MeetingAutostartMode>(
        "get_meeting_autostart_mode",
      );
      meetingAutostartError = null;
    } catch (e) {
      meetingAutostartError = "Couldn't read auto-start mode.";
      console.warn("[hush] get_meeting_autostart_mode failed", e);
    }
  }

  async function onMeetingAutostartChange(e: Event) {
    const next = (e.target as HTMLSelectElement).value as MeetingAutostartMode;
    meetingAutostartBusy = true;
    meetingAutostartError = null;
    try {
      await invoke("set_meeting_autostart_mode", { mode: next });
      meetingAutostartMode = next;
    } catch (err) {
      meetingAutostartError = formatErrorMessage(err);
      // Re-read on failure so the dropdown reflects what's
      // actually persisted, not the optimistic value.
      await loadMeetingAutostartMode();
    } finally {
      meetingAutostartBusy = false;
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

  async function openPrivacyPane(
    target: "microphone" | "input-monitoring" | "screen-recording",
  ) {
    try {
      // For Screen Recording: macOS only adds Hush to the Screen
      // & System Audio Recording list once Hush has actively
      // queried SCK. A user who hasn't started a Meeting Mode
      // session yet would land on the pane with no Hush row to
      // toggle. Prime the permission first so the row appears
      // (and the standard TCC prompt fires for not-determined
      // state). Fire-and-forget — we don't block deep-linking on
      // it, and the helper internally swallows the typical
      // "denied" return.
      if (target === "screen-recording") {
        try {
          await invoke("prime_screen_recording_permission");
        } catch (primeErr) {
          console.warn("[hush] prime SCK permission failed", primeErr);
        }
      }
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
      macosResetMessage = formatErrorMessage(e);
    } finally {
      macosResetting = false;
    }
  }

  // ---- Lifecycle ---------------------------------------------------------

  onMount(async () => {
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
    // Menu-driven Check for Updates lands here as an event
    // (#265) — the macOS menu fires the probe directly and emits
    // the result to all windows. Stash it in the same state
    // the in-tab button uses so the About tab renders the
    // outcome whether the user clicked the menu or the button.
    unlistenUpdaterResult = await listen<UpdateCheckResult>(
      Events.UpdaterResult,
      (e) => {
        updateCheck = e.payload;
        updateChecking = false;
      },
    );

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

    await Promise.all([
      loadModels(),
      loadVocabulary(),
      loadReplacements(),
      loadMacosDiagnostic(),
      loadAutostartState(),
      loadHudEnabled(),
      loadAppMetadata(),
      loadAppOverrides(),
      loadMeetingAutostartMode(),
    ]);

    // Auto-refresh the permissions diagnostic when the Settings
    // window regains focus. The "Grant in Settings…" button
    // deep-links the user out to System Settings; while they're
    // there they may toggle a permission on or off, but the
    // diagnostic was loaded once on mount and won't notice
    // unless we re-poll. Window-focus is the natural trigger:
    // the user has come back to look at Hush, so it's the right
    // moment to re-check. Cheap (single-digit ms) so re-running
    // on every focus is fine.
    //
    // Only fires on the macOS-capable path (`macosDiagnostic`
    // is the gate) — non-macOS builds skip the IPC entirely.
    function handleSettingsFocus() {
      if (macosDiagnostic !== null && !macosDiagnosticRefreshing) {
        void loadMacosDiagnostic();
      }
    }
    window.addEventListener("focus", handleSettingsFocus);
    settingsFocusHandler = handleSettingsFocus;
  });

  // Run the manual update probe. The backend returns a tagged
  // union; we just stash it and let the markup pick the branch.
  // Idempotent — repeated clicks just re-fetch.
  async function onCheckForUpdates() {
    updateChecking = true;
    updateCheck = null;
    try {
      updateCheck = await invoke<UpdateCheckResult>("check_for_updates");
    } catch (e) {
      // The Rust side already maps transport errors to a
      // `checkFailed` variant; an exception here means the IPC
      // itself blew up (e.g. the command isn't registered in a
      // stale build). Surface a generic failure rather than
      // dropping silently.
      updateCheck = {
        kind: "checkFailed",
        reason: formatErrorMessage(e),
      };
    } finally {
      updateChecking = false;
    }
  }

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
      autostartError = formatErrorMessage(err);
      // Re-read so the checkbox reverts to truth rather than the
      // optimistic state that didn't persist.
      await loadAutostartState();
    } finally {
      autostartBusy = false;
    }
  }

  async function loadHudEnabled(): Promise<void> {
    try {
      hudEnabled = await invoke<boolean>("get_hud_enabled");
      hudError = null;
    } catch (e) {
      hudError = "Couldn't read HUD setting.";
      console.warn("[hush] get_hud_enabled failed", e);
    }
  }

  async function onHudToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    hudBusy = true;
    hudError = null;
    try {
      await invoke("set_hud_enabled", { enabled: checked });
      hudEnabled = checked;
    } catch (err) {
      hudError = formatErrorMessage(err);
      // Re-read on failure so the checkbox reflects the persisted
      // value rather than the optimistic state.
      await loadHudEnabled();
    } finally {
      hudBusy = false;
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
      firstRunResetMessage = formatErrorMessage(e);
    } finally {
      firstRunResetBusy = false;
    }
  }

  onDestroy(() => {
    unlistenDownloadProgress?.();
    unlistenDownloadDone?.();
    unlistenDownloadFailed?.();
    unlistenGotoTab?.();
    unlistenUpdaterResult?.();
    if (settingsFocusHandler) {
      window.removeEventListener("focus", settingsFocusHandler);
      settingsFocusHandler = null;
    }
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
      <h2 class="tab-title">General</h2>

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

      <section class="settings-group" aria-labelledby="settings-interface-heading">
        <h2 id="settings-interface-heading" class="group-heading">Interface</h2>
        <label class="toggle-row">
          <input
            type="checkbox"
            data-testid="settings-hud-toggle"
            disabled={hudBusy}
            checked={hudEnabled}
            onchange={onHudToggle}
          />
          <span class="toggle-label">
            <span class="toggle-name">Show recording HUD</span>
            <span class="toggle-desc">
              The floating pill that appears in the top-right corner
              while Hush is capturing audio. Off hides it for both
              dictation and meeting mode; recording itself is
              unaffected.
            </span>
          </span>
        </label>
        {#if hudError}
          <p class="settings-error">{hudError}</p>
        {/if}
      </section>

      <section class="settings-group" aria-labelledby="settings-hotkeys-heading">
        <h2 id="settings-hotkeys-heading" class="group-heading">Hotkeys</h2>
        <p class="settings-row">
          <span class="row-label">Toggle recording</span>
          <span class="row-value">
            <span class="chord"><kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd></span>
            <span class="row-note">Not currently editable — the push-to-talk combo below is.</span>
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
      <h2 class="tab-title">Meeting</h2>

      <section class="settings-group" aria-labelledby="settings-autostart-heading">
        <h2 id="settings-autostart-heading" class="group-heading">Auto-start</h2>
        <div class="select-row">
          <label class="select-label" for="settings-meeting-autostart">
            <span class="select-name">When a meeting app focuses</span>
            <span class="select-desc">
              Off keeps every meeting manual. Always opens a
              Meeting Mode session whenever a known meeting app
              (Zoom, Teams, Discord, …) comes to the foreground.
              Sessions stop manually either way.
            </span>
          </label>
          <select
            id="settings-meeting-autostart"
            data-testid="settings-meeting-autostart"
            disabled={meetingAutostartBusy}
            value={meetingAutostartMode}
            onchange={onMeetingAutostartChange}
          >
            <option value="off">Off — start manually</option>
            <option value="always">Always start a session</option>
          </select>
        </div>
        {#if meetingAutostartError}
          <p class="settings-error">{meetingAutostartError}</p>
        {/if}
      </section>

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
        <div class="permissions-tab-header">
          <h2 class="tab-title">Permissions</h2>
          <!--
            Manual refresh button — belt-and-suspenders for the
            window-focus auto-refresh wired in onMount. The
            auto-refresh covers the common case (user toggles a
            permission in System Settings, switches back to Hush);
            the button covers the corner cases where focus didn't
            change (Settings + System Settings side-by-side,
            keyboard-only navigation, screen reader users) and
            gives the user a deliberate "re-check now" affordance
            for when they're not sure if the auto-refresh fired.
          -->
          <button
            type="button"
            class="ghost"
            onclick={() => void loadMacosDiagnostic()}
            disabled={macosDiagnosticRefreshing}
            aria-label="Re-check macOS permission status"
            data-testid="perms-refresh"
          >
            {macosDiagnosticRefreshing ? "Checking…" : "Refresh"}
          </button>
        </div>
        <ul class="perm-status-list" aria-label="Permission status summary">
          {#each [
            { key: "microphone", paneTarget: "microphone" as const, label: "Microphone", status: macosDiagnostic.statuses.microphone, why: "Required for dictation." },
            { key: "screenRecording", paneTarget: "screen-recording" as const, label: "Screen Recording", status: macosDiagnostic.statuses.screenRecording, why: "Required for system-audio capture in meetings." },
            { key: "inputMonitoring", paneTarget: "input-monitoring" as const, label: "Input Monitoring", status: macosDiagnostic.statuses.inputMonitoring, why: "Required for push-to-talk (on by default). Disable PTT in General → Hotkeys if you'd rather skip the prompt." },
          ] as row (row.key)}
            <li class="perm-row" data-perm={row.key} data-status={row.status}>
              <!--
                Two-column layout: text block on the left
                (title-line + why subtitle), action button on the
                right. Replaces the previous 4-column grid where
                the status label drifted horizontally between rows
                and competed with the action button for the right
                edge of the row. The status now lives as a
                coloured pill inline with the title — one signal
                instead of dot + uppercase label, anchored to the
                row's content rather than floating mid-row. Mirrors
                System Settings → Privacy & Security's visual
                idiom (status next to the name; controls flush
                right).
              -->
              <div class="perm-text">
                <div class="perm-title-line">
                  <span class="perm-name">{row.label}</span>
                  <span class="perm-status-pill">
                    {#if row.status === "granted"}Granted
                    {:else if row.status === "denied"}Denied
                    {:else if row.status === "not-determined"}Not yet granted
                    {:else}Not applicable
                    {/if}
                  </span>
                </div>
                <span class="perm-why">{row.why}</span>
              </div>
              <!--
                Per-row deep-link to the relevant System Settings
                pane. Renders for every row, not just unblocked
                ones, because granted rows still need a way to
                revoke / re-confirm. Copy varies with status so
                the click target reads as the right next step
                ("Grant in Settings…" vs "Open in Settings").
              -->
              {#if row.status !== "not-applicable"}
                <button
                  type="button"
                  class="perm-row-action"
                  data-testid="perm-action-{row.key}"
                  onclick={() => openPrivacyPane(row.paneTarget)}
                >
                  {#if row.status === "granted"}
                    Open in Settings
                  {:else}
                    Grant in Settings…
                  {/if}
                </button>
              {/if}
            </li>
          {/each}
        </ul>
        <p class="perm-recovery-intro">
          Stuck? Open the diagnostic below to reset all four
          permission grants (Microphone, Screen Recording, Input
          Monitoring, Accessibility) at once, or learn why a
          permission row might not appear in System Settings.
        </p>
        <MacosDiagnosticPanel
          {macosDiagnostic}
          bind:macosDiagnosticOpen
          {macosResetMessage}
          {macosResetting}
          onReset={runMacosReset}
        />
      {:else}
        <h2 class="tab-title">Permissions</h2>
        <p class="placeholder">
          Permission diagnostics are macOS-only. There's nothing
          actionable to surface on this platform.
        </p>
      {/if}
    {:else if active === "about"}
      <h2 class="tab-title">About</h2>
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

        <!--
          Manual "Check for updates" probe (#223). Sits below the
          version line so the comparison is contextual: user sees
          their version, clicks Check, gets a result inline.
          Auto-update via tauri-plugin-updater is the heavier
          follow-up — see #10.
        -->
        <div class="about-updates">
          <button
            type="button"
            class="ghost"
            data-testid="settings-check-updates"
            disabled={updateChecking}
            onclick={onCheckForUpdates}
          >
            {updateChecking ? "Checking…" : "Check for updates"}
          </button>
          {#if updateCheck}
            {#if updateCheck.kind === "upToDate"}
              <p class="about-update-result about-update-ok" role="status">
                You're on {updateCheck.current} — that's the
                latest.
              </p>
            {:else if updateCheck.kind === "updateAvailable"}
              <p class="about-update-result about-update-available" role="status">
                <strong>Update available:</strong>
                {updateCheck.latest} (you're on
                {updateCheck.current}).
                <a
                  href={updateCheck.releaseUrl}
                  target="_blank"
                  rel="noopener noreferrer"
                >Open release notes</a>.
              </p>
            {:else if updateCheck.kind === "checkFailed"}
              <p class="about-update-result about-update-failed" role="status">
                {updateCheck.reason}
              </p>
            {/if}
          {/if}
        </div>

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

  .tab-title {
    margin: 0 0 0.75rem;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }

  /* Permissions-tab header: aligns the title with a Refresh
     button on the right, so the user can re-check the diagnostic
     without leaving the tab. The auto-refresh on window focus
     covers the common case; this button covers the edge cases
     (side-by-side windows, keyboard-only nav). */
  .permissions-tab-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .permissions-tab-header .tab-title {
    margin: 0;
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

  .about-updates {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    margin: 0 0 1.25rem;
  }
  .about-updates button {
    align-self: flex-start;
  }
  .about-update-result {
    margin: 0;
    padding: 0.55rem 0.75rem;
    border-radius: 6px;
    font-size: 0.9rem;
    line-height: 1.4;
  }
  .about-update-ok {
    background-color: #e7f8ec;
    border: 1px solid #b6e5c5;
    color: #2a6b3c;
  }
  .about-update-available {
    background-color: #eef2ff;
    border: 1px solid #c7d2fe;
    color: #1e1b4b;
  }
  .about-update-failed {
    background-color: #fff7e6;
    border: 1px solid #ffd591;
    color: #8a5a00;
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
    color: var(--accent-hover);
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
      color: var(--accent);
    }
    .about-update-ok {
      background-color: rgba(46, 170, 83, 0.15);
      border-color: #2a6b3c;
      color: #b6e5c5;
    }
    .about-update-available {
      background-color: rgba(106, 140, 240, 0.15);
      border-color: #3a4a7a;
      color: #d8e0ff;
    }
    .about-update-failed {
      background-color: rgba(255, 193, 7, 0.12);
      border-color: #6b5300;
      color: #ffd591;
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

  /* Select-shaped settings row — same bordered-card pattern as
     `.toggle-row` so the visual rhythm across General, Interface,
     and Meeting auto-start stays consistent. Label + description
     above, dropdown right-aligned. */
  .select-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .select-label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    flex: 1;
    min-width: 0;
  }
  .select-name {
    font-weight: 600;
    color: #222;
  }
  .select-desc {
    font-size: 0.82rem;
    color: #666;
    line-height: 1.4;
  }
  .select-row select {
    flex-shrink: 0;
    align-self: flex-start;
    padding: 0.35rem 0.55rem;
    font-size: 0.85rem;
    font-family: inherit;
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
    grid-template-columns: 1fr auto;
    gap: 0.6rem 1rem;
    align-items: center;
    padding: 0.7rem 0.9rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .perm-text {
    /* min-width:0 lets the text column shrink under flex/grid
       constraints so a long "why" wraps instead of pushing the
       button off the row. */
    min-width: 0;
  }
  .perm-title-line {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .perm-name {
    font-weight: 600;
    color: #222;
  }
  /* Status pill — replaces the floating uppercase label AND the
     dot. Background colour carries the state signal that the dot
     used to carry (one element instead of two), and the inline
     position next to the title anchors the status to its row's
     content rather than letting it float in column 3. Mirrors
     System Settings → Privacy & Security where state lives next
     to the name and the right edge is reserved for controls. */
  .perm-status-pill {
    font-size: 0.72rem;
    font-weight: 600;
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    background: #ececf0;
    color: #555;
    line-height: 1.4;
    white-space: nowrap;
  }
  .perm-row[data-status="granted"] .perm-status-pill {
    background: #e3f5e8;
    color: #1f6b35;
  }
  .perm-row[data-status="not-determined"] .perm-status-pill {
    background: #fdf1d8;
    color: #7a4e00;
  }
  .perm-row[data-status="denied"] .perm-status-pill {
    background: #fbe3e3;
    color: #8a1f1f;
  }
  .perm-why {
    display: block;
    margin-top: 0.15rem;
    font-size: 0.82rem;
    color: #666;
  }
  /* Per-row "Grant in Settings…" button. Lives in the second
     grid column, vertically centred against the text-block on
     the left so the click target is balanced against the
     name+why stack. */
  .perm-row-action {
    align-self: center;
    padding: 0.35rem 0.7rem;
    font-size: 0.82rem;
    font-weight: 500;
    border: 1px solid #d1d1d8;
    background-color: white;
    border-radius: 6px;
    cursor: pointer;
    color: #2c3e8f;
    white-space: nowrap;
    transition: background-color 0.12s, border-color 0.12s;
  }
  .perm-row-action:hover {
    background-color: #f0f4ff;
    border-color: #4a6cd0;
  }
  .perm-row-action:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  /* On not-yet-granted rows the button is the primary path forward;
     give it a hint of weight so the user reads it as the actionable
     element. Granted rows render the same button in the quieter
     variant above. */
  .perm-row[data-status="not-determined"] .perm-row-action,
  .perm-row[data-status="denied"] .perm-row-action {
    background-color: #eef2ff;
    border-color: #c7d2fe;
    color: #1e1b4b;
    font-weight: 600;
  }
  .perm-row[data-status="not-determined"] .perm-row-action:hover,
  .perm-row[data-status="denied"] .perm-row-action:hover {
    background-color: #e0e7ff;
    border-color: var(--accent);
  }
  .perm-recovery-intro {
    margin: 0 0 1rem;
    font-size: 0.85rem;
    color: #555;
    max-width: 44rem;
  }
  @media (prefers-color-scheme: dark) {
    .perm-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .perm-name { color: #e8e8e8; }
    .perm-why { color: #a8a8a8; }
    .perm-status-pill {
      background: #3a3a3f;
      color: #c8c8cc;
    }
    .perm-row[data-status="granted"] .perm-status-pill {
      background: #1d3a26;
      color: #8fd9a3;
    }
    .perm-row[data-status="not-determined"] .perm-status-pill {
      background: #3d2f12;
      color: #f0c878;
    }
    .perm-row[data-status="denied"] .perm-status-pill {
      background: #3d1d1d;
      color: #f0a0a0;
    }
    .perm-recovery-intro { color: #b0b0b0; }
    .perm-row-action {
      background-color: #1f1f22;
      border-color: #38383b;
      color: #c0d0ff;
    }
    .perm-row-action:hover {
      background-color: #28283a;
      border-color: var(--accent);
    }
    .perm-row[data-status="not-determined"] .perm-row-action,
    .perm-row[data-status="denied"] .perm-row-action {
      background-color: #1e1b4b;
      border-color: #4338ca;
      color: #e0e7ff;
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
    .placeholder { color: #a8a8a8; }
    .toggle-row,
    .select-row,
    .settings-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .toggle-name,
    .select-name { color: #e8e8e8; }
    .toggle-desc,
    .select-desc { color: #a8a8a8; }
    .select-row select {
      background-color: #1f1f22;
      color: #e8e8e8;
      border-color: #38383b;
    }
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
