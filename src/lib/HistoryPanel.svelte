<script lang="ts">
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import ExportOptionsDialog from "./ExportOptionsDialog.svelte";
  import HistoryDictationRow from "./HistoryDictationRow.svelte";
  import HistoryMeetingRow from "./HistoryMeetingRow.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type {
    BundleSelection,
    HistoryEntry,
    MeetingExportFormat,
    MeetingSession,
    MeetingSessionDetail,
    ModelCard,
  } from "./types";

  /// Filter chip values for the unified History feed (#357 phase 2).
  /// "all" interleaves both kinds of rows by recency; "dictation"
  /// and "meetings" scope to a single kind. Search across meetings
  /// requires backend FTS that lands in a follow-up — while the
  /// search box has a query, the filter is forced to "dictation"
  /// since meetings can't match.
  export type HistoryFilter = "all" | "dictation" | "meetings";

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
    /// Meeting sessions to interleave with dictation entries (#357
    /// phase 2). Sorted by `startedAt` desc on the parent side; the
    /// panel reconciles the two streams by recency before rendering.
    /// Empty array when no meetings have been captured yet.
    meetingSessions: MeetingSession[];
    /// True after the parent's `meeting_sessions_list` IPC has
    /// resolved at least once, so the panel can distinguish "still
    /// loading" from "actually empty".
    meetingSessionsLoaded: boolean;
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
    /// Bulk "Export filtered" (#357 phase 3c-1). The panel
    /// surfaces the dialog; the parent fires the OS folder
    /// picker + the IPC with the chosen options + the active
    /// filter. `null` if the parent didn't pass a handler — the
    /// panel hides its Export filtered button in that case.
    onExportBundle?: (args: {
      kind: "auto" | "dictation" | "meetings" | "both";
      meetingFormat: MeetingExportFormat;
      activeFilter: HistoryFilter;
    }) => void | Promise<void>;
    /// Wipes every dictation row. Meetings have their own per-row
    /// Delete; bulk meeting delete pends until the export work in
    /// phase 3 ships an Export-filtered + Delete-filtered pair.
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
    meetingSessions,
    meetingSessionsLoaded,
    models,
    formatTimestamp,
    onSearchInput,
    onCopy,
    onDelete,
    onExportDictationCsv,
    onMeetingDelete,
    onMeetingLoadDetail,
    onMeetingExport,
    onExportBundle,
    onClearAll,
  }: Props = $props();

  // User-selected filter chip. Defaults to "all" so the unified
  // surface lands on first paint. Forced to "dictation" while the
  // search box has a query — see `effectiveFilter` below.
  let userFilter = $state<HistoryFilter>("all");

  /// Whether the bulk-export options dialog is open. Toggled by
  /// the panel header's "Export filtered" button; closes itself
  /// on Cancel / Confirm / Escape.
  let exportDialogOpen = $state(false);

  function onExportConfirm(selection: BundleSelection) {
    exportDialogOpen = false;
    void onExportBundle?.({
      kind: selection.kind,
      meetingFormat: selection.meetingFormat,
      activeFilter: effectiveFilter,
    });
  }

  let hasQuery = $derived(historyQuery.trim().length > 0);
  // #357 phase 2 step 3 lifted the search-time forced filter:
  // both streams now run their own searches (history FTS5 +
  // utterance FTS5 rolled up to sessions), so the user's chip
  // selection stays in effect while a query is active.
  let effectiveFilter = $derived<HistoryFilter>(userFilter);

  // Merged feed. Each entry tags its kind so the {#each} below can
  // dispatch to the right row component, and the sort key is the
  // creation/start instant in ms so the two streams interleave by
  // recency (newest first). Pre-#357-phase-2 the panel was
  // dictation-only and the parent's pagination already returned
  // entries newest-first; meetings are sorted parent-side too.
  type FeedRow =
    | { kind: "dictation"; sortKey: number; entry: HistoryEntry }
    | { kind: "meeting"; sortKey: number; session: MeetingSession };

  let mergedFeed = $derived<FeedRow[]>(
    (() => {
      const includeDictation =
        effectiveFilter === "all" || effectiveFilter === "dictation";
      const includeMeetings =
        effectiveFilter === "all" || effectiveFilter === "meetings";

      // Fast path: only one stream active — map directly, no merge.
      if (!includeMeetings) {
        if (!includeDictation) return [];
        return historyEntries.map((entry) => ({
          kind: "dictation" as const,
          sortKey: Date.parse(entry.createdAt) || 0,
          entry,
        }));
      }
      if (!includeDictation) {
        return meetingSessions.map((session) => ({
          kind: "meeting" as const,
          sortKey: Date.parse(session.startedAt) || 0,
          session,
        }));
      }

      // Both streams active. Both arrive newest-first from the
      // backend, so a two-pointer merge produces a sorted result in
      // O(N) rather than the previous O(N log N) rebuild+sort.
      const d: FeedRow[] = historyEntries.map((entry) => ({
        kind: "dictation" as const,
        sortKey: Date.parse(entry.createdAt) || 0,
        entry,
      }));
      const m: FeedRow[] = meetingSessions.map((session) => ({
        kind: "meeting" as const,
        sortKey: Date.parse(session.startedAt) || 0,
        session,
      }));

      const out: FeedRow[] = [];
      let di = 0,
        mi = 0;
      while (di < d.length && mi < m.length) {
        if (d[di].sortKey >= m[mi].sortKey) {
          out.push(d[di++]);
        } else {
          out.push(m[mi++]);
        }
      }
      while (di < d.length) out.push(d[di++]);
      while (mi < m.length) out.push(m[mi++]);
      return out;
    })(),
  );

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
    userFilter = next;
    // Don't drag a stale armed confirm across a filter switch —
    // the user has changed view, the muscle-memory of the prior
    // armed click no longer applies.
    window.clearTimeout(confirmDeleteTimer);
    confirmingDelete = null;
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
    <h2 id="history-heading">History</h2>
    <div class="header-actions">
      <div class="search-wrap">
        <input
          type="search"
          placeholder="Search history…"
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
          {#if onExportBundle && (historyTotalCount > 0 || meetingSessions.length > 0)}
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

  <!--
    Filter chips (#357 phase 2). "All" interleaves dictation +
    meeting rows by recency; the kind-specific chips scope to one
    stream. The chip strip is hidden when no rows of either kind
    exist (so a fresh install doesn't render a filter UI for
    nothing); the search-active state forces the filter to
    "Dictation" because cross-stream FTS lands in a follow-up.
  -->
  {#if historyTotalCount > 0 || meetingSessions.length > 0}
    <div class="history-filters" role="group" aria-label="Filter history">
      {#each [{ value: "all", label: "All" }, { value: "dictation", label: "Dictation" }, { value: "meetings", label: "Meetings" }] as chip (chip.value)}
        <button
          type="button"
          class="filter-chip"
          class:active={effectiveFilter === chip.value}
          aria-pressed={effectiveFilter === chip.value}
          onclick={() => selectFilter(chip.value as HistoryFilter)}
          data-testid="history-filter-{chip.value}"
        >
          {chip.label}
        </button>
      {/each}
    </div>
  {/if}

  {#if historyError}
    <ErrorDisplay error={historyError} scope="History" />
  {/if}

  {#if !historyLoaded || !meetingSessionsLoaded}
    <p class="loading-skeleton">Loading history…</p>
  {:else if mergedFeed.length === 0}
    <p class="empty-history">
      {#if historyQuery.trim().length > 0}
        No matches for "<em>{historyQuery}</em>". Try a shorter query.
      {:else if effectiveFilter === "dictation"}
        No dictation transcripts yet. Press the toggle hotkey or
        the Start button on the Dictation panel — the transcript
        will land here.
      {:else if effectiveFilter === "meetings"}
        No meeting sessions yet. Start a meeting from the
        Dictation panel — the session shows up here once it
        wraps up.
      {:else}
        Nothing here yet. Press the toggle hotkey or the Start
        button on the Dictation panel — your first transcript
        will land here.
      {/if}
    </p>
  {:else}
    <ul class="history-list" data-version={historyVersion}>
      {#each mergedFeed as row (row.kind + ":" + (row.kind === "dictation" ? row.entry.id : row.session.id))}
        {#if row.kind === "dictation"}
          <HistoryDictationRow
            entry={row.entry}
            confirming={isConfirming("dictation", row.entry.id)}
            {models}
            {formatTimestamp}
            {onCopy}
            onDelete={handleRowDelete}
            onExportCsv={onExportDictationCsv}
          />
        {:else}
          <HistoryMeetingRow
            session={row.session}
            confirming={isConfirming("meeting", row.session.id)}
            onLoadDetail={onMeetingLoadDetail}
            onDelete={handleMeetingDelete}
            onExport={onMeetingExport}
          />
        {/if}
      {/each}
    </ul>
  {/if}
</section>

{#if exportDialogOpen}
  <ExportOptionsDialog
    initialKind={effectiveFilter === "all"
      ? "auto"
      : effectiveFilter === "dictation"
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
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}

.history-header h2 {
  margin: 0;
  font-size: 1.1rem;
  font-weight: 600;
  color: var(--text-primary);
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
  background-color: rgba(44, 62, 143, 0.14);
  border-color: rgba(44, 62, 143, 0.4);
  color: var(--info-text);
  font-weight: 600;
}
.filter-chip:disabled {
  opacity: 0.55;
  cursor: not-allowed;
}

@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .clear-confirm {
    background-color: #2c1818;
    border-color: #4a2020;
  }
  :root:not([data-theme="light"]) .clear-confirm-text {
    color: #ff9090;
  }
  :root:not([data-theme="light"]) .clear-confirm-yes {
    background-color: #3a1818;
    border-color: var(--danger);
    color: #ff9090;
  }
}
:root[data-theme="dark"] .clear-confirm {
  background-color: #2c1818;
  border-color: #4a2020;
}
:root[data-theme="dark"] .clear-confirm-text {
  color: #ff9090;
}
:root[data-theme="dark"] .clear-confirm-yes {
  background-color: #3a1818;
  border-color: var(--danger);
  color: #ff9090;
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
  margin: 0.5rem 0;
  padding: 1rem;
  background-color: var(--bg-surface);
  border: 1px dashed #d1d1d1;
  border-radius: 8px;
  color: var(--text-muted);
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
  :root:not([data-theme="light"]) button {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button:hover:not(:disabled) {
    border-color: var(--accent);
  }
  :root:not([data-theme="light"]) .history-header h2 {
    color: #d8d8d8;
  }
  :root:not([data-theme="light"]) .filter-chip {
    background-color: #2a2a2d;
    border-color: #38383b;
  }
  :root:not([data-theme="light"]) .filter-chip:hover:not(:disabled) {
    background-color: #353539;
    border-color: #4a4a4d;
  }
  :root:not([data-theme="light"]) .filter-chip.active {
    background-color: rgba(150, 170, 240, 0.18);
    border-color: rgba(150, 170, 240, 0.5);
    color: #b8c8ff;
  }
  :root:not([data-theme="light"]) button.ghost {
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
  :root:not([data-theme="light"]) button.ghost.danger {
    color: #ff9090;
  }
  :root:not([data-theme="light"]) button.ghost.danger:hover:not(:disabled) {
    background-color: #3a1818;
    border-color: var(--danger);
  }
  :root:not([data-theme="light"]) .empty-history {
    background-color: #1f1f1f;
    border-color: #3a3a3a;
    color: #999;
  }
}
:root[data-theme="dark"] button {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button:hover:not(:disabled) {
  border-color: var(--accent);
}
:root[data-theme="dark"] .history-header h2 {
  color: #d8d8d8;
}
:root[data-theme="dark"] .filter-chip {
  background-color: #2a2a2d;
  border-color: #38383b;
}
:root[data-theme="dark"] .filter-chip:hover:not(:disabled) {
  background-color: #353539;
  border-color: #4a4a4d;
}
:root[data-theme="dark"] .filter-chip.active {
  background-color: rgba(150, 170, 240, 0.18);
  border-color: rgba(150, 170, 240, 0.5);
  color: #b8c8ff;
}
:root[data-theme="dark"] button.ghost {
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button.ghost:hover:not(:disabled) {
  background-color: #353535;
}
:root[data-theme="dark"] button.ghost.danger {
  color: #ff9090;
}
:root[data-theme="dark"] button.ghost.danger:hover:not(:disabled) {
  background-color: #3a1818;
  border-color: var(--danger);
}
:root[data-theme="dark"] .empty-history {
  background-color: #1f1f1f;
  border-color: #3a3a3a;
  color: #999;
}
</style>
