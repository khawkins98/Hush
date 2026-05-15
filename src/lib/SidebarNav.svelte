<!--
  Left navigation sidebar (#479 slice 1, refreshed for #494).

  Two groups of nav items:
  - **Primary** (top): Dictation, History — the core workflow surfaces.
  - **Utility/footer** (bottom, divider-separated): Settings, About — chrome
    and metadata, not part of the dictation/history workflow.

  ## Two states (#494)

  - **Open (default for fresh installs).** ~180 px wide, icon +
    horizontal label per item. Labels are visible without a
    hover, so a first-run user can tell Dictation / History /
    Settings apart at a glance — the discoverability concern
    that #494 filed. Panic-style left-chrome accent on the
    active row.
  - **Collapsed.** 56 px icon-only column with native
    `title=` tooltips. For users who want screen real estate
    over labels (the pre-#494 shape).

  A chevron toggle lives at the top of the sidebar; clicking
  flips the open state, fires the `onToggle` callback so the
  orchestrator can persist the choice (`localStorage["hush.sidebar.open"]`),
  and keeps focus on the toggle for keyboard ergonomics.
-->
<script lang="ts">
  import { slide } from "svelte/transition";
  import { SETTINGS_TABS } from "$lib/settings-tabs";
  import type { SettingsTab } from "$lib/settings-tabs";
  import { motionDuration } from "$lib/motion";

  export type SidebarSection = "dictation" | "history" | "settings" | "about";

  type Item = {
    id: SidebarSection;
    label: string;
    /// `recording` flag bumps the Dictation icon to a pulsing
    /// recording-red dot overlay, so the sidebar reads as "you're
    /// live" even when the user has navigated away to History.
    showRecordingDot?: boolean;
  };

  type Props = {
    active: SidebarSection;
    /// True while the dictation flow is recording. Shows a pulsing
    /// dot on the Dictation icon regardless of which panel is
    /// active.
    recording: boolean;
    /// True when the sidebar shows labels (open/expanded state),
    /// false when icon-only (collapsed). The orchestrator owns
    /// the persisted preference via `localStorage["hush.sidebar.open"]`
    /// and wires it through this prop.
    open: boolean;
    /// The currently active settings sub-tab. Drives the accordion
    /// highlight when the settings section is active.
    settingsTab: SettingsTab;
    /// Whether the Debug tab should appear in the settings accordion.
    showDebugTab: boolean;
    /// Called when the user clicks an item OR activates one with
    /// keyboard.
    onSelect: (id: SidebarSection) => void | Promise<void>;
    /// Called when the user clicks the open/collapse toggle. The
    /// orchestrator flips `open` and persists.
    onToggle: () => void | Promise<void>;
    /// Called when the user selects a settings sub-tab in the accordion.
    onSettingsTabSelect: (tab: SettingsTab) => void;
  };

  let { active = $bindable(), recording, open, settingsTab, showDebugTab, onSelect, onToggle, onSettingsTabSelect }: Props =
    $props();

  let primaryItems = $derived<Item[]>([
    { id: "dictation", label: "Transcribe", showRecordingDot: recording },
    { id: "history", label: "History" },
  ]);

  let visibleSettingsTabs = $derived(
    showDebugTab ? SETTINGS_TABS : SETTINGS_TABS.filter((t) => t.key !== "debug"),
  );

  function handleClick(id: SidebarSection) {
    void onSelect(id);
  }

  function handleToggle() {
    void onToggle();
  }
</script>

<nav
  class="sidebar-nav"
  class:open
  class:collapsed={!open}
  aria-label="Main navigation"
  id="sidebar-nav-region"
>
  <!-- Toggle button. The chevron points right when collapsed
       (suggesting "expand toward the content"), left when open
       (suggesting "collapse back into the chrome"). aria-expanded
       reflects current state; aria-controls points at the nav
       region so AT can map the button to what it controls. -->
  <button
    type="button"
    class="sidebar-nav-toggle"
    aria-label={open ? "Collapse sidebar" : "Expand sidebar"}
    aria-expanded={open}
    aria-controls="sidebar-nav-region"
    title={open ? "Collapse sidebar" : "Expand sidebar"}
    data-testid="sidebar-nav-toggle"
    onclick={handleToggle}
  >
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      {#if open}
        <polyline points="15 18 9 12 15 6" />
      {:else}
        <polyline points="9 18 15 12 9 6" />
      {/if}
    </svg>
  </button>

  <ul class="sidebar-nav-list">
    {#each primaryItems as item (item.id)}
      <li>
        <button
          type="button"
          class="sidebar-nav-item"
          class:active={active === item.id}
          aria-label={item.label}
          aria-current={active === item.id ? "page" : undefined}
          title={open ? undefined : item.label}
          data-testid="sidebar-nav-{item.id}"
          onclick={() => handleClick(item.id)}
        >
          <span class="sidebar-nav-icon" aria-hidden="true">
            {#if item.id === "dictation"}
              <!-- Microphone -->
              <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
                <rect x="9" y="2" width="6" height="12" rx="3" />
                <path d="M5 11a7 7 0 0 0 14 0" />
                <path d="M12 18v4" />
              </svg>
            {:else if item.id === "history"}
              <!-- Clock with arrow back -->
              <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
                <path d="M3 12a9 9 0 1 0 3-6.7" />
                <path d="M3 4v5h5" />
                <path d="M12 7v5l3 2" />
              </svg>
            {/if}
            {#if item.showRecordingDot}
              <span class="sidebar-nav-recording-dot" aria-hidden="true"></span>
            {/if}
          </span>
          {#if open}
            <span class="sidebar-nav-label">{item.label}</span>
          {/if}
        </button>
      </li>
    {/each}
  </ul>

  <ul class="sidebar-nav-footer">
    <!-- Settings with accordion sub-navigation -->
    <li>
      <button
        type="button"
        class="sidebar-nav-item"
        class:active={active === "settings"}
        aria-label="Settings"
        aria-current={active === "settings" ? "page" : undefined}
        aria-expanded={active === "settings" && open}
        aria-controls="sidebar-settings-tabs"
        title={open ? undefined : "Settings"}
        data-testid="sidebar-nav-settings"
        onclick={() => handleClick("settings")}
      >
        <span class="sidebar-nav-icon" aria-hidden="true">
          <!-- Settings gear -->
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z" />
            <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
          </svg>
        </span>
        {#if open}
          <span class="sidebar-nav-label">Settings</span>
        {/if}
      </button>

      {#if active === "settings" && open}
        <ul
          id="sidebar-settings-tabs"
          class="sidebar-settings-tabs"
          aria-label="Settings sections"
          transition:slide={{ duration: motionDuration(160) }}
        >
          {#each visibleSettingsTabs as tab (tab.key)}
            <li>
              <button
                type="button"
                class="sidebar-settings-tab-btn"
                class:active={settingsTab === tab.key}
                aria-current={settingsTab === tab.key ? "page" : undefined}
                data-testid={tab.testId}
                onclick={() => onSettingsTabSelect(tab.key)}
              >{tab.label}</button>
            </li>
          {/each}
        </ul>
      {/if}
    </li>

    <!-- About -->
    <li>
      <button
        type="button"
        class="sidebar-nav-item"
        class:active={active === "about"}
        aria-label="About"
        aria-current={active === "about" ? "page" : undefined}
        title={open ? undefined : "About"}
        data-testid="sidebar-nav-about"
        onclick={() => handleClick("about")}
      >
        <span class="sidebar-nav-icon" aria-hidden="true">
          <!-- Info circle -->
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="8" x2="12" y2="8" stroke-width="2.5" stroke-linecap="round" />
            <path d="M11 12h1v5h1" />
          </svg>
        </span>
        {#if open}
          <span class="sidebar-nav-label">About</span>
        {/if}
      </button>
    </li>
  </ul>
</nav>

<style>
  .sidebar-nav {
    flex-shrink: 0;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--bg-sidebar-border);
    padding: 0.6rem 0;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    /* Animate width changes so the open/collapsed transition
       reads as a smooth state flip rather than a jarring jump.
       Reduced-motion users get the instant transition via the
       media query at the bottom. */
    transition: width 160ms ease;
  }
  .sidebar-nav.open {
    width: 180px;
  }
  .sidebar-nav.collapsed {
    width: 56px;
  }

  /* Toggle button. Sits at the top of the column, slightly
     offset from the edge so it doesn't fight the left-chrome
     accent on active items. Quieter visual weight than the
     nav items themselves — it's chrome, not navigation. */
  .sidebar-nav-toggle {
    appearance: none;
    background: transparent;
    border: none;
    margin: 0 0.25rem 0.4rem auto;
    padding: 0.35rem 0.5rem;
    color: var(--text-muted);
    cursor: pointer;
    border-radius: 6px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: color 120ms ease, background-color 120ms ease;
  }
  .sidebar-nav-toggle:hover {
    color: var(--text-primary);
    background-color: var(--accent-blue-subtle);
  }
  .sidebar-nav-toggle:focus-visible {
    outline: 2px solid var(--accent-blue);
    outline-offset: 1px;
  }
  /* When collapsed, the toggle sits centred below the items
     (no horizontal label to align to). Override the open-state
     auto-margin. */
  .sidebar-nav.collapsed .sidebar-nav-toggle {
    margin: 0 auto 0.4rem;
  }

  .sidebar-nav-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  /* Utility items (Settings, About) pushed to the bottom of the
     column with a subtle top divider to signal they're secondary
     to the core workflow items above. */
  .sidebar-nav-footer {
    list-style: none;
    margin: auto 0 0;
    padding: 0.5rem 0 0;
    border-top: 1px solid var(--border-subtle);
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  /* Icon button — left-chrome accent indicator on active per the
     Panic addendum on #468 (no background fill, just a 3 px border
     on the leading edge). Reserves the chrome lane on every item
     by using a transparent border so the active state doesn't
     reflow content. */
  .sidebar-nav-item {
    appearance: none;
    background: transparent;
    border: none;
    border-left: 3px solid transparent;
    margin: 0;
    padding: 0.55rem 0;
    width: 100%;
    display: flex;
    align-items: center;
    color: var(--text-muted);
    cursor: pointer;
    transition: color 120ms ease, border-color 120ms ease;
  }
  .sidebar-nav-item:hover {
    color: var(--text-primary);
  }
  .sidebar-nav-item:focus-visible {
    outline: 2px solid var(--accent-blue);
    outline-offset: -3px;
  }
  .sidebar-nav-item.active {
    color: var(--accent-blue);
    border-left-color: var(--accent-blue);
  }
  /* Layout per state. Collapsed: icon centred. Open: icon +
     label horizontally, icon left-aligned with consistent
     gutter so labels start at the same x across rows. */
  .sidebar-nav.collapsed .sidebar-nav-item {
    justify-content: center;
  }
  .sidebar-nav.open .sidebar-nav-item {
    justify-content: flex-start;
    padding-left: 1rem;
    gap: 0.7rem;
  }

  .sidebar-nav-icon {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    flex-shrink: 0;
  }

  .sidebar-nav-label {
    font-size: 0.85rem;
    font-weight: 500;
    line-height: 1;
    color: inherit;
    user-select: none;
    white-space: nowrap;
  }

  /* Recording-state dot on the Dictation icon — even when the
     user is on the History panel, the pulse signals "you're
     live." 8 px filled circle in the danger token, positioned in
     the icon's top-right corner. */
  .sidebar-nav-recording-dot {
    position: absolute;
    top: -2px;
    right: -3px;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--danger);
    box-shadow: 0 0 0 2px var(--bg-sidebar);
    animation: sidebar-recording-pulse 1.4s ease-in-out infinite;
  }

  @keyframes sidebar-recording-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }

  .sidebar-settings-tabs {
    list-style: none;
    margin: 0;
    padding: 0 0 0.25rem;
    overflow: hidden;
  }

  /* Settings sub-tab buttons — indented under the gear icon so the
     visual hierarchy is clear: gear is the parent, tabs are children. */
  .sidebar-settings-tab-btn {
    appearance: none;
    background: transparent;
    border: none;
    border-left: 3px solid transparent;
    padding: 0.35rem 0.75rem 0.35rem 2.3rem;
    width: 100%;
    text-align: left;
    font-size: 0.8rem;
    font-family: inherit;
    font-weight: 400;
    color: var(--text-muted);
    cursor: pointer;
    white-space: nowrap;
    transition: color 120ms ease, border-color 120ms ease;
  }
  .sidebar-settings-tab-btn:hover {
    color: var(--text-primary);
  }
  .sidebar-settings-tab-btn.active {
    color: var(--accent);
    border-left-color: var(--accent);
    font-weight: 500;
  }
  .sidebar-settings-tab-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -3px;
  }

  @media (prefers-reduced-motion: reduce) {
    .sidebar-nav,
    .sidebar-nav-toggle,
    .sidebar-nav-item,
    .sidebar-nav-recording-dot {
      transition: none;
      animation: none;
    }
  }
</style>
