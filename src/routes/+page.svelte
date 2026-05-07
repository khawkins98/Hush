<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { platform } from "@tauri-apps/plugin-os";
  import { onDestroy, onMount } from "svelte";
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import CommandPalette from "$lib/CommandPalette.svelte";
  import type { CommandAction } from "$lib/CommandPalette.svelte";
  import DictationSection from "$lib/DictationSection.svelte";
  import SettingsPanel from "$lib/SettingsPanel.svelte";
  import type { SettingsTab } from "$lib/SettingsPanel.svelte";
  import SidebarNav from "$lib/SidebarNav.svelte";
  import HistoryPanel from "$lib/HistoryPanel.svelte";
  import AboutTab from "$lib/AboutTab.svelte";
  import FirstRunModal from "$lib/FirstRunModal.svelte";
  import MeetingSection from "$lib/MeetingSection.svelte";
  import PermissionHealthSection from "$lib/PermissionHealthSection.svelte";
  import { Events } from "$lib/events";
  import { formatTimestamp } from "$lib/format";
  import { motionDuration } from "$lib/motion";
  import type {
    PermissionStatuses,
    PermissionsHealth,
  } from "$lib/types";
  import { audio } from "$lib/state/audio.svelte";
  import { dictation, TRAILING_SILENCE_MS } from "$lib/state/dictation.svelte";
  import { history } from "$lib/state/history.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";
  import { nav } from "$lib/state/nav.svelte";

  // ⌘K command palette (#411 phase F3). State + the action set
  // are colocated here because every action needs the page's
  // existing handlers and state. The palette component itself is a
  // presentational leaf — see lib/CommandPalette.svelte.
  let paletteOpen = $state(false);

  // Platform check used to pick the right modifier glyph in the
  // shortcut hint (Right ⌘ on macOS, Right Ctrl elsewhere).
  let isMacOS = $state(false);

  // First-run welcome flow.
  let showFirstRun = $state(false);

  // ---- macOS permission diagnostic surface ----
  let macosCapable = $state(false);
  let permStatuses = $state<PermissionStatuses | null>(null);
  let permissionHealth = $state<PermissionsHealth | null>(null);
  let screenRecordingLive = $derived(
    audio.findSystemAudio()?.isSupported ?? false,
  );
  let allPermsGranted = $derived(
    !!permStatuses
      && permStatuses.microphone === "granted"
      && permStatuses.inputMonitoring !== "denied",
  );
  let anyPermsDenied = $derived(
    !!permStatuses
      && (permStatuses.microphone === "denied"
        || permStatuses.inputMonitoring === "denied"),
  );

  // Stale-banner: at least one permission was previously granted
  // but macOS no longer recognises it (common after ad-hoc rebuilds
  // where the csreq hash changes). Show a dismissable amber banner
  // to surface the issue proactively rather than waiting for the
  // user to notice the Settings → Permissions traffic-light.
  let anyPermsStale = $derived(
    macosCapable
      && !!permissionHealth
      && (permissionHealth.microphone === "stale"
        || permissionHealth.inputMonitoring === "stale"),
  );
  // Session-only dismiss — not persisted. The stale state is
  // tied to the running build's csreq, so a new launch (new build
  // or after granting fresh) re-evaluates it naturally.
  let staleBannerDismissed = $state(false);

  // Reusable permissions dialog (#232).
  let showPermissionsDialog = $state(false);
  let permissionsDialogIntro: string | undefined = $state(undefined);

  let unlistenToggle: UnlistenFn | null = null;
  let unlistenPttPress: UnlistenFn | null = null;
  let unlistenPttRelease: UnlistenFn | null = null;
  let unlistenMenuGoto: UnlistenFn | null = null;
  let unlistenSettingsGoto: UnlistenFn | null = null;
  let unlistenDownloadDone: UnlistenFn | null = null;
  let unlistenAppProfileActivated: UnlistenFn | null = null;
  let unlistenMeetingSourceFailed: UnlistenFn | null = null;

  // PTT state machine.
  //
  // Minimum hold time (ms) before a PTT press is treated as intentional.
  // Taps shorter than this are discarded as accidental. 100 ms is below
  // deliberate-hold perception but clears OS key-bounce artifacts and
  // rapid accidental taps.
  const PTT_MIN_HOLD_MS = 100;
  // Whether the PTT key is physically down right now. Used by the timer
  // callback to avoid starting a recording after the key was already
  // released, and to detect the stuck-recording race (key released while
  // start IPC was in-flight).
  let pttIsDown = false;
  // True only when PTT itself started the current recording. Guards the
  // release handler from stopping a recording that was started by the UI
  // button or the toggle hotkey.
  let pttOwnedRecording = false;
  // Non-null while the minimum-hold guard timer is running (before we
  // commit to starting a recording). Cancelled on early key-up.
  let pttPressTimer: ReturnType<typeof setTimeout> | null = null;

  let meetingActivePollHandle: ReturnType<typeof setInterval> | null = null;

  let paletteActions = $derived<CommandAction[]>([
    {
      id: "dictation.start",
      label: "Start dictation",
      subtitle: dictation.noModelInstalled ? "Choose a model first" : undefined,
      group: "Dictation",
      enabled:
        !dictation.recording && !dictation.busy && !dictation.noModelInstalled,
      run: () => {
        void dictation.startRecord(screenRecordingLive);
      },
    },
    {
      id: "dictation.stop",
      label: "Stop dictation",
      subtitle: "Stop the current recording and transcribe",
      group: "Dictation",
      enabled: dictation.recording,
      run: () => {
        void dictation.stop(TRAILING_SILENCE_MS);
      },
    },
    {
      id: "navigate.history",
      label: "Show History",
      subtitle: "Switch to the History panel",
      group: "Navigate",
      run: () => {
        nav.activeSection = "history";
      },
    },
    {
      id: "navigate.dictation",
      label: "Show Dictation",
      subtitle: "Switch back to the dictation panel",
      group: "Navigate",
      enabled: nav.activeSection !== "dictation",
      run: () => {
        nav.activeSection = "dictation";
      },
    },
    {
      id: "settings.general",
      label: "Open Settings: General",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("general");
      },
    },
    {
      id: "settings.model",
      label: "Open Settings: Models",
      subtitle: dictation.activeModel?.displayName ?? "No model loaded",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("model");
      },
    },
    {
      id: "settings.vocabulary",
      label: "Open Settings: Vocabulary",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("vocabulary");
      },
    },
    {
      id: "settings.replacements",
      label: "Open Settings: Replacements",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("replacements");
      },
    },
    {
      id: "settings.meeting",
      label: "Open Settings: Meeting",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("meeting");
      },
    },
    {
      id: "settings.permissions",
      label: "Open Settings: Permissions",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("permissions");
      },
    },
    {
      id: "settings.about",
      label: "Show About",
      group: "Settings",
      run: () => {
        nav.openSettingsTab("about");
      },
    },
  ]);

  function handleGlobalKeydown(event: KeyboardEvent) {
    // ⌘K opens the palette; ⌘K again closes (toggle). Cmd on
    // macOS, Ctrl elsewhere — matches the platform muscle memory
    // for "spotlight-style" pickers. Only fire when the user
    // isn't typing into a textfield other than the palette's
    // own input.
    const isMod = event.metaKey || event.ctrlKey;
    if (!isMod || event.key.toLowerCase() !== "k") return;
    const target = event.target as HTMLElement | null;
    if (
      target
      && target.closest('[data-testid="command-palette"]') === null
      && (target.tagName === "INPUT"
        || target.tagName === "TEXTAREA"
        || target.isContentEditable)
    ) {
      return;
    }
    event.preventDefault();
    paletteOpen = !paletteOpen;
  }

  function openModelSettings() {
    nav.openSettingsTab("model");
  }

  // Keep the document title in sync with recording state. Helps users who
  // have the window in the background — at-a-glance signal that the mic
  // is hot. Tauri exposes `window.document` like a regular browser.
  $effect(() => {
    document.title = dictation.recording ? "Hush ● Recording" : "Hush";
  });

  // Push recording state to the backend so the tray's "Start / Stop
  // Recording" menu item label can mirror the UI.
  $effect(() => {
    void emit(Events.UiRecordingState, dictation.recording);
  });

  $effect(() => {
    if (dictation.pendingPermissionsDialogIntro !== null) {
      permissionsDialogIntro = dictation.pendingPermissionsDialogIntro;
      showPermissionsDialog = true;
      dictation.clearPendingPermissionsDialog();
    }
  });

  $effect(() => {
    if (meeting.pendingPermissionsDialogIntro !== null) {
      permissionsDialogIntro = meeting.pendingPermissionsDialogIntro;
      showPermissionsDialog = true;
      meeting.clearPendingPermissionsDialog();
    }
  });

  // Poll for the active meeting's live transcript while it is in flight.
  // This stays in the page so the interval is owned by component lifecycle.
  $effect(() => {
    if (meetingActivePollHandle !== null) {
      clearInterval(meetingActivePollHandle);
      meetingActivePollHandle = null;
    }
    const id = meeting.activeId;
    if (id === null) {
      meeting.clearActiveDetail();
      return;
    }
    void meeting.refreshActiveDetail(id);
    meetingActivePollHandle = setInterval(() => {
      void meeting.refreshActiveDetail(id);
    }, 3000);
    return () => {
      if (meetingActivePollHandle !== null) {
        clearInterval(meetingActivePollHandle);
        meetingActivePollHandle = null;
      }
    };
  });

  onMount(async () => {
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() probe failed; defaulting to non-macOS glyph", e);
    }

    // Check the first-run flag BEFORE the Promise.all — round-7 UX
    // reviewer caught a real timing bug.
    try {
      const done = await invoke<boolean>("get_first_run_completed");
      if (!done) showFirstRun = true;
    } catch (e) {
      console.error("get_first_run_completed failed:", e);
    }

    await Promise.all([
      dictation.loadSources(),
      history.refresh(),
      dictation.refreshModels(),
      meeting.refresh(),
    ]);

    unlistenToggle = await listen(Events.HotkeyToggle, () => {
      if (dictation.busy) return;
      if (dictation.recording) void dictation.stop(TRAILING_SILENCE_MS);
      else void dictation.start();
    });

    unlistenMenuGoto = await listen<string>(Events.MenuGotoSection, (e) => {
      const payload = e.payload;
      nav.activeSection =
        payload === "meetings" || payload === "history"
          ? "history"
          : "dictation";
    });

    unlistenSettingsGoto = await listen<string>(Events.SettingsGotoTab, (e) => {
      nav.openSettingsTab(e.payload as SettingsTab | "about");
    });

    unlistenDownloadDone = await listen<{ id: string }>(
      Events.ModelDownloadDone,
      () => {
        void dictation.refreshModels();
      },
    );

    unlistenAppProfileActivated = await listen<{
      appName: string;
      preferredAudioSource: string | null;
      preferredModelId: string | null;
    }>(Events.AppProfileActivated, (e) => {
      void dictation.onAppProfileActivated(e.payload);
    });

    unlistenPttPress = await listen(Events.HotkeyPttPress, () => {
      pttIsDown = true;
      if (dictation.busy || dictation.recording) return;
      // Ignore key-repeat events while the hold timer is already running.
      if (pttPressTimer !== null) return;
      pttPressTimer = setTimeout(() => {
        pttPressTimer = null;
        // Key may have been released before the timer fired (short tap).
        if (!pttIsDown || dictation.busy || dictation.recording) return;
        void dictation.start().then(() => {
          if (!dictation.recording || dictation.busy) return;
          if (pttIsDown) {
            // Normal case: key still held — mark this recording as PTT-owned
            // so the release handler knows it's allowed to stop it.
            pttOwnedRecording = true;
          } else {
            // Stuck-recording race: key was released while start IPC was
            // in-flight. Release handler saw busy=true and skipped stop().
            void dictation.stop(TRAILING_SILENCE_MS);
          }
        });
      }, PTT_MIN_HOLD_MS);
    });
    unlistenPttRelease = await listen(Events.HotkeyPttRelease, () => {
      pttIsDown = false;
      if (pttPressTimer !== null) {
        // Key released before the minimum hold elapsed — treat as an
        // accidental tap. Cancel the timer; nothing starts or stops.
        clearTimeout(pttPressTimer);
        pttPressTimer = null;
        return;
      }
      // If start() is in-flight (busy=true, recording=false), the
      // post-start callback above will call stop() once it resolves.
      // Only stop if PTT itself started this recording — don't interrupt
      // a UI-button or toggle-hotkey recording on PTT key release.
      if (!pttOwnedRecording || !dictation.recording || dictation.busy) return;
      pttOwnedRecording = false;
      void dictation.stop(TRAILING_SILENCE_MS);
    });

    // Surface per-source transcription failures as a banner (#533).
    // The backend emits this at session start (source failed to open)
    // and mid-session (drain failure / panic). No activeId gate here:
    // startup failures arrive before the invoke resolves and sets
    // activeId, so gating would silently drop the very case this
    // banner is designed for. The backend never emits for a stopped
    // session, so stale-event bleed isn't a real risk.
    unlistenMeetingSourceFailed = await listen<{
      sessionId: number;
      sourceKind: string;
      reason: string;
    }>(Events.MeetingSourceFailed, (e) => {
      console.debug(
        "[MeetingSourceFailed]",
        e.payload.sourceKind,
        e.payload.reason,
        "sessionId:",
        e.payload.sessionId,
      );
      const label =
        e.payload.sourceKind === "mic" ? "Microphone" : "System audio";
      // Three reason classes:
      //   "at session start" — never started (pre-warm or
      //   start_stream failure caught in lifecycle.rs).
      //   "device disconnected" — mid-session DeviceLost from the
      //   audio backend (#587 PR 2a). The user pulled their mic /
      //   AirPods walked out / webcam disabled.
      //   anything else — generic mid-session drain failure or
      //   inference panic (#591's existing surface).
      // Multi-source intent: the user picked a mic AND opted into
      // system audio, AND system audio was supported on this host.
      // Mirrors the include-source logic in meeting-sessions.svelte.ts.
      // When this is true and only one source failed, the other is
      // still capturing — claiming "recording stopped" would be a lie.
      const wasMultiSource =
        audio.meetingMicId !== null &&
        audio.meetingIncludeSystemAudio &&
        audio.findSystemAudio()?.isSupported === true;
      const otherSourceLabel =
        e.payload.sourceKind === "mic" ? "system audio" : "microphone";

      let verb: string;
      if (e.payload.reason.includes("at session start")) {
        verb = "couldn't start";
      } else if (e.payload.reason.includes("disconnected")) {
        verb = wasMultiSource
          ? `disconnected mid-session — ${otherSourceLabel} still recording`
          : "disconnected mid-session — recording stopped";
      } else {
        verb = "stopped transcribing";
      }
      meeting.sourceFailedNotice = `${label} ${verb}.`;
    });

    window.addEventListener("keydown", handleGlobalKeydown);
  });

  onDestroy(() => {
    unlistenToggle?.();
    unlistenMenuGoto?.();
    unlistenSettingsGoto?.();
    unlistenPttPress?.();
    unlistenPttRelease?.();
    unlistenDownloadDone?.();
    unlistenAppProfileActivated?.();
    unlistenMeetingSourceFailed?.();
    if (pttPressTimer !== null) {
      clearTimeout(pttPressTimer);
      pttPressTimer = null;
    }
    window.removeEventListener("keydown", handleGlobalKeydown);
    if (meetingActivePollHandle !== null) {
      clearInterval(meetingActivePollHandle);
      meetingActivePollHandle = null;
    }
    dictation.cleanup();
  });

  // Debounce the search input so we don't fire SQLite queries on
  // every keystroke. 200ms is the empirical sweet spot.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearchInput(e: Event) {
    history.historyQuery = (e.target as HTMLInputElement).value;
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      void history.refresh();
      void meeting.refresh();
    }, 200);
  }

  async function dismissFirstRun() {
    // The first-run wizard now starts on the Permissions step (#609),
    // so by the time it dismisses the user has already had a chance
    // to grant the OS permissions inline. Pre-#609 we auto-opened
    // PermissionsDialog right after the wizard, which produced a
    // redundant third "permissions" surface — the user saw the same
    // rows twice in a row. PermissionsDialog stays around for its
    // other use cases (ad-hoc launches from permission-shaped errors,
    // Settings → Permissions); it's just no longer auto-opened here.
    showFirstRun = false;
    try {
      await invoke("mark_first_run_completed");
    } catch (e) {
      console.error("mark_first_run_completed failed:", e);
    }
  }

  async function openPrivacyPane(
    target: "microphone" | "input-monitoring" | "screen-recording",
  ) {
    try {
      await invoke("open_macos_privacy_pane", { target });
    } catch (e) {
      console.error("open_macos_privacy_pane failed:", e);
    }
  }
</script>

<FirstRunModal
  show={showFirstRun}
  onDismiss={dismissFirstRun}
  onOpenPrivacyPane={openPrivacyPane}
/>

<!--
  Permission-health lifecycle + recovery dialog (#432). The
  section owns the focus-debounced probe and the
  diagnose_macos_permissions one-shot; the orchestrator binds the
  state so welcome derivations and the MacosPermsPill render as
  before.
-->
<PermissionHealthSection
  bind:permissionHealth
  bind:permStatuses
  bind:macosCapable
  bind:showDialog={showPermissionsDialog}
  bind:dialogIntro={permissionsDialogIntro}
  onOpenPrivacyPane={openPrivacyPane}
/>

<!--
  ⌘K command palette (#411 phase F3). Mounts above the rest of the
  page so the backdrop covers everything; the binding is wired in
  the global keydown handler in onMount.
-->
<CommandPalette
  open={paletteOpen}
  actions={paletteActions}
  onClose={() => (paletteOpen = false)}
/>

<div class="app-shell">
  <SidebarNav
    bind:active={nav.activeSection}
    recording={dictation.recording}
    open={nav.sidebarOpen}
    onSelect={nav.onSidebarSelect}
    onToggle={nav.onSidebarToggle}
  />

<main class="app-main" data-active-section={nav.activeSection}>
  <!--
    Stale-permission banner (#520). Shown when get_permission_health
    returns Stale for any permission — common after an ad-hoc signed
    rebuild where the csreq hash changes. Hidden when the user is
    already on Settings → Permissions (they can see the rows directly)
    or has dismissed it for this session.
  -->
  {#if anyPermsStale && !staleBannerDismissed && !(nav.activeSection === "settings" && nav.settingsActiveTab === "permissions")}
  <div class="stale-perm-banner" role="alert">
    <span class="stale-perm-banner-text">
      ⚠️ A macOS permission may need to be re-granted — this can happen after updating Hush.
    </span>
    <button
      type="button"
      class="stale-perm-banner-btn"
      onclick={() => nav.openSettingsTab("permissions")}
    >Open Permissions</button>
    <button
      type="button"
      class="stale-perm-banner-dismiss"
      aria-label="Dismiss"
      onclick={() => (staleBannerDismissed = true)}
    >✕</button>
  </div>
  {/if}
  <!--
    Meeting source-failed banner (#533). Shown when the backend emits
    `meeting:source-failed` during an active session — at startup if a
    source fails to open a streaming session, or mid-session on panic /
    drain failure. Visible regardless of which sidebar section is active
    so the user sees it even if they switch away from Dictation while
    the meeting runs. Dismissed automatically when the session ends (the
    listener clears `meeting.sourceFailedNotice` in stopSession), or
    manually with ✕.
  -->
  {#if meeting.sourceFailedNotice}
  <div class="source-failed-banner" role="alert" data-testid="source-failed-banner">
    <span class="source-failed-banner-icon" aria-hidden="true">⚠️</span>
    <span class="source-failed-banner-text">{meeting.sourceFailedNotice}</span>
    <button
      type="button"
      class="source-failed-banner-dismiss"
      aria-label="Dismiss"
      onclick={() => (meeting.sourceFailedNotice = null)}
    >✕</button>
  </div>
  {/if}
  <!--
    Dictation section markup extracted into a leaf (#432 slice
    3/3). Action functions + hotkey listeners stay in this
    orchestrator because they touch a sprawl of cross-section
    state — the section component is the render boundary, the
    page is the controller. With #479 slice 1 the Dictation +
    History sections are mutually exclusive — the active
    sidebar item drives which one mounts.
  -->
  {#if nav.activeSection === "dictation"}
  <DictationSection
    {isMacOS}
    sources={audio.sources}
    sourcesLoaded={audio.sourcesLoaded}
    bind:selected={audio.selected}
    recording={dictation.recording}
    busy={dictation.busy}
    transcribing={dictation.transcribing}
    noModelInstalled={dictation.noModelInstalled}
    error={dictation.error}
    result={dictation.result}
    recordMode={dictation.recordMode}
    activeModelName={dictation.activeModel?.displayName ?? null}
    {permissionHealth}
    {macosCapable}
    {allPermsGranted}
    {anyPermsDenied}
    meetingActiveDetail={meeting.activeDetail}
    onStart={() => dictation.startRecord(screenRecordingLive)}
    onStop={() => dictation.stop(TRAILING_SILENCE_MS)}
    onScrollToModelPicker={openModelSettings}
    onOpenPermissionsTab={() => nav.openSettingsTab("permissions")}
  />
  {/if}

  {#if nav.activeSection === "history"}
  <section id="history-section" class="page-section">
    <header class="section-header">
      <h1>History</h1>
    </header>

    {#if dictation.appProfileNotice}
      <!--
        Per-app audio profile auto-apply notice (#427 Item 5 /
        #457). Auto-clears after ~3 s; the user can dismiss
        sooner with the close button if it's in the way.
        role="status" for SR announcement, data-testid for
        Playwright coverage.
      -->
      <div
        class="app-profile-notice"
        role="status"
        data-testid="app-profile-notice"
        in:fly={{ y: -6, duration: motionDuration(200), easing: backOut }}
        out:fade={{ duration: motionDuration(150), easing: cubicIn }}
      >
        <span class="app-profile-notice-icon" aria-hidden="true">↻</span>
        <span class="app-profile-notice-message">{dictation.appProfileNotice}</span>
        <button
          type="button"
          class="app-profile-notice-dismiss"
          aria-label="Dismiss profile-switched notice"
          onclick={() => {
            dictation.clearAppProfileNotice();
          }}
        >×</button>
      </div>
    {/if}

    <!-- Auto-copy outcome notice (#408 / #432 slice 2/3). -->
    <MeetingSection bind:notice={meeting.copyNotice} />

    <HistoryPanel
      historyEntries={history.entries}
      historyLoaded={history.loaded}
      historyQuery={history.historyQuery}
      historySearching={history.searching}
      historyError={history.error}
      historyVersion={history.version}
      historyTotalCount={history.totalCount}
      meetingSessions={meeting.sessions}
      meetingSessionsLoaded={meeting.sessionsLoaded}
      models={dictation.models}
      {formatTimestamp}
      {onSearchInput}
      onCopy={history.copyEntry}
      onDelete={history.deleteEntry}
      onExportDictationCsv={history.exportDictationCsv}
      onMeetingDelete={meeting.deleteSession}
      onMeetingLoadDetail={meeting.loadSessionDetail}
      onMeetingExport={history.exportMeetingSession}
      onExportBundle={history.exportBundle}
      onClearAll={history.clearAll}
    />
  </section>
  {/if}

  {#if nav.activeSection === "settings"}
    <SettingsPanel bind:activeTab={nav.settingsActiveTab} onModelLoaded={dictation.handleModelLoaded} />
  {/if}

  {#if nav.activeSection === "about"}
    <div class="about-panel">
      <AboutTab />
    </div>
  {/if}
</main>
</div>

<style>
:root {
  /* System font stack — picks San Francisco on macOS, Segoe UI on
     Windows, the distro default on Linux. Inter / Avenir were
     close-enough fallbacks but rendered noticeably "off" on macOS,
     so the app deliberately uses whatever the host considers
     native instead. The trailing emoji families let macOS render
     coloured emoji inline; Linux fonts handle the rest. */
  font-family:
    -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
    Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif,
    "Apple Color Emoji", "Segoe UI Emoji";

  /* Layer 1 of "feel native". Two CSS primitives, no per-OS code:
     - `color-scheme` opts into the user agent's native dark-mode
       rendering for form controls, scrollbars, and the document
       background. Without this, scrollbars on macOS render as the
       light-mode style even when the rest of the app is dark.
     - `accent-color: auto` makes checkboxes / radios / range
       sliders / progress bars pick up the user's OS accent (the
       Mac highlight blue, the Windows accent, the GNOME accent)
       instead of the browser default cobalt. One line, real
       impact on perceived nativeness. */
  color-scheme: light dark;
  accent-color: auto;

  font-size: 16px;
  line-height: 24px;
  color: var(--text-primary);
  background-color: var(--bg-app);
  font-synthesis: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

/* Single-scroll layout: top bar + main content column.
   Brand mark is centre-anchored (#450 / former #411 Phase E) —
   reads more like a polished native app title bar than a
   web-style left-aligned logo. Settings button sits at the
   right, end-aligned via flex. The brand is `position: absolute`
/* #479 slice 1: flex shell hosts the left icon sidebar + the
   active panel. Subtracts the sticky app-bar's height so the
   shell fills the remaining viewport — same total height the
   pre-r3 single-column layout occupied, just split horizontally. */
.app-shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
}

/* Padding-left tightened from 1.5rem → 1rem because the sidebar's
   right border already provides visual separation; pre-sidebar
   padding had to do the visual work the sidebar now does. Right
   padding stays 1.5rem so scrollbar gutter has breathing room. */
.app-main {
  flex: 1;
  padding: 0 1.5rem 4rem 1rem;
  text-align: left;
  overflow-y: auto;
  box-sizing: border-box;
  min-width: 0;
}

/* Stale-permission banner (#520). Amber warning bar that appears
   when any permission health is "stale" (csreq mismatch after
   a rebuild). Lives at the top of .app-main so it's visible
   regardless of which section is active. */
.stale-perm-banner {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.55rem 0.8rem;
  margin: 0.75rem 0 0;
  background-color: #fdf6e3;
  border: 1px solid #e0a020;
  border-radius: 7px;
  font-size: 0.85rem;
  flex-wrap: wrap;
}

/* Meeting source-failed banner (#533). Same amber style as the
   stale-perm banner; shown when a mic or system-audio source
   stops transcribing mid-session. */
.source-failed-banner {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  padding: 0.55rem 0.8rem;
  margin: 0.75rem 0 0;
  background-color: #fdf6e3;
  border: 1px solid #e0a020;
  border-radius: 7px;
  font-size: 0.85rem;
  flex-wrap: wrap;
}

.source-failed-banner-icon {
  flex-shrink: 0;
}

.source-failed-banner-text {
  flex: 1;
  min-width: 0;
  color: #5a3e00;
  line-height: 1.4;
}

.source-failed-banner-dismiss {
  padding: 0.2em 0.5em;
  font-size: 0.78rem;
  border: none;
  background: transparent;
  cursor: pointer;
  color: #7a5500;
  border-radius: 4px;
  font-family: inherit;
  transition: background-color 0.1s;
}

.source-failed-banner-dismiss:hover {
  background-color: rgba(0, 0, 0, 0.07);
}

.stale-perm-banner-text {
  flex: 1;
  min-width: 0;
  color: #5a3e00;
  line-height: 1.4;
}

.stale-perm-banner-btn {
  padding: 0.25em 0.7em;
  font-size: 0.82rem;
  font-weight: 600;
  border: 1px solid #c08000;
  background-color: #fff8e6;
  border-radius: 5px;
  cursor: pointer;
  color: #5a3e00;
  white-space: nowrap;
  font-family: inherit;
  transition: background-color 0.1s;
}

.stale-perm-banner-btn:hover {
  background-color: #ffedc0;
}

.stale-perm-banner-dismiss {
  padding: 0.2em 0.5em;
  font-size: 0.78rem;
  border: none;
  background: transparent;
  cursor: pointer;
  color: #7a5500;
  border-radius: 4px;
  font-family: inherit;
  transition: background-color 0.1s;
}

.stale-perm-banner-dismiss:hover {
  background-color: rgba(0, 0, 0, 0.07);
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .stale-perm-banner {
    background-color: #2a2200;
    border-color: #7a5500;
  }
  :root:not([data-theme="light"]) .stale-perm-banner-text {
    color: #f0c878;
  }
  :root:not([data-theme="light"]) .stale-perm-banner-btn {
    background-color: #2a2200;
    border-color: #7a5500;
    color: #f0c878;
  }
  :root:not([data-theme="light"]) .stale-perm-banner-btn:hover {
    background-color: #3a3000;
  }
  :root:not([data-theme="light"]) .stale-perm-banner-dismiss {
    color: #c08000;
  }
}
:root[data-theme="dark"] .stale-perm-banner {
  background-color: #2a2200;
  border-color: #7a5500;
}
:root[data-theme="dark"] .stale-perm-banner-text {
  color: #f0c878;
}
:root[data-theme="dark"] .stale-perm-banner-btn {
  background-color: #2a2200;
  border-color: #7a5500;
  color: #f0c878;
}
:root[data-theme="dark"] .stale-perm-banner-btn:hover {
  background-color: #3a3000;
}
:root[data-theme="dark"] .stale-perm-banner-dismiss {
  color: #c08000;
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .source-failed-banner {
    background-color: #2a2200;
    border-color: #7a5500;
  }
  :root:not([data-theme="light"]) .source-failed-banner-text {
    color: #f0c878;
  }
  :root:not([data-theme="light"]) .source-failed-banner-dismiss {
    color: #c08000;
  }
}
:root[data-theme="dark"] .source-failed-banner {
  background-color: #2a2200;
  border-color: #7a5500;
}
:root[data-theme="dark"] .source-failed-banner-text {
  color: #f0c878;
}
:root[data-theme="dark"] .source-failed-banner-dismiss {
  color: #c08000;
}

/* About as a standalone sidebar section. AboutTab's own
   .tab-title gives the "About" heading; just add breathing room
   to match the Settings panel's visual top-padding. */
.about-panel {
  padding-top: 1.5rem;
  max-width: 44rem;
}

.page-section {
  padding-top: 2.5rem;
}

/* :global() so the selector still matches when the dictation
   `<section>` is rendered from DictationSection — Svelte's
   scoped CSS hashes are per-component and the adjacent-sibling
   selector would otherwise see one hash on each side. */
:global(.page-section + .page-section) {
  border-top: 1px solid var(--border, #e1e1e1);
  margin-top: 2rem;
  padding-top: 2.5rem;
}


.section-header {
  margin-bottom: 1.5rem;
}
.section-header h1 {
  margin: 0 0 0.25rem;
  font-size: 1.75rem;
  letter-spacing: -0.02em;
}

/* Meeting auto-copy outcome notice (#408). Sits between the
   History section header and the panel, gated on
   meetingCopyNotice being set. Two visual variants drive off
   data-kind: success (green-tinted) auto-clears after 4 s,
   failure (amber-tinted) after 10 s. Dismiss button is a
   manual escape hatch in case the dwell feels long. */
/* Per-app audio profile auto-apply notice (#427 Item 5 / #457).
   Subtle accent-tinted, matches the meeting-copy-notice's row
   geometry so the two notices line up cleanly when both fire
   in quick succession. */
.app-profile-notice {
  display: flex;
  align-items: flex-start;
  gap: 0.55rem;
  padding: 0.6rem 0.85rem;
  margin: 0 0 1rem;
  border-radius: 8px;
  font-size: 0.88rem;
  line-height: 1.4;
  border: 1px solid var(--accent-subtle, rgba(124, 111, 247, 0.18));
  background-color: var(--accent-subtle, rgba(124, 111, 247, 0.12));
  color: var(--accent-hover, #5c4fd4);
}
.app-profile-notice-icon {
  font-weight: 700;
  flex-shrink: 0;
  line-height: 1.4;
}
.app-profile-notice-message {
  flex: 1;
  min-width: 0;
}
.app-profile-notice-dismiss {
  flex-shrink: 0;
  background: none;
  border: 0;
  padding: 0 0.25rem;
  font-size: 1.05rem;
  line-height: 1;
  cursor: pointer;
  color: inherit;
  opacity: 0.75;
}
.app-profile-notice-dismiss:hover {
  opacity: 1;
}

</style>
