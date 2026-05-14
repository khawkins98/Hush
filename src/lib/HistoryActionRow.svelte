<!--
  Shared icon-action cluster for history rows (#686).

  Renders the right-aligned buttons that are identical between
  HistoryDictationRow and HistoryMeetingRow: an optional expand
  chevron, optional Copy, optional Export popover, and always-present
  Delete with click-to-confirm. All buttons are styled here so neither
  row file needs to duplicate the icon-btn / export-popover CSS.

  The export popover is data-driven (ExportMenuEntry[]) rather than
  snippet-based so the buttons rendered inside the popover receive this
  component's scoped CSS — Svelte 5 scopes snippet content to the
  defining component, not the rendering one.
-->
<script lang="ts">
  export type ExportMenuEntry =
    | { kind: "separator" }
    | {
        kind: "item";
        label: string;
        onSelect: () => void | Promise<void>;
        testId?: string;
      };

  export type ExpandAction = {
    expanded: boolean;
    onClick: () => void;
    title: string;
    ariaLabel: string;
    testId: string;
  };

  type Props = {
    // Optional expand/collapse button rendered before Copy (meeting row).
    expandAction?: ExpandAction;

    // Copy button — hidden when undefined.
    onCopy?: () => void | Promise<void>;
    copyPending?: boolean;
    copyTitle?: string;
    copyAriaLabel?: string;
    copyTestId?: string;

    // Export popover — hidden when undefined or empty.
    exportItems?: ExportMenuEntry[];
    exportTitle?: string;
    exportAriaLabel?: string;
    exportTestId?: string;

    // Delete — always shown.
    confirming: boolean;
    onDelete: () => void;
    deleteTitle?: string;
    confirmTitle?: string;
    deleteAriaLabel?: string;
    confirmAriaLabel?: string;
    deleteTestId?: string;
  };

  let {
    expandAction,
    onCopy,
    copyPending = false,
    copyTitle = "Copy",
    copyAriaLabel = "Copy",
    copyTestId,
    exportItems,
    exportTitle = "Export",
    exportAriaLabel = "Export",
    exportTestId,
    confirming,
    onDelete,
    deleteTitle = "Delete",
    confirmTitle = "Click again to confirm delete",
    deleteAriaLabel = "Delete",
    confirmAriaLabel = "Click again to confirm delete",
    deleteTestId,
  }: Props = $props();

  let exportOpen = $state(false);

  function selectExportItem(item: Extract<ExportMenuEntry, { kind: "item" }>) {
    exportOpen = false;
    void item.onSelect();
  }
</script>

<div class="history-actions" role="group" aria-label="Row actions">
  {#if expandAction}
    <button
      type="button"
      class="icon-btn"
      class:expanded={expandAction.expanded}
      onclick={expandAction.onClick}
      aria-expanded={expandAction.expanded}
      title={expandAction.title}
      aria-label={expandAction.ariaLabel}
      data-testid={expandAction.testId}
    >
      <!-- Chevron: rotates 180° when expanded via CSS -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <polyline points="6 9 12 15 18 9"/>
      </svg>
    </button>
  {/if}

  {#if onCopy}
    <button
      type="button"
      class="icon-btn"
      disabled={copyPending}
      onclick={onCopy}
      title={copyTitle}
      aria-label={copyAriaLabel}
      data-testid={copyTestId}
    >
      <!-- Lucide Copy -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
      </svg>
    </button>
  {/if}

  {#if exportItems?.length}
    <div class="export-popover">
      <button
        type="button"
        class="icon-btn"
        onclick={() => (exportOpen = !exportOpen)}
        aria-haspopup="menu"
        aria-expanded={exportOpen}
        title={exportTitle}
        aria-label={exportAriaLabel}
        data-testid={exportTestId}
      >
        <!-- Lucide Download -->
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/>
          <polyline points="7 10 12 15 17 10"/>
          <line x1="12" y1="15" x2="12" y2="3"/>
        </svg>
      </button>
      {#if exportOpen}
        <ul class="export-menu" role="menu">
          {#each exportItems as entry}
            {#if entry.kind === "separator"}
              <li role="separator" class="export-menu-separator"></li>
            {:else}
              <li>
                <button
                  type="button"
                  role="menuitem"
                  class="export-menu-item"
                  onclick={() => selectExportItem(entry)}
                  data-testid={entry.testId}
                >
                  {entry.label}
                </button>
              </li>
            {/if}
          {/each}
        </ul>
      {/if}
    </div>
  {/if}

  <button
    type="button"
    class="icon-btn danger"
    class:confirming
    title={confirming ? confirmTitle : deleteTitle}
    onclick={onDelete}
    aria-label={confirming ? confirmAriaLabel : deleteAriaLabel}
    data-testid={deleteTestId}
  >
    {#if confirming}
      <!-- X mark: second click will fire -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <line x1="18" y1="6" x2="6" y2="18"/>
        <line x1="6" y1="6" x2="18" y2="18"/>
      </svg>
    {:else}
      <!-- Lucide Trash2 -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <polyline points="3 6 5 6 21 6"/>
        <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/>
        <line x1="10" y1="11" x2="10" y2="17"/>
        <line x1="14" y1="11" x2="14" y2="17"/>
      </svg>
    {/if}
  </button>
</div>

<style>
  .history-actions {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    flex-shrink: 0;
    /* Align icon cluster with the first line of content */
    padding-top: 0.1rem;
  }

  .icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: none;
    border-radius: 6px;
    background: transparent;
    cursor: pointer;
    color: var(--text-muted);
    transition: background-color 0.12s, color 0.12s, transform 0.15s;
  }
  .icon-btn:hover:not(:disabled) {
    background-color: var(--bg-app);
    color: var(--text-primary);
  }
  .icon-btn:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  /* Chevron rotates when transcript is expanded */
  .icon-btn.expanded svg {
    transform: rotate(180deg);
  }
  .icon-btn.danger {
    color: var(--danger);
  }
  .icon-btn.danger:hover:not(:disabled) {
    background-color: var(--danger-bg);
  }
  .icon-btn.danger.confirming {
    background-color: var(--danger-bg);
    color: var(--danger);
  }

  .export-popover {
    position: relative;
    display: inline-block;
  }
  .export-menu {
    position: absolute;
    top: calc(100% + 0.25rem);
    right: 0;
    z-index: 5;
    list-style: none;
    margin: 0;
    padding: 0.25rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border-input);
    border-radius: 8px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.08);
    min-width: 11rem;
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .export-menu-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 0.4rem 0.7rem;
    background-color: transparent;
    border: none;
    border-radius: 6px;
    font-size: 0.85rem;
    font-family: inherit;
    color: var(--text-primary);
    cursor: pointer;
  }
  .export-menu-item:hover {
    background-color: var(--bg-app);
  }
  .export-menu-separator {
    height: 1px;
    background-color: var(--border);
    margin: 0.2rem 0.25rem;
  }

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .export-menu {
      box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
    }
    :root:not([data-theme="light"]) .icon-btn { color: #6e6e73; }
    :root:not([data-theme="light"]) .icon-btn:hover:not(:disabled) {
      background-color: #2a2a2d;
      color: #d8d8d8;
    }
    :root:not([data-theme="light"]) .icon-btn.danger { color: #f0a0a0; }
    :root:not([data-theme="light"]) .icon-btn.danger:hover:not(:disabled) {
      background-color: #3d1d1d;
    }
    :root:not([data-theme="light"]) .icon-btn.danger.confirming {
      background-color: #3d1d1d;
      color: #f0c0c0;
    }
  }
  :root[data-theme="dark"] .export-menu {
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }
  :root[data-theme="dark"] .icon-btn { color: #6e6e73; }
  :root[data-theme="dark"] .icon-btn:hover:not(:disabled) {
    background-color: #2a2a2d;
    color: #d8d8d8;
  }
  :root[data-theme="dark"] .icon-btn.danger { color: #f0a0a0; }
  :root[data-theme="dark"] .icon-btn.danger:hover:not(:disabled) {
    background-color: #3d1d1d;
  }
  :root[data-theme="dark"] .icon-btn.danger.confirming {
    background-color: #3d1d1d;
    color: #f0c0c0;
  }
</style>
