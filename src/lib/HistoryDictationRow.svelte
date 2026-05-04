<!--
  Card for a single dictation transcript inside the unified
  History feed (#357 phase 2). Extracted from `HistoryPanel.svelte`
  so the meeting-row component can sit alongside it without the
  panel growing unwieldy.

  Affordances are unchanged from the inline version:
    - Copy: writes the transcript to clipboard via the parent's
      handler (uses the shared sound-cue / toast plumbing).
    - Delete: click-to-confirm in two beats, identical 5 s
      auto-reset window the panel uses elsewhere.

  The confirmation state lives in the parent `HistoryPanel` so a
  click on a different row's Delete resets the previous arm — that
  cross-row coordination would be awkward to do per-row component.
  We just take `confirming` as a prop here.
-->
<script lang="ts">
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

<li class="history-row" data-kind="dictation">
  <p class="history-text">{entry.transcript}</p>
  <p class="history-meta">
    {formatTimestamp(entry.createdAt)}
    {#if formatDuration(entry.durationMs)}· {formatDuration(entry.durationMs)}{/if}
    {#if entry.appName}· {entry.appName}{/if}
    {#if entry.model}· {displayModelName(entry.model)}{/if}
  </p>
  <div class="history-actions">
    <button class="ghost" onclick={() => onCopy(entry)}>Copy</button>
    {#if onExportCsv}
      <button
        class="ghost"
        onclick={() => onExportCsv?.(entry)}
        data-testid="history-export-{entry.id}"
        aria-label="Export this transcript as CSV"
      >
        Export CSV
      </button>
    {/if}
    <button
      class="ghost danger"
      class:confirming
      onclick={() => onDelete(entry)}
      aria-label={confirming
        ? "Click again to confirm deleting this transcript"
        : "Delete this transcript"}
      data-testid="history-delete-{entry.id}"
    >
      {confirming ? "Click to confirm" : "Delete"}
    </button>
  </div>
</li>

<style>
  .history-row {
    padding: 0.75rem 1rem;
    background-color: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 8px;
  }

  .history-text {
    margin: 0 0 0.35rem;
    font-size: 0.95rem;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .history-meta {
    margin: 0 0 0.5rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }

  .history-actions {
    display: flex;
    gap: 0.4rem;
  }

  button.ghost {
    padding: 0.3em 0.75em;
    font-size: 0.8rem;
    font-weight: 500;
    background-color: transparent;
    border: 1px solid var(--border-input);
    border-radius: 8px;
    cursor: pointer;
    font-family: inherit;
    color: var(--text-primary);
    transition: border-color 0.15s, background-color 0.15s;
  }
  button.ghost:hover:not(:disabled) {
    background-color: var(--bg-app);
  }
  button.ghost.danger {
    color: var(--danger);
    border-color: var(--danger-border);
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: var(--danger-bg);
    border-color: var(--danger);
  }
  button.ghost.danger.confirming {
    background-color: var(--danger-bg);
    border-color: var(--danger);
    color: #8a0000;
  }

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .history-meta { color: #9a9aa0; }
    :root:not([data-theme="light"]) button.ghost {
      color: #d8d8d8;
      border-color: #38383b;
    }
    :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
      background-color: #2a2a2d;
    }
    :root:not([data-theme="light"]) button.ghost.danger {
      color: #f0a0a0;
      border-color: #5a3a3a;
    }
    :root:not([data-theme="light"]) button.ghost.danger:hover:not(:disabled) {
      background-color: #3d1d1d;
    }
    :root:not([data-theme="light"]) button.ghost.danger.confirming {
      background-color: #3d1d1d;
      border-color: var(--danger);
      color: #f0c0c0;
    }
  }
  :root[data-theme="dark"] .history-meta { color: #9a9aa0; }
  :root[data-theme="dark"] button.ghost {
    color: #d8d8d8;
    border-color: #38383b;
  }
  :root[data-theme="dark"] button.ghost:hover:not(:disabled) {
    background-color: #2a2a2d;
  }
  :root[data-theme="dark"] button.ghost.danger {
    color: #f0a0a0;
    border-color: #5a3a3a;
  }
  :root[data-theme="dark"] button.ghost.danger:hover:not(:disabled) {
    background-color: #3d1d1d;
  }
  :root[data-theme="dark"] button.ghost.danger.confirming {
    background-color: #3d1d1d;
    border-color: var(--danger);
    color: #f0c0c0;
  }
</style>
