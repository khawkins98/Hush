<!--
  Left icon-column navigation (#479 slice 1).

  Three top-level surfaces — Dictation, History, Settings — each
  represented by a 22 px outlined icon. Clicking an item swaps the
  active panel; the orchestrator owns `active` as a `$bindable`
  state so other inputs (native menu's "View" submenu, ⌘K palette
  Show-History action, deep links from FirstRunModal) can drive
  the same selection.

  Visual treatment per the #468 / #479 reference: 56 px wide,
  icon-only with hover tooltips, accent left-chrome on the active
  item (Panic-style, no background fill). Icons are deliberate
  placeholders inline-SVG until proper icon work lands later.

  Settings is treated as a single top-level item rather than
  exploding the seven existing tabs into the sidebar; the Settings
  panel will keep its own tab strip when it gets inlined in
  slice 2. Slice 1 leaves the Settings window alive — clicking
  the icon opens it the way the existing pre-r3 Settings button
  in the app-bar did.
-->
<script lang="ts">
  export type SidebarSection = "dictation" | "history" | "settings";

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
    /// Called when the user clicks an item OR activates one with
    /// keyboard. The orchestrator decides what to do — for slice
    /// 1 the Settings click opens the standalone window; once the
    /// Settings panel is inlined in slice 2 the same callback just
    /// flips `active`.
    onSelect: (id: SidebarSection) => void | Promise<void>;
  };

  let { active = $bindable(), recording, onSelect }: Props = $props();

  let items = $derived<Item[]>([
    { id: "dictation", label: "Dictation", showRecordingDot: recording },
    { id: "history", label: "History" },
    { id: "settings", label: "Settings" },
  ]);

  function handleClick(id: SidebarSection) {
    void onSelect(id);
  }
</script>

<nav class="sidebar-nav" aria-label="Main navigation">
  <ul class="sidebar-nav-list">
    {#each items as item (item.id)}
      <li>
        <button
          type="button"
          class="sidebar-nav-item"
          class:active={active === item.id}
          aria-label={item.label}
          aria-current={active === item.id ? "page" : undefined}
          title={item.label}
          data-testid="sidebar-nav-{item.id}"
          onclick={() => handleClick(item.id)}
        >
          <span class="sidebar-nav-icon-label-stack">
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
            {:else}
              <!-- Settings gear -->
              <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">
                <path d="M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7z" />
                <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1A1.7 1.7 0 0 0 9 19.4a1.7 1.7 0 0 0-1.9.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.9l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.9.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.9V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
              </svg>
            {/if}
            {#if item.showRecordingDot}
              <span class="sidebar-nav-recording-dot" aria-hidden="true"></span>
            {/if}
          </span>
          <span class="sidebar-nav-label">{item.label}</span>
          </span>
        </button>
      </li>
    {/each}
  </ul>
</nav>

<style>
  .sidebar-nav {
    /* Pre-#494 the column was 56 px (icon-only with native title=
       tooltips). The tooltips had ~500 ms show-delay and zero touch
       support — a first-time user couldn't tell the clock-with-arrow
       apart from "schedule" without hovering. Bumped to 72 px so a
       small label fits under each icon for at-a-glance
       discoverability while keeping the Panic-style left-chrome
       aesthetic. */
    width: 72px;
    flex-shrink: 0;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border-subtle);
    padding: 0.6rem 0;
    display: flex;
    flex-direction: column;
  }

  .sidebar-nav-list {
    list-style: none;
    margin: 0;
    padding: 0;
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
    /* Tighter vertical padding now that each item carries a
       label below the icon — the visual weight of icon+label
       stack is taller than icon-only, so the rhythm needs
       slightly less per-item padding to stay balanced. */
    padding: 0.45rem 0;
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    cursor: pointer;
    transition: color 120ms ease, border-color 120ms ease;
  }
  /* Inner stack: icon on top, label below. Centred horizontally;
     the column-flex layout matches the Panic / Audio Hijack /
     Loop pattern where every item announces its own name. */
  .sidebar-nav-icon-label-stack {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.2rem;
  }
  .sidebar-nav-label {
    font-size: 0.65rem;
    font-weight: 500;
    letter-spacing: 0.01em;
    line-height: 1;
    color: inherit;
    user-select: none;
  }
  .sidebar-nav-item:hover {
    color: var(--text-primary);
  }
  .sidebar-nav-item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -3px;
  }
  .sidebar-nav-item.active {
    color: var(--accent);
    border-left-color: var(--accent);
  }

  .sidebar-nav-icon {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
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

  @media (prefers-reduced-motion: reduce) {
    .sidebar-nav-recording-dot {
      animation: none;
    }
  }
</style>
