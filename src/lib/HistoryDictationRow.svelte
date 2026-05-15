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
  import HistoryActionRow, { type ExportMenuEntry } from "./HistoryActionRow.svelte";

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
    /// Set or clear the user-defined short label for this entry.
    onSetName?: (id: number, name: string | null) => void | Promise<void>;
  };

  let {
    entry,
    confirming,
    models,
    formatTimestamp,
    onCopy,
    onDelete,
    onExportCsv,
    onSetName,
  }: Props = $props();

  let isIgnored = $derived(entry.ignored);
  const CLIPBOARD_FORMATS = ["plain", "markdown", "srt", "vtt"] as const satisfies readonly ExportFormat[];
  // Export items for the shared action row. Empty when the row is
  // ignored (no transcript to export).
  let exportItems = $derived<ExportMenuEntry[]>(
    isIgnored
      ? []
      : [
          ...CLIPBOARD_FORMATS.map((fmt) => ({
            kind: "item" as const,
            label: EXPORT_FORMAT_LABELS[fmt],
            onSelect: () => copyAs(fmt),
            testId: `history-export-${fmt}-${entry.id}`,
          })),
          ...(onExportCsv
            ? [
                { kind: "separator" as const },
                {
                  kind: "item" as const,
                  label: "Export CSV…",
                  onSelect: () => void onExportCsv?.(entry),
                  testId: `history-export-csv-${entry.id}`,
                },
              ]
            : []),
        ],
  );

  async function copyAs(format: ExportFormat) {
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

  // Transcript expand state — long entries are clamped to 3 lines by default.
  let transcriptExpanded = $state(false);
  let isLong = $derived(
    !entry.ignored &&
    (entry.transcript.length > 300 || entry.transcript.split(/\r?\n/).length > 4),
  );

  // Inline name editing.
  let editingName = $state(false);
  let nameInputValue = $state("");
  $effect(() => { if (!editingName) nameInputValue = entry.name ?? ""; });

  function startNameEdit(e: Event) {
    e.stopPropagation();
    nameInputValue = entry.name ?? "";
    editingName = true;
  }

  async function commitName() {
    editingName = false;
    const trimmed = nameInputValue.trim();
    const newName = trimmed === "" ? null : trimmed;
    if (newName === entry.name) return;
    await onSetName?.(entry.id, newName);
  }

  function handleNameKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") { void commitName(); }
    else if (e.key === "Escape") { editingName = false; }
  }
</script>

<li class="history-row" class:confirming-active={confirming} data-kind="dictation">
  <div class="row-layout">
    <div class="row-content">
      {#if onSetName}
        {#if editingName}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="name-input"
            type="text"
            bind:value={nameInputValue}
            placeholder="Add a label…"
            autofocus
            onblur={commitName}
            onkeydown={handleNameKeydown}
            onclick={(e) => e.stopPropagation()}
            onpointerdown={(e) => e.stopPropagation()}
          />
        {:else}
          <button
            class="card-title"
            class:card-title--set={entry.name !== null}
            onclick={startNameEdit}
            title={entry.name ? "Edit label" : "Add a label"}
            aria-label={entry.name ? `Label: ${entry.name} — click to edit` : "Add a label"}
          >{entry.name ?? "Add label…"}</button>
        {/if}
      {/if}
      <div class="row-meta">
        <span class="meta-item">
          <!-- calendar icon -->
          <svg class="meta-icon" aria-hidden="true" focusable="false" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="18" rx="2" ry="2"/><line x1="16" y1="2" x2="16" y2="6"/><line x1="8" y1="2" x2="8" y2="6"/><line x1="3" y1="10" x2="21" y2="10"/></svg>
          {formatTimestamp(entry.createdAt)}
        </span>
        {#if formatDuration(entry.durationMs)}
          <span class="meta-item">
            <!-- clock icon -->
            <svg class="meta-icon" aria-hidden="true" focusable="false" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
            {formatDuration(entry.durationMs)}
          </span>
        {/if}
        {#if entry.model}
          <span class="meta-chip">{displayModelName(entry.model)}</span>
        {/if}
      </div>
      {#if isIgnored}
        <p class="history-text ignored-note">Recording too short — not transcribed</p>
      {:else}
        <p class="history-text" class:clamped={!transcriptExpanded && isLong}>{entry.transcript}</p>
        {#if isLong}
          <button class="show-more-btn" onclick={() => (transcriptExpanded = !transcriptExpanded)}>
            {transcriptExpanded ? "Show less" : "Show more"}
          </button>
        {/if}
      {/if}
    </div>
    <HistoryActionRow
      onCopy={!isIgnored ? () => onCopy(entry) : undefined}
      copyTitle="Copy transcript"
      copyAriaLabel="Copy transcript"
      exportItems={exportItems.length ? exportItems : undefined}
      exportTitle="Export transcript"
      exportAriaLabel="Export transcript"
      exportTestId="history-export-{entry.id}"
      {confirming}
      onDelete={() => onDelete(entry)}
      deleteTitle="Delete transcript"
      confirmTitle="Click again to confirm delete"
      deleteAriaLabel="Delete this transcript"
      confirmAriaLabel="Click again to confirm deleting this transcript"
      deleteTestId="history-delete-{entry.id}"
    />
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

  .card-title {
    display: inline-block;
    margin: 0 0 0.3rem;
    padding: 0.1rem 0.45rem;
    font-size: 0.78rem;
    font-weight: 500;
    border-radius: 4px;
    border: 1px dashed var(--border);
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    line-height: 1.6;
    font-family: inherit;
  }
  .card-title--set {
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text-primary);
    border-style: solid;
    border-color: var(--accent, #5a7fff);
    background-color: color-mix(in srgb, var(--accent, #5a7fff) 12%, transparent);
  }
  .card-title:hover {
    border-color: var(--accent, #5a7fff);
    color: var(--text-primary);
  }

  .row-meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
    margin: 0.25rem 0 0.4rem;
    font-size: 0.78rem;
    color: var(--text-muted);
  }

  .meta-item {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
  }

  .meta-icon {
    flex-shrink: 0;
    opacity: 0.7;
  }

  .meta-chip {
    display: inline-block;
    padding: 0.1rem 0.45rem;
    font-size: 0.72rem;
    font-weight: 500;
    border-radius: 4px;
    border: 1px solid var(--border);
    background-color: var(--bg-app);
    color: var(--text-muted);
    line-height: 1.6;
  }

  .history-text {
    margin: 0 0 0.25rem;
    font-size: 0.95rem;
    line-height: 1.45;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .history-text.clamped {
    white-space: normal;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .show-more-btn {
    display: inline;
    margin: 0 0 0.25rem;
    padding: 0;
    background: none;
    border: none;
    font-size: 0.8rem;
    color: var(--accent, #5a7fff);
    cursor: pointer;
    font-family: inherit;
  }
  .show-more-btn:hover {
    text-decoration: underline;
  }

  .ignored-note {
    color: var(--text-muted);
    font-style: italic;
  }

  .name-input {
    display: block;
    width: 100%;
    margin: 0 0 0.3rem;
    padding: 0.15rem 0.4rem;
    font-size: 0.82rem;
    border: 1px solid var(--accent, #5a7fff);
    border-radius: 4px;
    background-color: var(--bg-app);
    color: var(--text-primary);
    outline: none;
    box-sizing: border-box;
  }
</style>
