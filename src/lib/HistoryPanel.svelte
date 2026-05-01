<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import HistoryDictationRow from "./HistoryDictationRow.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type { HistoryEntry, ModelCard } from "./types";

  type Props = {
    historyEntries: HistoryEntry[];
    historyLoaded: boolean;
    historyQuery: string;
    historySearching: boolean;
    historyError: ErrorDisplayShape | null;
    historyVersion: number;
    /// Unfiltered total — drives the "Clear all 7" confirmation
    /// copy. Different from `historyEntries.length` when the user
    /// has a search query active.
    historyTotalCount: number;
    /// Model catalog. Used to render a friendly display name in
    /// each row's meta line ("Whisper Base") instead of the raw
    /// filename ("ggml-base.bin"); the latter leaks implementation
    /// detail to end users. Empty array is fine — the lookup
    /// falls back to the stored filename.
    models: ModelCard[];
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
    models,
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

  // Per-row Delete also click-to-confirm. Each row arms
  // independently — clicking Delete on a different row resets
  // the previous arm. 5 s auto-reset matches Clear-all's window.
  let confirmingDeleteId = $state<number | null>(null);
  let confirmDeleteTimer: number | undefined;

  function handleRowDelete(entry: HistoryEntry) {
    if (confirmingDeleteId === entry.id) {
      window.clearTimeout(confirmDeleteTimer);
      confirmingDeleteId = null;
      void onDelete(entry);
      return;
    }
    window.clearTimeout(confirmDeleteTimer);
    confirmingDeleteId = entry.id;
    confirmDeleteTimer = window.setTimeout(() => {
      confirmingDeleteId = null;
    }, 5000);
  }

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
    <ErrorDisplay error={historyError} scope="History" />
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
        <HistoryDictationRow
          {entry}
          confirming={confirmingDeleteId === entry.id}
          {models}
          {formatTimestamp}
          {onCopy}
          onDelete={handleRowDelete}
        />
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
  border-color: var(--accent-hover);
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
    border-color: var(--accent);
  }
  .history-header h2 {
    color: #d8d8d8;
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
}
</style>
