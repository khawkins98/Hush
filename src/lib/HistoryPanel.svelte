<script lang="ts">
  import { onDestroy } from "svelte";
  import { formatTimestamp } from "$lib/format";
  import { dictation } from "$lib/state/dictation.svelte";
  import { history, type FeedRow, type HistoryFilter } from "$lib/state/history.svelte";
  import { meeting } from "$lib/state/meeting-sessions.svelte";

  import ErrorDisplay from "./ErrorDisplay.svelte";
  import ExportOptionsDialog from "./ExportOptionsDialog.svelte";
  import HistoryDictationRow from "./HistoryDictationRow.svelte";
  import HistoryMeetingRow from "./HistoryMeetingRow.svelte";
  import type {
    BundleSelection,
    HistoryEntry,
    MeetingExportFormat,
    MeetingSession,
    MeetingSessionDetail,
  } from "./types";

  type Props = {
    onSearchInput: (e: Event) => void;
    onCopy: (entry: HistoryEntry) => void | Promise<void>;
    onDelete: (entry: HistoryEntry) => void | Promise<void>;
    /// Per-row CSV export for dictation entries (#357 phase 3a).
    /// Optional so embeddings without export support stay clean —
    /// the row hides its Export button when this is `null`.
    onExportDictationCsv?: (entry: HistoryEntry) => void | Promise<void>;
    onMeetingDelete: (session: MeetingSession) => void | Promise<void>;
    /// Resolves the full session detail (utterances + metadata)
    /// when a meeting row is expanded. The row caches the result
    /// locally so a re-toggle is free.
    onMeetingLoadDetail: (id: number) => Promise<MeetingSessionDetail>;
    /// Per-row meeting export (#357 phase 3b). Drives the OS save
    /// picker + the IPC. `null` if the parent didn't pass a
    /// handler — the row hides its Export ▾ button in that case.
    onMeetingExport?: (
      session: MeetingSession,
      format: MeetingExportFormat,
    ) => void | Promise<void>;
    /// Per-row clipboard copy for meeting transcripts. `null` if
    /// the parent didn't pass a handler — the row hides its Copy
    /// button in that case so embeddings without clipboard support
    /// stay clean.
    onMeetingCopy?: (session: MeetingSession) => void | Promise<void>;
    /// Bulk "Export filtered" (#357 phase 3c-1). The panel
    /// surfaces the dialog; the parent fires the OS folder
    /// picker + the IPC with the chosen options + the active
    /// filter. `null` if the parent didn't pass a handler — the
    /// panel hides its Export filtered button in that case.
    onExportBundle?: (args: {
      kind: "auto" | "dictation" | "meetings" | "both";
      meetingFormat: MeetingExportFormat;
    }) => void | Promise<void>;
    /// Wipes every dictation row. Meetings have their own per-row
    /// Delete; bulk meeting delete pends until the export work in
    /// phase 3 ships an Export-filtered + Delete-filtered pair.
    onClearAll: () => void | Promise<void>;
  };

  let {
    onSearchInput,
    onCopy,
    onDelete,
    onExportDictationCsv,
    onMeetingDelete,
    onMeetingLoadDetail,
    onMeetingExport,
    onMeetingCopy,
    onExportBundle,
    onClearAll,
  }: Props = $props();

  /// Whether the bulk-export options dialog is open. Toggled by
  /// the panel header's "Export filtered" button; closes itself
  /// on Cancel / Confirm / Escape.
  let exportDialogOpen = $state(false);

  function onExportConfirm(selection: BundleSelection) {
    exportDialogOpen = false;
    void onExportBundle?.({
      kind: selection.kind,
      meetingFormat: selection.meetingFormat,
    });
  }

  let hasQuery = $derived(history.historyQuery.trim().length > 0);

  // Click-to-confirm state for the "Clear all" button. Same shape
  // as the meeting-mode Stop session confirmation: first click
  // reveals the danger-styled confirm; second click within the
  // timeout fires the clear; the prompt auto-resets after ~5 s so
  // a stale armed state can't catch the user later.
  let clearConfirming = $state(false);
  let clearTimer: number | undefined;

  // Per-row Delete is click-to-confirm. Only one row across the
  // entire feed can be armed at a time — clicking Delete on a
  // different row resets the previous arm. The compound key
  // disambiguates a dictation row id from a meeting session id
  // (both are integer PKs from different tables).
  type ConfirmingRow =
    | { kind: "dictation"; id: number }
    | { kind: "meeting"; id: number };
  let confirmingDelete = $state<ConfirmingRow | null>(null);
  let confirmDeleteTimer: number | undefined;

  onDestroy(() => {
    window.clearTimeout(clearTimer);
    window.clearTimeout(confirmDeleteTimer);
    history.cancelSearchDebounce();
  });

  function isConfirming(kind: "dictation" | "meeting", id: number): boolean {
    return (
      confirmingDelete?.kind === kind && confirmingDelete?.id === id
    );
  }

  function armConfirm(row: ConfirmingRow) {
    window.clearTimeout(confirmDeleteTimer);
    confirmingDelete = row;
    confirmDeleteTimer = window.setTimeout(() => {
      confirmingDelete = null;
    }, 5000);
  }

  function handleRowDelete(entry: HistoryEntry) {
    if (isConfirming("dictation", entry.id)) {
      window.clearTimeout(confirmDeleteTimer);
      confirmingDelete = null;
      void onDelete(entry);
      return;
    }
    armConfirm({ kind: "dictation", id: entry.id });
  }

  function handleMeetingDelete(session: MeetingSession) {
    if (isConfirming("meeting", session.id)) {
      window.clearTimeout(confirmDeleteTimer);
      confirmingDelete = null;
      void onMeetingDelete(session);
      return;
    }
    armConfirm({ kind: "meeting", id: session.id });
  }

  function selectFilter(next: HistoryFilter) {
    history.filter = next;
    // Don't drag a stale armed confirm across a filter switch.
    window.clearTimeout(confirmDeleteTimer);
    confirmingDelete = null;
  }

  function startClear() {
    if (history.totalCount === 0) return;
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
    <div class="header-actions">
      <div class="search-wrap">
        <!-- search icon -->
        <svg class="search-icon" aria-hidden="true" focusable="false" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
        <input
          type="text"
          placeholder="Search history…"
          value={history.historyQuery}
          oninput={onSearchInput}
          aria-label="Search history"
        />
        {#if history.searching}
          <span class="search-spinner" aria-label="Searching" role="status"></span>
        {/if}
      </div>
      {#if history.totalCount > 0}
        {#if clearConfirming}
          <div class="clear-confirm" role="group" aria-label="Confirm clear history">
            <span class="clear-confirm-text">
              Delete all {history.totalCount}?
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
      {#if onExportBundle && (history.totalCount > 0 || meeting.sessions.length > 0)}
        <button
          type="button"
          class="ghost"
          onclick={() => (exportDialogOpen = true)}
          data-testid="history-export-bundle"
          aria-label="Export filtered rows"
        >
          Export filtered…
        </button>
      {/if}
    </div>
  </header>

  <!--
    Filter chips (#357 phase 2). "All" interleaves dictation +
    meeting rows by recency; the kind-specific chips scope to one
    stream. The chip strip is hidden when no rows of either kind
    exist (so a fresh install doesn't render a filter UI for
    nothing); the search-active state forces the filter to
    "Dictation" because cross-stream FTS lands in a follow-up.
  -->
  {#if history.totalCount > 0 || meeting.sessions.length > 0}
    <div class="history-filters" role="group" aria-label="Filter history">
      {#each [{ value: "all", label: "All" }, { value: "dictation", label: "Transcriptions" }, { value: "meetings", label: "Meetings" }] as chip (chip.value)}
        <button
          type="button"
          class="filter-chip"
          class:active={history.effectiveFilter === chip.value}
          aria-pressed={history.effectiveFilter === chip.value}
          onclick={() => selectFilter(chip.value as HistoryFilter)}
          data-testid="history-filter-{chip.value}"
        >
          {chip.label}
        </button>
      {/each}
    </div>
  {/if}

  {#if history.error}
    <ErrorDisplay error={history.error} scope="Dictation history" />
  {/if}
  {#if meeting.error}
    <ErrorDisplay error={meeting.error} scope="Meeting history" />
  {/if}

  {#if !history.feedLoaded}
    <p class="loading-skeleton">Loading history…</p>
  {:else if history.mergedFeed.length === 0}
    <div class="empty-history">
      <!-- archive icon -->
      <svg class="empty-icon" aria-hidden="true" focusable="false" width="44" height="44" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.25" stroke-linecap="round" stroke-linejoin="round"><polyline points="21 8 21 21 3 21 3 8"/><rect x="1" y="3" width="22" height="5"/><line x1="10" y1="12" x2="14" y2="12"/></svg>
      <p class="empty-text">
        {#if hasQuery}
          No matches for "<em>{history.historyQuery}</em>". Try a shorter query.
        {:else if history.effectiveFilter === "dictation"}
          No dictation transcripts yet.
        {:else if history.effectiveFilter === "meetings"}
          No meeting sessions yet.
        {:else}
          Nothing here yet.
        {/if}
      </p>
      {#if !hasQuery}
        <div class="empty-hint">
          <p>Press the toggle hotkey or the Start button on the Dictation panel — your first transcript will land here.</p>
        </div>
      {/if}
    </div>
  {:else}
    <ul class="history-list" data-version={history.version}>
      {#each history.mergedFeed as row (row.kind + ":" + (row.kind === "dictation" ? row.entry.id : row.session.id))}
        {#if row.kind === "dictation"}
          <HistoryDictationRow
            entry={row.entry}
            confirming={isConfirming("dictation", row.entry.id)}
            models={dictation.models}
            {formatTimestamp}
            {onCopy}
            onDelete={handleRowDelete}
            onExportCsv={onExportDictationCsv}
            onSetName={(id, name) => void history.setEntryName(id, name)}
          />
        {:else}
          <HistoryMeetingRow
            session={row.session}
            confirming={isConfirming("meeting", row.session.id)}
            onLoadDetail={onMeetingLoadDetail}
            onDelete={handleMeetingDelete}
            onExport={onMeetingExport}
            onCopy={onMeetingCopy}
            onSetName={(id, name) => void meeting.setSessionName(id, name)}
          />
        {/if}
      {/each}
    </ul>
  {/if}
</section>

{#if exportDialogOpen}
  <ExportOptionsDialog
    initialKind={history.effectiveFilter === "all"
      ? "auto"
      : history.effectiveFilter === "dictation"
        ? "dictation"
        : "meetings"}
    onConfirm={onExportConfirm}
    onCancel={() => (exportDialogOpen = false)}
  />
{/if}

<style>
.history {
  margin-top: 2.5rem;
  text-align: left;
}

.history-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header input[type="text"] {
  flex: 1;
  max-width: 18rem;
  padding: 0.5em 0.85em 0.5em 2.2rem;
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
  background-color: var(--danger-bg);
  border: 1px solid #e8c4c4;
  border-radius: 8px;
  font-size: 0.85rem;
}
.clear-confirm-text {
  color: var(--danger);
  font-weight: 500;
}
.clear-confirm-yes {
  /* Slightly emphasised — same `danger` palette but opaque so the
     primary action reads first. The Cancel button below it stays
     in the default ghost-danger styling. */
  background-color: var(--danger-bg);
  border-color: var(--danger);
  color: #8a0000;
}

.history-filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem;
  margin-bottom: 1rem;
}
.filter-chip {
  appearance: none;
  padding: 0.3em 0.85em;
  font-size: 0.82rem;
  font-weight: 500;
  font-family: inherit;
  color: var(--text-secondary);
  background-color: var(--bg-sidebar);
  border: 1px solid #d8d8dc;
  border-radius: 999px;
  cursor: pointer;
  transition: background-color 0.12s, border-color 0.12s, color 0.12s;
}
.filter-chip:hover:not(:disabled) {
  background-color: var(--bg-sidebar);
  border-color: var(--border);
}
.filter-chip.active {
  background-color: var(--text-primary);
  border-color: var(--text-primary);
  color: var(--bg-surface);
  font-weight: 600;
}
.filter-chip:disabled {
  opacity: 0.55;
  cursor: not-allowed;
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
  color: var(--text-primary);
  background-color: var(--bg-surface);
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

.empty-history {
  margin: 2rem 0 0.5rem;
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  color: var(--text-muted);
}

.empty-icon {
  margin-bottom: 0.75rem;
  opacity: 0.35;
}

.empty-text {
  font-size: 0.9rem;
  margin: 0 0 1rem;
}

.empty-hint {
  padding: 0.75rem 1.25rem;
  border: 1px dashed var(--border);
  border-radius: 10px;
  max-width: 22rem;
}

.empty-hint p {
  margin: 0;
  font-size: 0.8rem;
  color: var(--text-muted);
}

.panel-history {
  margin-top: 2.5rem;
  text-align: left;
  padding-bottom: 0.25rem;
}

.loading-skeleton {
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
  border-radius: 6px;
  color: var(--text-muted);
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

.search-icon {
  position: absolute;
  left: 0.65rem;
  pointer-events: none;
  color: var(--text-muted);
  opacity: 0.7;
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

</style>
