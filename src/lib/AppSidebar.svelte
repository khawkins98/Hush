<!--
  Left-rail navigation for the main window. Splits the main
  window's content into Dictation / History sections; configuration
  lives in the standalone Settings window opened from the footer
  (or ⌘, on macOS).

  Phase 1 of #357 dropped the standalone "Meetings" entry. Meeting
  sessions surface in the History feed once Phase 2 lands; until
  then History still renders dictation rows only.

  Why a sibling component rather than inline markup: the parent
  page is already large (#156). Pulling the sidebar out keeps the
  layout legible in `+page.svelte`.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { AppSection } from "./types";

  type Props = {
    active: AppSection;
    onSelect: (section: AppSection) => void;
    historyCount: number | null;
    // Ambient session state displayed below the nav list. Keeps the
    // sidebar useful at a glance — model + source tell the user what
    // config is active without opening Settings.
    sessionStatus?: {
      modelName: string | null;
      audioSourceName: string | null;
      recording: boolean;
    };
  };

  let { active, onSelect, historyCount, sessionStatus }: Props = $props();

  async function openSettings() {
    try {
      await invoke("open_settings");
    } catch (e) {
      // Best-effort — settings window failing to open is rare and
      // logged backend-side; surface a console warning rather than
      // a noisy banner so the rest of the app stays usable.
      console.warn("[hush] open_settings invoke failed", e);
    }
  }

  // Section definitions. Order matches the brief's recommendation
  // (hot path first). Keys are stable test ids; labels are
  // user-facing copy.
  const sections: Array<{ key: AppSection; label: string; testId: string }> = [
    { key: "dictation", label: "Dictation", testId: "nav-dictation" },
    { key: "history", label: "History", testId: "nav-history" },
  ];

  function badgeFor(key: AppSection): string | null {
    if (key === "history" && historyCount !== null && historyCount > 0) {
      return historyCount > 99 ? "99+" : String(historyCount);
    }
    return null;
  }
</script>

<nav class="app-sidebar" aria-label="Main navigation">
  <div class="brand">
    <!--
      Small-optical-size brand icon (#395 follow-up) —
      microphone glyph at 22 px. See +page.svelte for the full
      rationale.
    -->
    <img
      class="brand-icon"
      src="/app-icon-small.svg"
      alt=""
      aria-hidden="true"
      width="22"
      height="22"
    />
    <span class="brand-name">Hush</span>
  </div>

  <ul class="nav-list">
    {#each sections as section (section.key)}
      {@const badge = badgeFor(section.key)}
      <li>
        <button
          type="button"
          class="nav-item"
          class:active={active === section.key}
          aria-current={active === section.key ? "page" : undefined}
          data-testid={section.testId}
          onclick={() => onSelect(section.key)}
        >
          <span class="nav-label">{section.label}</span>
          {#if badge}
            <span class="nav-badge" aria-hidden="true">
              {badge}
            </span>
          {/if}
        </button>
      </li>
    {/each}
  </ul>

  {#if sessionStatus}
    <div class="session-status" class:recording={sessionStatus.recording}>
      {#if sessionStatus.recording}
        <span class="status-dot" aria-hidden="true"></span>
        <span class="status-label recording-label">Recording</span>
      {:else}
        {#if sessionStatus.modelName}
          <button type="button" class="status-stack" onclick={openSettings} title="Open Settings to change model">
            <span class="status-key">Model</span>
            <span class="status-val">{sessionStatus.modelName}</span>
          </button>
        {/if}
        {#if sessionStatus.audioSourceName}
          <button type="button" class="status-stack" onclick={openSettings} title="Open Settings to change source">
            <span class="status-key">Source</span>
            <span class="status-val">{sessionStatus.audioSourceName}</span>
          </button>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="sidebar-footer">
    <button
      type="button"
      class="nav-item nav-item-secondary"
      data-testid="nav-open-settings"
      onclick={openSettings}
      title="Settings (⌘,)"
    >
      <span class="nav-label">Settings</span>
      <span class="nav-shortcut" aria-hidden="true">⌘,</span>
    </button>
  </div>
</nav>

<style>
  .app-sidebar {
    width: 180px;
    flex-shrink: 0;
    padding: 1.25rem 0.75rem;
    background-color: var(--bg-sidebar, #f0f0f3);
    border-right: 1px solid var(--border, #e1e1e1);
    display: flex;
    flex-direction: column;
    gap: 1rem;
    height: 100vh;
    box-sizing: border-box;
    position: sticky;
    top: 0;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0 0.5rem 0.5rem;
    border-bottom: 1px solid var(--border, #e1e1e1);
  }
  .brand-icon {
    width: 22px;
    height: 22px;
    border-radius: 5px;
    image-rendering: -webkit-optimize-contrast;
    flex-shrink: 0;
  }
  .brand-name {
    font-weight: 600;
    font-size: 1rem;
    letter-spacing: -0.01em;
  }

  .nav-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }

  .nav-item {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    border: none;
    background-color: transparent;
    color: #333;
    font-family: inherit;
    font-size: 0.9rem;
    font-weight: 500;
    text-align: left;
    border-radius: 6px;
    cursor: pointer;
    transition: background-color 0.12s, color 0.12s;
  }
  .nav-item:hover {
    background-color: rgba(44, 62, 143, 0.08);
  }
  .nav-item.active {
    background-color: rgba(44, 62, 143, 0.14);
    color: #2c3e8f;
    font-weight: 600;
  }
  .nav-item:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .nav-badge {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 1.4rem;
    height: 1.25rem;
    padding: 0 0.4rem;
    background-color: rgba(0, 0, 0, 0.08);
    color: #555;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 600;
    line-height: 1;
  }
  .nav-item.active .nav-badge {
    background-color: rgba(44, 62, 143, 0.2);
    color: #2c3e8f;
  }
  .sidebar-footer {
    margin-top: auto;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border, #e1e1e1);
  }
  .nav-item-secondary {
    color: #666;
  }
  .nav-shortcut {
    font-size: 0.72rem;
    color: #888;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  }
  .session-status {
    padding: 0.6rem 0.75rem;
    border-top: 1px solid var(--border, #e1e1e1);
    border-bottom: 1px solid var(--border, #e1e1e1);
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .session-status.recording {
    flex-direction: row;
    align-items: center;
    gap: 0.5rem;
  }

  .status-dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background-color: #d83a3a;
    flex-shrink: 0;
    animation: sidebar-pulse 1.2s ease-in-out infinite;
  }

  @keyframes sidebar-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.45; }
  }

  @media (prefers-reduced-motion: reduce) {
    .status-dot { animation: none; }
  }

  .status-label {
    font-size: 0.8rem;
    font-weight: 600;
  }

  .recording-label {
    color: #d83a3a;
  }

  .status-stack {
    display: flex;
    flex-direction: column;
    gap: 0.05rem;
    min-width: 0;
    width: 100%;
    /* reset button chrome */
    background: none;
    border: none;
    padding: 0.3rem 0.5rem;
    margin: 0 -0.5rem;
    border-radius: 5px;
    cursor: pointer;
    font-family: inherit;
    text-align: left;
    transition: background-color 0.12s;
  }
  .status-stack:hover {
    background-color: rgba(44, 62, 143, 0.08);
  }
  .status-stack:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .status-key {
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: #888;
  }

  .status-val {
    font-size: 0.85rem;
    font-weight: 500;
    color: #333;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  @media (prefers-color-scheme: dark) {
    .app-sidebar {
      border-right-color: #2f2f33;
    }
    .brand {
      border-bottom-color: #2f2f33;
    }
    .brand-name { color: #e8e8e8; }
    .nav-item { color: #d8d8d8; }
    .nav-item:hover { background-color: rgba(150, 170, 240, 0.1); }
    .nav-item.active {
      background-color: rgba(150, 170, 240, 0.18);
      color: #b8c8ff;
    }
    .nav-badge {
      background-color: rgba(255, 255, 255, 0.08);
      color: #b0b0b0;
    }
    .nav-item.active .nav-badge {
      background-color: rgba(150, 170, 240, 0.25);
      color: #d8e0ff;
    }
    .sidebar-footer {
      border-top-color: #2f2f33;
    }
    .nav-item-secondary { color: #a8a8a8; }
    .nav-shortcut { color: #888; }
    .session-status {
      border-color: #2f2f33;
    }
    .status-key { color: #666; }
    .status-val { color: #c0c0c0; }
    .status-stack:hover { background-color: rgba(150, 170, 240, 0.1); }
  }
</style>
