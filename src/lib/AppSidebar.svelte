<!--
  Left-rail navigation for the main window. Splits the main
  window's content into Dictation / Meetings / History sections;
  configuration lives in the standalone Settings window opened
  from the footer (or ⌘, on macOS).

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
    meetingsCount: number | null;
    activeMeetingInProgress: boolean;
  };

  let {
    active,
    onSelect,
    historyCount,
    meetingsCount,
    activeMeetingInProgress,
  }: Props = $props();

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
    { key: "meetings", label: "Meetings", testId: "nav-meetings" },
    { key: "history", label: "History", testId: "nav-history" },
  ];

  function badgeFor(key: AppSection): string | null {
    if (key === "history" && historyCount !== null && historyCount > 0) {
      return historyCount > 99 ? "99+" : String(historyCount);
    }
    if (key === "meetings") {
      if (activeMeetingInProgress) return "●";
      if (meetingsCount !== null && meetingsCount > 0) {
        return meetingsCount > 99 ? "99+" : String(meetingsCount);
      }
    }
    return null;
  }
</script>

<nav class="app-sidebar" aria-label="Main navigation">
  <div class="brand">
    <img
      class="brand-icon"
      src="/app-icon.png"
      srcset="/app-icon.png 1x, /app-icon@2x.png 2x"
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
            <span
              class="nav-badge"
              class:nav-badge-live={section.key === "meetings" && activeMeetingInProgress}
              aria-hidden="true"
            >
              {badge}
            </span>
          {/if}
        </button>
      </li>
    {/each}
  </ul>

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
    background-color: #f6f6f8;
    border-right: 1px solid #e1e1e1;
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
    border-bottom: 1px solid #e1e1e1;
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
  .nav-badge-live {
    background-color: #ff4040;
    color: white;
    animation: live-pulse 1.4s ease-in-out infinite;
  }

  .sidebar-footer {
    margin-top: auto;
    padding-top: 0.75rem;
    border-top: 1px solid #e1e1e1;
  }
  .nav-item-secondary {
    color: #666;
  }
  .nav-shortcut {
    font-size: 0.72rem;
    color: #888;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  }
  @keyframes live-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.55; }
  }
  @media (prefers-reduced-motion: reduce) {
    .nav-badge-live { animation: none; }
  }

  @media (prefers-color-scheme: dark) {
    .app-sidebar {
      background-color: #1d1d1f;
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
  }
</style>
