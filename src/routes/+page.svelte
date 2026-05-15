<script lang="ts">
  import { emit } from "@tauri-apps/api/event";
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  import AppLifecycle from "$lib/AppLifecycle.svelte";
  import CommandPalette from "$lib/CommandPalette.svelte";
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
  import { motionDuration } from "$lib/motion";
  import { audio } from "$lib/state/audio.svelte";
  import { dictation, TRAILING_SILENCE_MS } from "$lib/state/dictation.svelte";
  import { history } from "$lib/state/history.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";
  import { nav } from "$lib/state/nav.svelte";
  import { onboarding } from "$lib/state/onboarding.svelte";
  import { palette } from "$lib/state/palette.svelte";
  import { permissions } from "$lib/state/permissions.svelte";

  // ⌘K command palette (#411 phase F3). State + the action set
  // are colocated here because every action needs the page's
  // existing handlers and state. The palette component itself is a
  // presentational leaf — see lib/CommandPalette.svelte.
  let paletteOpen = $state(false);

  // Platform check used to pick the right modifier glyph in the
  // shortcut hint (Right ⌘ on macOS, Right Ctrl elsewhere).
  let isMacOS = $state(false);

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
    document.title = dictation.anyRecordingActive ? "Hush ● Recording" : "Hush";
  });

  // Push recording state to the backend so the tray's "Start / Stop
  // Recording" menu item label can mirror the UI.
  $effect(() => {
    void emit(Events.UiRecordingState, dictation.anyRecordingActive);
  });

  // Debounce the search input so we don't fire SQLite queries on
  // every keystroke. 200ms is the empirical sweet spot.
  function onSearchInput(e: Event) {
    history.setSearchQuery((e.target as HTMLInputElement).value);
  }
</script>

<AppLifecycle
  bind:isMacOS
  onGlobalKeydown={handleGlobalKeydown}
/>

<FirstRunModal
  show={onboarding.showFirstRun}
  onDismiss={() => onboarding.completeFirstRun()}
  onOpenPrivacyPane={(t) => permissions.openPrivacyPane(t)}
/>

<!--
  Permission-health lifecycle + recovery dialog (#432). The
  section owns the focus-debounced probe and the
  diagnose_macos_permissions one-shot; all resulting state lands
  in the shared `permissions` module (#722).
-->
<PermissionHealthSection
  onOpenPrivacyPane={(t) => permissions.openPrivacyPane(t)}
/>

<!--
  ⌘K command palette (#411 phase F3). Mounts above the rest of the
  page so the backdrop covers everything; the binding is wired in
  the global keydown handler registered by AppLifecycle.
-->
<CommandPalette
  open={paletteOpen}
  actions={palette.actions}
  onClose={() => (paletteOpen = false)}
/>

<div class="app-shell">
  <SidebarNav
    bind:active={nav.activeSection}
    recording={dictation.anyRecordingActive}
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
  {#if permissions.showStaleBanner}
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
      onclick={() => (permissions.staleBannerDismissed = true)}
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
    Meeting-append-failed banner (#696): shown when a transcription
    couldn't be written to the active meeting session. The text still
    landed on the clipboard so the user didn't lose their work, but the
    session log is incomplete. Dismissed automatically when the session
    ends or manually with ✕.
  -->
  {#if meeting.appendFailedNotice}
  <div class="source-failed-banner" role="alert" data-testid="append-failed-banner">
    <span class="source-failed-banner-icon" aria-hidden="true">⚠️</span>
    <span class="source-failed-banner-text">{meeting.appendFailedNotice}</span>
    <button
      type="button"
      class="source-failed-banner-dismiss"
      aria-label="Dismiss"
      onclick={() => (meeting.appendFailedNotice = null)}
    >✕</button>
  </div>
  {/if}
  <!--
    Meeting-tail-dropped banner (#833): shown when one or more streaming
    sessions failed to flush their tail utterances at meeting stop. The
    last few seconds of audio were lost. Unlike source-failed and
    append-failed, this banner is NOT cleared on session-ended (it fires
    because the session ended) — only dismissed manually.
  -->
  {#if meeting.tailDroppedNotice}
  <div class="source-failed-banner" role="alert" data-testid="tail-dropped-banner">
    <span class="source-failed-banner-icon" aria-hidden="true">⚠️</span>
    <span class="source-failed-banner-text">{meeting.tailDroppedNotice}</span>
    <button
      type="button"
      class="source-failed-banner-dismiss"
      aria-label="Dismiss"
      onclick={() => (meeting.tailDroppedNotice = null)}
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
    permissionHealth={permissions.permissionHealth}
    macosCapable={permissions.macosCapable}
    allPermsGranted={permissions.allPermsGranted}
    anyPermsDenied={permissions.anyPermsDenied}
    onStart={() => dictation.startRecord()}
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
          data-testid="app-profile-notice-dismiss"
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
      {onSearchInput}
      onCopy={history.copyEntry}
      onDelete={history.deleteEntry}
      onExportDictationCsv={history.exportDictationCsv}
      onMeetingDelete={meeting.deleteSession}
      onMeetingLoadDetail={meeting.loadSessionDetail}
      onMeetingCopy={(s) => meeting.copyToClipboard(s.id)}
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
