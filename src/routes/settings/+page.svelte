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
  import { platform } from "@tauri-apps/plugin-os";
  import {
    disable as disableAutostart,
    enable as enableAutostart,
    isEnabled as isAutostartEnabled,
  } from "@tauri-apps/plugin-autostart";
  import { onDestroy, onMount, tick } from "svelte";

  import { openExternal } from "$lib/openExternal";
  import MeetingAppOverridesPanel from "$lib/MeetingAppOverridesPanel.svelte";
  import PermissionsTab from "$lib/PermissionsTab.svelte";
  import ModelPickerPanel from "$lib/ModelPickerPanel.svelte";
  import PttHotkeyEditor from "$lib/PttHotkeyEditor.svelte";
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
    DiarizerModelStatus,
    DownloadProgress,
    IpcError,
    BuiltinAppEntry,
    MeetingAppKind,
    MeetingAppOverride,
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
  let isMacOS = $state(false);

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
  /* (Permissions-tab window-focus listener handle moved to
     PermissionsTab.svelte in #332 phase 1.) */

  // ---- General-tab state ------------------------------------------------
  // Autostart toggle: queried on mount via the autostart plugin and
  // mirrored locally so the checkbox can show optimistic UI while the
  // plugin call is in flight.
  let autostartEnabled = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state<string | null>(null);
  // LaunchAgent path-staleness flag (#317). Read on mount; surfaces
  // as a warning row when the boot-time re-register failed. Cleared
  // by a successful retry.
  let autostartPathStale = $state(false);
  let autostartRetryBusy = $state(false);
  let autostartRetryFailed = $state(false);
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

  // Audio cues (#292). Default off — opt-in only. Same load /
  // optimistic-update / error-snap-back shape as hudEnabled.
  let soundCuesEnabled = $state(false);
  let soundCuesBusy = $state(false);
  let soundCuesError = $state<string | null>(null);

  // Transcription threads (#255). Backend clamps to [1, 16].
  // Bigger model + slower CPU benefits from more threads;
  // background-friendly setups want fewer. Persists across launches
  // and is read on every inference call via a shared atomic, so a
  // slider change takes effect on the next chunk without restart.
  //
  // Two state cells (#348): `inferenceThreads` is the persisted
  // value (source of truth from the backend); `inferenceThreadsDisplay`
  // tracks the slider thumb live during drag so the inline label
  // updates in real time without firing one IPC per pixel. The
  // change-event (release) is what actually persists.
  let inferenceThreads = $state(4);
  let inferenceThreadsDisplay = $state(4);
  let inferenceThreadsBusy = $state(false);
  let inferenceThreadsError = $state<string | null>(null);

  // Diarization (#111). Default off — opt-in. When the toggle is
  // on AND the wespeaker .onnx model is present in the models
  // directory, the meeting pump labels utterances per-speaker
  // (Speaker 1, 2, …) instead of the source-derived "You" /
  // "Remote" tags. The toggle persists; runtime behaviour gates
  // on `FlagGatedDiarizer` reading the same atomic shared with
  // AppState.
  let diarizationEnabled = $state(false);
  let diarizationBusy = $state(false);
  let diarizationError = $state<string | null>(null);

  // Diarizer model status (#301). When the wespeaker .onnx is
  // missing, the toggle is informational only — the runtime falls
  // back to source-only labels. Settings → Speakers reads this on
  // mount + after each download lifecycle event so the UI can
  // render "model not installed", "downloading", or "ready". The
  // type lives in `$lib/types` per the four-place IPC sync rule.
  let diarizerModelStatus = $state<DiarizerModelStatus | null>(null);
  let diarizerDownloadBusy = $state(false);
  let diarizerDownloadProgress = $state<{ received: number; total: number | null } | null>(null);
  let diarizerDownloadError = $state<string | null>(null);
  let unlistenDiarizerProgress: (() => void) | null = null;
  let unlistenDiarizerDone: (() => void) | null = null;
  let unlistenDiarizerFailed: (() => void) | null = null;

  // Remove-model affordance (#351). Two-state click-to-confirm
  // pattern matching `clearConfirming` over in History — first
  // click reveals the danger-styled confirm button, second click
  // fires. No timeout reset here because the dialog is small and
  // the user has explicitly opened the details panel; a stale arm
  // is unlikely.
  let diarizerRemoveConfirming = $state(false);
  let diarizerRemoveBusy = $state(false);
  let diarizerRemoveError = $state<string | null>(null);

  /* (Vocabulary state + handlers moved to VocabularyTab.svelte
     in #332 phase 1.) */

  /* (Replacements state + handlers moved to ReplacementsTab.svelte
     in #332 phase 1.) */

  // ---- Meeting app classification overrides (Phase E, #112) -------------
  let appOverrides = $state<MeetingAppOverride[]>([]);
  let appOverridesLoaded = $state(false);
  let appOverridesError = $state<ErrorDisplay | null>(null);
  let newOverrideName = $state("");
  let newOverrideKind = $state<MeetingAppKind>("meeting");
  let overrideInputEl = $state<HTMLInputElement | null>(null);
  // Built-in classification table (#320). Loaded once on mount; the
  // panel renders these in a read-only disclosure so users can see
  // what's already covered before adding a redundant override.
  let appDefaults = $state<BuiltinAppEntry[]>([]);

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

  async function loadAppDefaults(): Promise<void> {
    // The built-in table is stable per build; we read it once on
    // mount + cache. Failure here is non-fatal — the disclosure
    // just stays empty; the user-overrides UI still works.
    try {
      appDefaults = await invoke<BuiltinAppEntry[]>(
        "meeting_app_classifier_defaults",
      );
    } catch (e) {
      console.warn("[hush] meeting_app_classifier_defaults failed", e);
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

  /// Batch add for the variant-suggestion box (#320 part 2). The
  /// user picks N defaults from the suggestion list; we run an
  /// upsert per name in parallel + merge each result into the
  /// override list. Errors short-circuit at the first failure
  /// (the others may have already landed; the panel's full-list
  /// reload would catch any drift, but an explicit refresh keeps
  /// state simple).
  async function addAppOverrideVariants(
    appNames: string[],
    kind: MeetingAppKind,
  ) {
    if (appNames.length === 0) return;
    try {
      const created = await Promise.all(
        appNames.map((appName) =>
          invoke<MeetingAppOverride>("meeting_app_override_upsert", {
            appName,
            kind,
          }),
        ),
      );
      // Merge upserts into the existing list — replace any rows
      // with matching appName, then sort.
      const createdNames = new Set(created.map((o) => o.appName));
      appOverrides = [
        ...appOverrides.filter((o) => !createdNames.has(o.appName)),
        ...created,
      ].sort((a, b) => a.appName.localeCompare(b.appName));
      newOverrideName = "";
      newOverrideKind = "meeting";
      appOverridesError = null;
      await tick();
      overrideInputEl?.focus();
    } catch (err) {
      // Any partial successes already landed; reload to get a
      // consistent view rather than leaving the UI in a guessed
      // state.
      await loadAppOverrides();
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
    // Platform glyph (#272). Resolves via `plugin-os`; failure
    // leaves the default `false` (Right Ctrl glyph in the PTT
    // hint), same fallback `navigator.platform` would have given.
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() probe failed; defaulting to non-macOS glyph", e);
    }

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
    //
    // Gate on `!updateChecking` (review #4 UX-1): if the user
    // clicked the in-tab button and *also* clicked the menu mid-
    // probe, the broadcast event would clobber `updateChecking`
    // and double-mutate `updateCheck`, causing a screen-reader
    // double-announce on the `role="status"` paragraph and a
    // potential UI race when the in-flight invoke returns. The
    // locally-issued probe is the source of truth; menu events
    // fired *outside* an active local probe still land
    // (the common case is "I clicked the menu and nothing else").
    unlistenUpdaterResult = await listen<UpdateCheckResult>(
      Events.UpdaterResult,
      (e) => {
        if (updateChecking) {
          return;
        }
        updateCheck = e.payload;
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
      loadAutostartState(),
      loadAutostartPathStatus(),
      loadHudEnabled(),
      loadSoundCuesEnabled(),
      loadInferenceThreads(),
      loadAppMetadata(),
      loadAppOverrides(),
      loadAppDefaults(),
      loadMeetingAutostartMode(),
      loadDiarizationEnabled(),
      loadDiarizerModelStatus(),
    ]);

    // Wire up diarizer-download lifecycle listeners (#301). The
    // backend reuses the existing `model:` events the Whisper
    // download path emits, but we filter by `id` so the diarizer
    // download doesn't get confused with a Whisper download in
    // flight at the same time.
    const isDiarizerEvent = (id: string) => id === "wespeaker-resnet34-lm";
    unlistenDiarizerProgress = await listen<DownloadProgressEvent>(
      "model:download-progress",
      (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadProgress = {
          received: event.payload.bytesReceived,
          total: event.payload.bytesTotal,
        };
      },
    );
    unlistenDiarizerDone = await listen<{ id: string }>(
      "model:download-done",
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadBusy = false;
        diarizerDownloadProgress = null;
        diarizerDownloadError = null;
        await loadDiarizerModelStatus();
      },
    );
    unlistenDiarizerFailed = await listen<{ id: string; message: string | null }>(
      "model:download-failed",
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadBusy = false;
        diarizerDownloadProgress = null;
        diarizerDownloadError = event.payload.message ?? "Download failed.";
        await loadDiarizerModelStatus();
      },
    );

    /* Permissions tab's window-focus auto-refresh moved to
       PermissionsTab.svelte in #332 phase 1 — its lifecycle
       hooks own the listener now. */
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

  async function loadAutostartPathStatus(): Promise<void> {
    try {
      const status = await invoke<{ stale: boolean }>(
        "get_autostart_path_status",
      );
      autostartPathStale = status.stale;
    } catch (e) {
      // Failure is non-fatal — the warning just doesn't render.
      console.warn("[hush] get_autostart_path_status failed", e);
      autostartPathStale = false;
    }
  }

  async function onRetryAutostartRegistration() {
    if (autostartRetryBusy) return;
    autostartRetryBusy = true;
    autostartRetryFailed = false;
    try {
      const ok = await invoke<boolean>("retry_autostart_registration");
      if (ok) {
        autostartPathStale = false;
      } else {
        autostartRetryFailed = true;
      }
    } catch (e) {
      autostartRetryFailed = true;
      console.warn("[hush] retry_autostart_registration failed", e);
    } finally {
      autostartRetryBusy = false;
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

  async function loadSoundCuesEnabled(): Promise<void> {
    try {
      soundCuesEnabled = await invoke<boolean>("get_sound_cues_enabled");
      soundCuesError = null;
    } catch (e) {
      soundCuesError = "Couldn't read audio-cues setting.";
      console.warn("[hush] get_sound_cues_enabled failed", e);
    }
  }

  async function onSoundCuesToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    soundCuesBusy = true;
    soundCuesError = null;
    try {
      await invoke("set_sound_cues_enabled", { enabled: checked });
      soundCuesEnabled = checked;
    } catch (err) {
      soundCuesError = formatErrorMessage(err);
      await loadSoundCuesEnabled();
    } finally {
      soundCuesBusy = false;
    }
  }

  async function loadInferenceThreads(): Promise<void> {
    try {
      inferenceThreads = await invoke<number>("get_inference_threads");
      inferenceThreadsDisplay = inferenceThreads;
      inferenceThreadsError = null;
    } catch (e) {
      inferenceThreadsError = "Couldn't read inference-threads setting.";
      console.warn("[hush] get_inference_threads failed", e);
    }
  }

  /// Live drag handler. Only updates the visible label so the user
  /// sees the slider thumb's position in real time without firing
  /// one IPC per pixel of movement. The `change` event below fires
  /// on release and is what actually persists. (#348 follow-up)
  function onInferenceThreadsInput(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(next)) {
      inferenceThreadsDisplay = next;
    }
  }

  async function onInferenceThreadsChange(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (!Number.isFinite(next)) {
      return;
    }
    inferenceThreadsBusy = true;
    inferenceThreadsError = null;
    try {
      await invoke("set_inference_threads", { threads: next });
      inferenceThreads = next;
      inferenceThreadsDisplay = next;
    } catch (err) {
      inferenceThreadsError = formatErrorMessage(err);
      // Snap the display back to the persisted value so the user
      // can see their drag didn't take. `loadInferenceThreads`
      // syncs both `inferenceThreads` and the display.
      await loadInferenceThreads();
    } finally {
      inferenceThreadsBusy = false;
    }
  }

  async function loadDiarizationEnabled(): Promise<void> {
    // Refresh-only path: re-read the persisted value, but don't
    // touch `diarizationError` if it's already non-null. The
    // setter-failure path needs the error to survive the
    // post-failure refresh; clobbering it on a successful read
    // hid the error from users (caught by #302 e2e).
    try {
      diarizationEnabled = await invoke<boolean>("get_diarization_enabled");
    } catch (e) {
      diarizationError = "Couldn't read diarization setting.";
      console.warn("[hush] get_diarization_enabled failed", e);
    }
  }

  async function onDiarizationToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    diarizationBusy = true;
    diarizationError = null;
    try {
      await invoke("set_diarization_enabled", { enabled: checked });
      diarizationEnabled = checked;
    } catch (err) {
      diarizationError = formatErrorMessage(err);
      // Re-read the persisted value (likely false) without
      // clobbering the error message we just set.
      await loadDiarizationEnabled();
    } finally {
      diarizationBusy = false;
    }
  }

  async function loadDiarizerModelStatus(): Promise<void> {
    try {
      diarizerModelStatus = await invoke<DiarizerModelStatus>(
        "get_diarizer_model_status",
      );
    } catch (e) {
      console.warn("[hush] get_diarizer_model_status failed", e);
      diarizerModelStatus = null;
    }
  }

  async function onDiarizerDownload() {
    if (diarizerDownloadBusy) return;
    diarizerDownloadBusy = true;
    diarizerDownloadProgress = null;
    diarizerDownloadError = null;
    try {
      await invoke("download_diarizer_model");
      // The actual completion is signalled via the
      // `model:download-done` listener — that handler clears
      // diarizerDownloadBusy + refreshes the status.
    } catch (err) {
      diarizerDownloadBusy = false;
      diarizerDownloadError = formatErrorMessage(err);
    }
  }

  async function onDiarizerCancel() {
    // Reuses the existing `model_cancel_download` IPC keyed by id;
    // `AppState::downloads` is shared between the Whisper picker
    // and the diarizer downloader, so the same cancel path works.
    // The download task notices the flag on its next chunk
    // boundary and exits via `model:download-failed` (an empty-
    // looking failure message is the convention; the Whisper
    // picker treats it the same way).
    try {
      await invoke("model_cancel_download", { id: "wespeaker-resnet34-lm" });
    } catch (err) {
      // Cancel itself failing is exotic — just surface for debugging.
      console.warn("[hush] model_cancel_download failed", err);
    }
  }

  async function onDiarizerRemoveConfirm() {
    if (diarizerRemoveBusy) return;
    diarizerRemoveBusy = true;
    diarizerRemoveError = null;
    try {
      await invoke("remove_diarizer_model");
      // Reset the local toggle state in lockstep with the
      // backend's `diarization_enabled` flip — the Speakers
      // toggle's `checked` prop reads from `diarizationEnabled`,
      // so the next render shows it off.
      diarizationEnabled = false;
      // Refresh the model status so the UI flips back to the
      // "not installed" branch, exposing the Download button.
      await loadDiarizerModelStatus();
      diarizerRemoveConfirming = false;
    } catch (err) {
      diarizerRemoveError = formatErrorMessage(err);
    } finally {
      diarizerRemoveBusy = false;
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
    unlistenDiarizerProgress?.();
    unlistenDiarizerDone?.();
    unlistenDiarizerFailed?.();
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

        {#if autostartPathStale}
          <!--
            Stale-LaunchAgent warning (#317). The setup hook re-
            registers the plist on every launch; if that re-register
            failed (read-only home, fs permission), the LaunchAgent
            still points at whatever path it had before. Surface the
            failure with a retry button so the user isn't left with
            a silent broken autostart.
          -->
          <div
            class="settings-warning-row"
            data-testid="autostart-path-stale-warning"
            role="alert"
          >
            <p class="settings-row-name">⚠ Autostart path is out of date</p>
            <p class="settings-row-desc">
              Hush couldn't refresh the LaunchAgent at startup, so
              "Launch at Login" may not work after the next restart.
              Click below to retry — usually a one-click fix.
            </p>
            <button
              type="button"
              class="ghost"
              data-testid="autostart-retry-button"
              disabled={autostartRetryBusy}
              onclick={onRetryAutostartRegistration}
            >
              {autostartRetryBusy ? "Retrying…" : "Click to update"}
            </button>
            {#if autostartRetryFailed}
              <p
                class="settings-error"
                data-testid="autostart-retry-error"
              >
                Retry failed too. Check that <code
                  >~/Library/LaunchAgents/</code
                > is writable, then try again.
              </p>
            {/if}
          </div>
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

        <!--
          Audio cues toggle (#292). Sits in the Interface group
          alongside the HUD toggle since both are sensory-feedback
          settings the user calibrates to their environment. Off by
          default — opt-in deliberately because cues are intrusive
          in shared spaces / meeting rooms / focus modes.
        -->
        <label class="toggle-row">
          <input
            type="checkbox"
            data-testid="settings-sound-cues-toggle"
            disabled={soundCuesBusy}
            checked={soundCuesEnabled}
            onchange={onSoundCuesToggle}
          />
          <span class="toggle-label">
            <span class="toggle-name">Audio cues</span>
            <span class="toggle-desc">
              Plays a short macOS system sound when recording
              starts (Tink) and when transcription completes
              (Glass — "safe to paste"). Honours your system
              volume and Do Not Disturb. Off keeps Hush silent.
            </span>
          </span>
        </label>
        {#if soundCuesError}
          <p class="settings-error">{soundCuesError}</p>
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

      <section class="settings-group" aria-labelledby="settings-performance-heading">
        <h2 id="settings-performance-heading" class="group-heading">Performance</h2>
        <label class="slider-row">
          <span class="toggle-label">
            <span class="toggle-name">
              Transcription threads:
              <span
                data-testid="settings-inference-threads-value"
                aria-live="polite"
              >{inferenceThreadsDisplay}</span>
              {#if inferenceThreadsBusy}
                <span class="row-note" aria-live="polite">Saving…</span>
              {/if}
            </span>
            <span id="settings-inference-threads-desc" class="toggle-desc">
              How many CPU threads whisper.cpp uses per chunk. More
              threads finish each chunk faster on a multi-core CPU but
              compete with other apps for cores. The default (4) suits
              most laptops; bump it up if transcription lags on a
              larger model, drop it if you want Hush to run quietly
              alongside heavy workloads.
            </span>
          </span>
          <input
            type="range"
            min="1"
            max="16"
            step="1"
            data-testid="settings-inference-threads-slider"
            aria-label="Transcription threads"
            aria-describedby="settings-inference-threads-desc"
            aria-valuetext={`${inferenceThreadsDisplay} threads`}
            disabled={inferenceThreadsBusy}
            value={inferenceThreadsDisplay}
            oninput={onInferenceThreadsInput}
            onchange={onInferenceThreadsChange}
          />
        </label>
        {#if inferenceThreadsError}
          <p class="settings-error">{inferenceThreadsError}</p>
        {/if}
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
      <VocabularyTab />
    {:else if active === "replacements"}
      <ReplacementsTab />
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

      <!--
        Diarization toggle + model status (#111, #301). When the
        wespeaker model is present AND the toggle is on, the
        meeting pump routes utterances through OnnxDiarizer; if
        the model is missing the toggle is informational only
        (FlagGatedDiarizer's inner is NoopDiarizer until the
        download lands), so the download affordance appears
        before the toggle.
      -->
      <section class="settings-group" aria-labelledby="settings-diarization-heading">
        <h2 id="settings-diarization-heading" class="group-heading">Speakers</h2>

        {#if diarizerModelStatus && !diarizerModelStatus.downloaded}
          <div class="diarizer-model-status" data-testid="diarizer-model-not-installed">
            <p class="settings-row-name">Speaker model not installed</p>
            <p class="settings-row-desc">
              Per-speaker labels need a {diarizerModelStatus.sizeMb} MB ONNX
              model. Hush downloads it once and verifies the
              SHA-256; the toggle below has no effect until this
              completes.
            </p>
            <div class="diarizer-download-row">
              <button
                type="button"
                class="diarizer-download-button"
                data-testid="diarizer-download-button"
                disabled={diarizerDownloadBusy}
                onclick={onDiarizerDownload}
              >
                {#if diarizerDownloadBusy}
                  {#if diarizerDownloadProgress?.total}
                    Downloading… {Math.round(
                      (100 * diarizerDownloadProgress.received) /
                        diarizerDownloadProgress.total,
                    )}%
                  {:else}
                    Downloading…
                  {/if}
                {:else}
                  Download speaker model ({diarizerModelStatus.sizeMb} MB)
                {/if}
              </button>
              {#if diarizerDownloadBusy}
                <button
                  type="button"
                  class="ghost danger"
                  data-testid="diarizer-cancel-button"
                  onclick={onDiarizerCancel}
                >
                  Cancel
                </button>
              {/if}
            </div>
            {#if diarizerDownloadError}
              <p class="settings-error" data-testid="diarizer-download-error">
                {diarizerDownloadError}
              </p>
            {/if}
            <!--
              Manual-drop escape hatch (audit-2). Corp networks that
              block huggingface.co can't use the Download button;
              surface the expected path so the user can drop the
              file there manually. Same affordance the Whisper
              picker provides via `expectedPath` on its cards.
            -->
            <details class="diarizer-manual-install">
              <summary>Or install manually</summary>
              <p class="settings-row-desc">
                Drop <code>{diarizerModelStatus.expectedPath}</code> with
                SHA-256 <code>{diarizerModelStatus.sha256}</code>. Restart
                Hush to load it.
              </p>
            </details>
          </div>
        {:else if diarizerModelStatus?.downloaded}
          <!--
            Installed-model details (#351). Replaces the old
            single-line "Speaker model installed." with the
            catalog metadata + a one-line description of how the
            labelling works + a Remove affordance. Collapsed
            details so the panel stays calm; user expands when
            they want to verify or copy a value out.
          -->
          <div class="diarizer-model-status" data-testid="diarizer-model-ready">
            <p class="settings-row-name">
              {diarizerModelStatus.displayName} — installed
            </p>
            <details class="diarizer-installed-details">
              <summary>Model details</summary>
              <dl class="diarizer-details">
                <dt>Size</dt>
                <dd>{diarizerModelStatus.sizeMb} MB</dd>
                <dt>Path</dt>
                <dd><code class="path-code">{diarizerModelStatus.expectedPath}</code></dd>
                <dt>SHA-256</dt>
                <dd><code class="path-code">{diarizerModelStatus.sha256}</code></dd>
                <dt>Source</dt>
                <dd>
                  <button
                    type="button"
                    class="link-like"
                    onclick={() =>
                      diarizerModelStatus &&
                      openExternal(diarizerModelStatus.sourceUrl)}
                    data-testid="diarizer-source-link"
                  >
                    {diarizerModelStatus.sourceUrl}
                  </button>
                </dd>
              </dl>
              <p class="settings-row-desc diarizer-explainer">
                Each utterance gets a 256-dim speaker embedding;
                embeddings are clustered live (1-NN with threshold)
                so utterances from the same voice get the same
                Speaker N label across the session. Labels reset
                between sessions.
              </p>
            </details>
            <div class="diarizer-installed-actions">
              {#if diarizerRemoveConfirming}
                <span class="settings-row-desc">
                  Delete the speaker model? You can re-download anytime.
                </span>
                <button
                  type="button"
                  class="ghost danger"
                  data-testid="diarizer-remove-confirm"
                  disabled={diarizerRemoveBusy}
                  onclick={onDiarizerRemoveConfirm}
                >
                  {diarizerRemoveBusy ? "Removing…" : "Yes, remove"}
                </button>
                <button
                  type="button"
                  class="ghost"
                  data-testid="diarizer-remove-cancel"
                  disabled={diarizerRemoveBusy}
                  onclick={() => (diarizerRemoveConfirming = false)}
                >
                  Cancel
                </button>
              {:else}
                <button
                  type="button"
                  class="ghost danger"
                  data-testid="diarizer-remove-button"
                  onclick={() => (diarizerRemoveConfirming = true)}
                >
                  Remove model
                </button>
              {/if}
            </div>
            {#if diarizerRemoveError}
              <p class="settings-error">{diarizerRemoveError}</p>
            {/if}
          </div>
        {/if}

        <label class="toggle-row">
          <input
            type="checkbox"
            data-testid="settings-diarization-toggle"
            disabled={diarizationBusy ||
              (diarizerModelStatus !== null && !diarizerModelStatus.downloaded)}
            checked={diarizationEnabled}
            onchange={onDiarizationToggle}
          />
          <span class="toggle-label">
            <span class="toggle-name">Label speakers in meeting transcripts</span>
            <span class="toggle-desc">
              Groups utterances by who spoke (Speaker 1, Speaker 2, …)
              instead of just tagging mic vs. system audio. Off
              keeps the simpler mic / system labels.
            </span>
          </span>
        </label>
        {#if diarizationError}
          <p class="settings-error">{diarizationError}</p>
        {/if}
      </section>

      <MeetingAppOverridesPanel
        overrides={appOverrides}
        overridesLoaded={appOverridesLoaded}
        overridesError={appOverridesError}
        defaults={appDefaults}
        bind:newAppName={newOverrideName}
        bind:newKind={newOverrideKind}
        bind:inputEl={overrideInputEl}
        onSubmit={addAppOverride}
        onSubmitVariants={addAppOverrideVariants}
        onChangeKind={changeAppOverrideKind}
        onDelete={deleteAppOverride}
      />
    {:else if active === "permissions"}
      <PermissionsTab />
    {:else if active === "about"}
      <h2 class="tab-title">About</h2>
      <section class="about-tab">
        <header class="about-header">
          <!--
            App name is subordinate to the "About" tab title (H2),
            so it's H3. Pre-fix it was a sibling H2 — two H2s with
            no semantic relationship was a hierarchy violation
            flagged in review #3.
          -->
          <h3 class="about-name">{appName}</h3>
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
            aria-label="Check for application updates"
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
              <!--
                Bare `reason` strings (e.g. "Try again in a few
                minutes.") read as fragmentary without a headline.
                With #281 the same surface now lights up from a
                menu click (potentially while the user's attention
                is elsewhere); the bold lead anchors what the
                paragraph is about.
              -->
              <p class="about-update-result about-update-failed" role="status">
                <strong>Couldn't check for updates.</strong>
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
              onclick={(e) => {
                e.preventDefault();
                openExternal("https://www.apache.org/licenses/LICENSE-2.0");
              }}
              rel="noopener noreferrer">Apache License 2.0</a
            >
          </dd>
          <dt>Source</dt>
          <dd>
            <a
              href="https://github.com/khawkins98/Hush"
              onclick={(e) => {
                e.preventDefault();
                openExternal("https://github.com/khawkins98/Hush");
              }}
              rel="noopener noreferrer">github.com/khawkins98/Hush</a
            >
          </dd>
          <dt>Report a bug</dt>
          <dd>
            <a
              href="https://github.com/khawkins98/Hush/issues/new"
              onclick={(e) => {
                e.preventDefault();
                openExternal("https://github.com/khawkins98/Hush/issues/new");
              }}
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
            onclick={(e) => {
              e.preventDefault();
              openExternal("https://github.com/ggerganov/whisper.cpp");
            }}
            rel="noopener noreferrer">whisper.cpp</a
          >,
          <a
            href="https://tauri.app"
            onclick={(e) => {
              e.preventDefault();
              openExternal("https://tauri.app");
            }}
            rel="noopener noreferrer">Tauri</a
          >, and
          <a
            href="https://svelte.dev"
            onclick={(e) => {
              e.preventDefault();
              openExternal("https://svelte.dev");
            }}
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

  /* (`.permissions-tab-header`, `.placeholder`, `.perm-recovery-intro`
     CSS moved to PermissionsTab.svelte in #332 phase 1.) */

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

  /* Slider-shaped settings row — same bordered-card pattern as
     `.toggle-row`. Label + description on the left, range input
     stretches across the bottom for fine-grained adjustment. */
  .slider-row {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
    cursor: pointer;
  }
  .slider-row input[type="range"] {
    width: 100%;
    /* Defensive dark-mode contrast (#348). The settings-window root
       sets `accent-color: auto` + `color-scheme: light dark`, so the
       slider thumb already adapts to the system theme on most
       backends. WebKit on macOS dark mode can render the native
       thumb with low contrast against the dark card background;
       pinning `color-scheme: light dark` *on the input itself*
       lets WebKit pick the dark-mode form-control palette directly
       rather than inheriting through the wrapper. No-op on
       light-mode and on backends that already render correctly. */
    color-scheme: light dark;
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

  /* Speakers panel installed-model details (#351). */
  .diarizer-installed-details {
    margin-top: 0.5rem;
  }
  .diarizer-installed-details summary {
    cursor: pointer;
    font-size: 0.85rem;
    color: #2c3e8f;
    user-select: none;
  }
  .diarizer-details {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.4rem 0.85rem;
    margin: 0.6rem 0 0.4rem;
    font-size: 0.85rem;
  }
  .diarizer-details dt {
    color: #555;
    font-weight: 500;
  }
  .diarizer-details dd {
    margin: 0;
    color: #1a1a1a;
    user-select: text;
    word-break: break-all;
  }
  .path-code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    color: #2a2a2a;
    background-color: rgba(0, 0, 0, 0.04);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }
  button.link-like {
    background: none;
    border: none;
    padding: 0;
    color: #2c3e8f;
    text-decoration: underline;
    cursor: pointer;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    word-break: break-all;
    text-align: left;
  }
  button.link-like:hover {
    color: #1a2a6c;
  }
  .diarizer-explainer {
    margin: 0.5rem 0 0;
    line-height: 1.5;
  }
  .diarizer-installed-actions {
    margin-top: 0.65rem;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
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
    .toggle-row,
    .select-row,
    .slider-row,
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
