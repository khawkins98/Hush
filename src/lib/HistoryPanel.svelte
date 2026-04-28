<script lang="ts">
  import type { HistoryEntry } from "./types";

  type Props = {
    historyEntries: HistoryEntry[];
    historyLoaded: boolean;
    historyQuery: string;
    historySearching: boolean;
    historyError: string | null;
    historyVersion: number;
    /// Unfiltered total — drives the "Clear all 7" confirmation
    /// copy. Different from `historyEntries.length` when the user
    /// has a search query active.
    historyTotalCount: number;
    formatTimestamp: (iso: string) => string;
    onSearchInput: (e: Event) => void;
    onCopy: (entry: HistoryEntry) => void | Promise<void>;
    onDelete: (entry: HistoryEntry) => void | Promise<void>;
    /// Wipes every history row. The panel handles the click-to-
    /// confirm dance in-component so the parent doesn't have to
    /// thread a confirming-flag through props.
    onClearAll: () => void | Promise<void>;
  };

  let {
    historyEntries,
    historyLoaded,
    historyQuery,
    historySearching,
    historyError,
    historyVersion,
    historyTotalCount,
    formatTimestamp,
    onSearchInput,
    onCopy,
    onDelete,
    onClearAll,
  }: Props = $props();

  // Click-to-confirm state for the "Clear all" button. Same shape
  // as the meeting-mode Stop session confirmation: first click
  // reveals the danger-styled confirm; second click within the
  // timeout fires the clear; the prompt auto-resets after ~5 s so
  // a stale armed state can't catch the user later.
  let clearConfirming = $state(false);
  let clearTimer: number | undefined;

  function startClear() {
    if (historyTotalCount === 0) return;
    if (!clearConfirming) {
      clearConfirming = true;
      window.clearTimeout(clearTimer);
      clearTimer = window.setTimeout(() => {
        clearConfirming = false;
      }, 5000);
      return;
    }
    // Second click — fire and reset.
    window.clearTimeout(clearTimer);
    clearConfirming = false;
    void onClearAll();
  }

  function cancelClear() {
    window.clearTimeout(clearTimer);
    clearConfirming = false;
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

<section class="history panel-history" aria-labelledby="history-heading">
  <header class="history-header">
    <h2 id="history-heading">
      <span class="panel-tag" aria-hidden="true">H</span>
      History
    </h2>
    <div class="header-actions">
      <div class="search-wrap">
        <input
          type="search"
          placeholder="Search transcriptions…"
          value={historyQuery}
          oninput={onSearchInput}
          aria-label="Search history"
        />
        {#if historySearching}
          <span class="search-spinner" aria-label="Searching" role="status"></span>
        {/if}
      </div>
      {#if historyTotalCount > 0}
        {#if clearConfirming}
          <div class="clear-confirm" role="group" aria-label="Confirm clear history">
            <span class="clear-confirm-text">
              Delete all {historyTotalCount}?
            </span>
            <button
              type="button"
              class="ghost danger clear-confirm-yes"
              onclick={startClear}
              data-testid="history-clear-confirm"
            >
              Yes, clear
            </button>
            <button
              type="button"
              class="ghost"
              onclick={cancelClear}
              data-testid="history-clear-cancel"
            >
              Cancel
            </button>
          </div>
        {:else}
          <button
            type="button"
            class="ghost danger"
            onclick={startClear}
            aria-label="Clear all transcripts"
            data-testid="history-clear-all"
          >
            Clear all
          </button>
        {/if}
      {/if}
    </div>
  </header>

  {#if historyError}
    <p class="error scoped-error" role="alert">
      <strong>History:</strong>
      {historyError}
    </p>
  {/if}

  {#if !historyLoaded}
    <p class="loading-skeleton">Loading history…</p>
  {:else if historyEntries.length === 0}
    <p class="empty-history">
      {#if historyQuery.trim().length > 0}
        No matches for "<em>{historyQuery}</em>". Try a shorter query.
      {:else}
        No transcriptions yet. Switch to the Dictation tab and
        press the toggle hotkey or the Start button — the
        transcript will land here.
      {/if}
    </p>
  {:else}
    <ul class="history-list" data-version={historyVersion}>
      {#each historyEntries as entry (entry.id)}
        <li class="history-row">
          <p class="history-text">{entry.transcript}</p>
          <p class="history-meta">
            {formatTimestamp(entry.createdAt)}
            {#if formatDuration(entry.durationMs)}· {formatDuration(entry.durationMs)}{/if}
            {#if entry.appName}· {entry.appName}{/if}
            {#if entry.model}· {entry.model}{/if}
          </p>
          <div class="history-actions">
            <button class="ghost" onclick={() => onCopy(entry)}>
              Copy
            </button>
            <button class="ghost danger" onclick={() => onDelete(entry)}>
              Delete
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.history {
  margin-top: 2.5rem;
  text-align: left;
}

.history-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header h2 {
  margin: 0;
  font-size: 1.1rem;
  font-weight: 600;
  color: #333;
}

.history-header input[type="search"] {
  flex: 1;
  max-width: 18rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.clear-confirm {
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  padding: 0.3rem 0.6rem;
  background-color: #fdf3f3;
  border: 1px solid #e8c4c4;
  border-radius: 8px;
  font-size: 0.85rem;
}
.clear-confirm-text {
  color: #8a3030;
  font-weight: 500;
}
.clear-confirm-yes {
  /* Slightly emphasised — same `danger` palette but opaque so the
     primary action reads first. The Cancel button below it stays
     in the default ghost-danger styling. */
  background-color: #fbeaea;
  border-color: #d83a3a;
  color: #8a0000;
}

@media (prefers-color-scheme: dark) {
  .clear-confirm {
    background-color: #2c1818;
    border-color: #4a2020;
  }
  .clear-confirm-text {
    color: #ff9090;
  }
  .clear-confirm-yes {
    background-color: #3a1818;
    border-color: #d83a3a;
    color: #ff9090;
  }
}

.history-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}

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

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
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

.empty-history {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: #666;
  font-size: 0.9rem;
  text-align: center;
}

.panel-history {
  margin-top: 2.5rem;
  text-align: left;
  border-left: 3px solid #c0c0c0;
  padding-left: 1rem;
  padding-bottom: 0.25rem;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.4em;
  height: 1.4em;
  border-radius: 5px;
  font-size: 0.75em;
  font-weight: 700;
  background-color: #e8e8e8;
  color: #444;
  margin-right: 0.5rem;
}

.error {
  margin-top: 1.5rem;
  padding: 0.75rem 1rem;
  background-color: #fee;
  border: 1px solid #d83a3a;
  border-radius: 8px;
  color: #8a0000;
  text-align: left;
  line-height: 1.5;
}

.scoped-error {
  /* `.error` already provides the red box; `strong` inside scopes
     the message to a section. */
  padding-left: 1rem;
}
.scoped-error strong {
  margin-right: 0.4rem;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: #fafafa;
  border-radius: 6px;
  color: #999;
  font-size: 0.9rem;
  text-align: center;
  font-style: italic;
}

.search-wrap {
  position: relative;
  display: flex;
  align-items: center;
  gap: 0.4rem;
}

.search-spinner {
  width: 0.7rem;
  height: 0.7rem;
  border: 2px solid #b0b0b0;
  border-right-color: transparent;
  border-radius: 50%;
  display: inline-block;
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

@media (prefers-reduced-motion: reduce) {
  .search-spinner {
    animation: none;
  }
}

@media (prefers-color-scheme: dark) {
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  .history-header h2 {
    color: #d8d8d8;
  }
  .history-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .history-meta {
    color: #9a9a9a;
  }
  button.ghost {
    border-color: #3a3a3a;
    color: #f0f0f0;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  button.ghost.danger {
    color: #ff9090;
    border-color: #5a2020;
  }
  button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: #d83a3a;
  }
  .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
  .error {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
}
</style>
