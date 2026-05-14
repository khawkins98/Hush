<!--
  Card for a single dictation transcript inside the unified
  History feed (#357 phase 2). Extracted from `HistoryPanel.svelte`
  so the meeting-row component can sit alongside it without the
  panel growing unwieldy.

  Affordances:
    - Copy: writes the transcript to clipboard via the parent's
      handler (uses the shared sound-cue / toast plumbing).
    - Export CSV: optional; hides when no handler is provided.
    - Delete: click-to-confirm in two beats, identical 5 s
      auto-reset window the panel uses elsewhere.

  Action buttons are icon-only and always visible (right-aligned),
  eliminating the reserved blank space of the previous hover-reveal
  text-button layout.

  The confirmation state lives in the parent `HistoryPanel` so a
  click on a different row's Delete resets the previous arm — that
  cross-row coordination would be awkward to do per-row component.
  We just take `confirming` as a prop here.
-->
<script lang="ts">
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import {
    EXPORT_FORMAT_LABELS,
    exportAs,
    type ExportFormat,
  } from "./export-formats";
  import type { HistoryEntry, ModelCard } from "./types";

  type Props = {
    entry: HistoryEntry;
    /// True when this row's Delete button is currently armed
    /// (one click already landed; next click confirms).
    confirming: boolean;
    /// Model catalog used to resolve the friendly display name
    /// from the stored GGUF filename. Empty array is fine — the
    /// lookup falls back to the raw filename.
    models: ModelCard[];
    formatTimestamp: (iso: string) => string;
    onCopy: (entry: HistoryEntry) => void | Promise<void>;
    /// Click handler for Delete. The parent's implementation arms
    /// or fires based on the current row's `confirming` state and
    /// resets any other armed row.
    onDelete: (entry: HistoryEntry) => void;
    /// Per-row CSV export (#357 phase 3a). The parent fires the
    /// IPC + drives the OS save dialog; the row just exposes the
    /// affordance. `null` if the parent didn't pass a handler —
    /// the button hides in that case so an embedding without
    /// export support stays clean.
    onExportCsv?: (entry: HistoryEntry) => void | Promise<void>;
  };

  let {
    entry,
    confirming,
    models,
    formatTimestamp,
    onCopy,
    onDelete,
    onExportCsv,
  }: Props = $props();

  let isIgnored = $derived(entry.ignored);
  // Export-format popover: clipboard formats (plain/markdown/srt/vtt)
  // + CSV file-download. Mirrors the format picker in ResultBlock.svelte.
  let exportOpen = $state(false);
  const CLIPBOARD_FORMATS = ["plain", "markdown", "srt", "vtt"] as const satisfies readonly ExportFormat[];

  async function copyAs(format: ExportFormat) {
    exportOpen = false;
    const body = exportAs(format, {
      text: entry.transcript,
      durationMs: entry.durationMs,
      capturedAt: new Date(entry.createdAt),
    });
    try {
      await writeText(body);
    } catch (e) {
      console.warn("[hush] history export-as clipboard write failed", e);
    }
  }

  function handleExportCsv() {
    exportOpen = false;
    void onExportCsv?.(entry);
  }

  function displayModelName(filename: string | null): string | null {
    if (!filename) return null;
    return (
      models.find((m) => m.filename === filename)?.displayName ?? filename
    );
  }

  // Render duration as a compact m:ss / s.s string. Sub-second clips
  // get one decimal so a 0.4s mis-press is visibly different from a
  // 4s real recording. Anything ≥1 minute uses m:ss.
  function formatDuration(ms: number | null): string | null {
    if (ms === null || ms < 0) return null;
    if (ms < 1000) return `${(ms / 1000).toFixed(1)}s`;
    const totalSeconds = Math.round(ms / 1000);
    if (totalSeconds < 60) return `${totalSeconds}s`;
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
  }
</script>

<li class="history-row" class:confirming-active={confirming} data-kind="dictation">
  <div class="row-layout">
    <div class="row-content">
      {#if isIgnored}
        <p class="history-text ignored-note">Recording too short — not transcribed</p>
      {:else}
        <p class="history-text">{entry.transcript}</p>
      {/if}
      <p class="history-meta">
        {formatTimestamp(entry.createdAt)}
        {#if formatDuration(entry.durationMs)}· {formatDuration(entry.durationMs)}{/if}
        {#if entry.appName}· {entry.appName}{/if}
        {#if entry.model}· {displayModelName(entry.model)}{/if}
      </p>
    </div>
    <div class="history-actions" role="group" aria-label="Row actions">
      {#if !isIgnored}
      <button
        class="icon-btn"
        title="Copy transcript"
        onclick={() => onCopy(entry)}
        aria-label="Copy transcript"
      >
        <!-- Lucide Copy -->
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
        </svg>
      </button>
      <div class="export-popover">
          <button
            class="icon-btn"
            title="Export transcript"
            onclick={() => (exportOpen = !exportOpen)}
            aria-haspopup="menu"
            aria-expanded={exportOpen}
            aria-label="Export transcript"
            data-testid="history-export-{entry.id}"
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
              {#each CLIPBOARD_FORMATS as fmt}
                <li>
                  <button
                    type="button"
                    role="menuitem"
                    class="export-menu-item"
                    onclick={() => copyAs(fmt)}
                    data-testid="history-export-{fmt}-{entry.id}"
                  >
                    {EXPORT_FORMAT_LABELS[fmt]}
                  </button>
                </li>
              {/each}
              {#if onExportCsv}
                <li role="separator" class="export-menu-separator"></li>
                <li>
                  <button
                    type="button"
                    role="menuitem"
                    class="export-menu-item"
                    onclick={handleExportCsv}
                    data-testid="history-export-csv-{entry.id}"
                  >
                    Export CSV…
                  </button>
                </li>
              {/if}
            </ul>
          {/if}
        </div>
      {/if}
      <button
        class="icon-btn danger"
        class:confirming
        title={confirming ? "Click again to confirm delete" : "Delete transcript"}
        onclick={() => onDelete(entry)}
        aria-label={confirming
          ? "Click again to confirm deleting this transcript"
          : "Delete this transcript"}
        data-testid="history-delete-{entry.id}"
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
  </div>
</li>

<style>
  .history-row {
    padding: 0.65rem 1rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }

  .row-layout {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
  }

  .row-content {
    flex: 1;
    min-width: 0;
  }

  .history-text {
    margin: 0 0 0.25rem;
    font-size: 0.95rem;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .ignored-note {
    color: var(--text-muted);
    font-style: italic;
  }

  .history-meta {
    margin: 0;
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .history-actions {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    flex-shrink: 0;
    /* Align icon cluster with the first line of text */
    padding-top: 0.1rem;
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
    min-width: 10rem;
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
    transition: background-color 0.12s, color 0.12s;
  }
  .icon-btn:hover:not(:disabled) {
    background-color: var(--bg-app);
    color: var(--text-primary);
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

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .history-meta { color: #9a9aa0; }
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
  :root[data-theme="dark"] .history-meta { color: #9a9aa0; }
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
