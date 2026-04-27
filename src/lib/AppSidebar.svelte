<!--
  Left-rail navigation for the main window. Phase 1 of the IA
  redesign (UX brief 2026-04-27): splits the main window's flat
  panel stack into Dictation / Meetings / History sections, keeps
  not-yet-moved configuration panels under a temporary
  "Configuration" tab until Phase 3 lifts them into the standalone
  Settings window.

  Why a sibling component rather than inline markup: the parent
  page is already 1.4k LOC (#156). Pulling the sidebar out keeps
  the new layout legible in `+page.svelte` and makes the eventual
  cleanup PR (drop "Configuration" tab once Settings ships) a
  one-line diff in this file.
-->
<script lang="ts">
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

  // Section definitions. Order matches the brief's recommendation
  // (hot path first). Keys are stable test ids; labels are
  // user-facing copy.
  const sections: Array<{ key: AppSection; label: string; testId: string }> = [
    { key: "dictation", label: "Dictation", testId: "nav-dictation" },
    { key: "meetings", label: "Meetings", testId: "nav-meetings" },
    { key: "history", label: "History", testId: "nav-history" },
    { key: "configuration", label: "Configuration", testId: "nav-configuration" },
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
    <span class="brand-mark" aria-hidden="true">H</span>
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
  .brand-mark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 6px;
    background-color: #2c3e8f;
    color: white;
    font-weight: 700;
    font-size: 0.85rem;
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
    outline: 2px solid #6a8cf0;
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
  }
</style>
