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
  };

  let { entry, confirming, models, formatTimestamp, onCopy, onDelete }: Props =
    $props();

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
    background-color: white;
    border: 1px solid #e1e1e1;
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
    color: #6b6b6b;
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
    border: 1px solid #d1d1d1;
    border-radius: 8px;
    cursor: pointer;
    font-family: inherit;
    color: #0f0f0f;
    transition: border-color 0.15s, background-color 0.15s;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #f0f0f0;
  }
  button.ghost.danger {
    color: #b03030;
    border-color: #e1b8b8;
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: #fbeaea;
    border-color: #d83a3a;
  }
  button.ghost.danger.confirming {
    background-color: #fbeaea;
    border-color: #d83a3a;
    color: #8a0000;
  }

  @media (prefers-color-scheme: dark) {
    .history-row {
      background-color: #1f1f22;
      border-color: #2f2f33;
    }
    .history-text { color: #e8e8e8; }
    .history-meta { color: #9a9aa0; }
    button.ghost {
      color: #d8d8d8;
      border-color: #38383b;
    }
    button.ghost:hover:not(:disabled) {
      background-color: #2a2a2d;
    }
    button.ghost.danger {
      color: #f0a0a0;
      border-color: #5a3a3a;
    }
    button.ghost.danger:hover:not(:disabled) {
      background-color: #3d1d1d;
    }
    button.ghost.danger.confirming {
      background-color: #3d1d1d;
      border-color: #d83a3a;
      color: #f0c0c0;
    }
  }
</style>
