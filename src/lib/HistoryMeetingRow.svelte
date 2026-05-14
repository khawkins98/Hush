<!--
  Card for a single meeting session inside the unified History
  feed (#357 phase 2). Sibling to `HistoryDictationRow.svelte`;
  keeps the same row-card chrome but renders meeting-specific
  metadata (app, started, duration, utterance count, sources)
  and an inline transcript-expand affordance.

  The "Show transcript" expand uses Svelte's reactive primitives
  rather than a `<details>` element so the load state (loading /
  loaded / error) can be shown explicitly. The parent supplies an
  `onLoadDetail` callback that resolves to the full
  `MeetingSessionDetail`; the row caches it locally so a re-toggle
  is a free re-render rather than a refetch.

  Delete still uses the click-to-confirm pattern the dictation row
  shares with the meeting-mode panel — first click arms the button,
  second click within 5 s fires. Per-row arming with a 5 s timer.
  Cross-row coordination (only one row armed at a time across the
  list) lives in the parent so each row can stay self-contained.

  Action buttons are icon-only and always visible (right-aligned),
  matching the dictation row redesign.
-->
<script lang="ts">
  import type {
    MeetingExportFormat,
    MeetingSession,
    MeetingSessionDetail,
  } from "./types";

  type Props = {
    session: MeetingSession;
    /// True when this row's Delete button is currently armed.
    confirming: boolean;
    /// Resolves the session's full detail (utterances + metadata)
    /// for the inline transcript view. Called on first expand;
    /// subsequent toggles use the cached detail without a refetch.
    onLoadDetail: (id: number) => Promise<MeetingSessionDetail>;
    /// Click handler for Delete. The parent's implementation arms
    /// or fires based on the current row's `confirming` state.
    onDelete: (session: MeetingSession) => void;
    /// Per-row export (#357 phase 3b). Drives the OS save picker +
    /// the IPC. `null` if the parent didn't pass a handler — the
    /// Export button hides in that case so an embedding without
    /// export support stays clean.
    onExport?: (
      session: MeetingSession,
      format: MeetingExportFormat,
    ) => void | Promise<void>;
    /// Copy the full transcript to the clipboard. `null` if the
    /// parent didn't pass a handler — the Copy button hides so
    /// embeddings without clipboard support stay clean.
    onCopy?: (session: MeetingSession) => void | Promise<void>;
  };

  let { session, confirming, onLoadDetail, onDelete, onExport, onCopy }: Props = $props();

  // Local copy-in-flight state so the button shows a spinner while
  // the clipboard write (which may involve a fetch) is pending.
  let copyPending = $state(false);

  async function handleCopy() {
    copyPending = true;
    try {
      await onCopy?.(session);
    } finally {
      copyPending = false;
    }
  }

  // Open/close state for the Export popover. Toggled by the
  // download icon button; closes itself once the user picks a
  // format.
  let exportOpen = $state(false);

  function pickFormat(format: MeetingExportFormat) {
    exportOpen = false;
    void onExport?.(session, format);
  }

  // Inline-expand state for the transcript view. Initial click
  // fires `loadDetail`; subsequent toggles use the cached
  // `detail` so a flick of the affordance is free.
  let expanded = $state(false);
  let detail = $state<MeetingSessionDetail | null>(null);
  let detailError = $state<string | null>(null);
  let detailLoading = $state(false);

  async function toggleExpand() {
    expanded = !expanded;
    if (!expanded || detail !== null) return;
    detailLoading = true;
    detailError = null;
    try {
      detail = await onLoadDetail(session.id);
    } catch (e) {
      detailError = e instanceof Error ? e.message : String(e);
    } finally {
      detailLoading = false;
    }
  }

  function formatStarted(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function formatDuration(start: string, end: string | null): string {
    if (!end) return "in progress";
    const startMs = Date.parse(start);
    const endMs = Date.parse(end);
    if (isNaN(startMs) || isNaN(endMs)) return "?";
    const seconds = Math.round((endMs - startMs) / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes} min`;
    const hours = Math.floor(minutes / 60);
    const remMin = minutes - hours * 60;
    return `${hours}h ${remMin}m`;
  }

  // Source kinds are persisted as `mic` / `system` in the DB and
  // surfaced here under friendlier names. Unknown values pass
  // through so a future source kind still renders something.
  function sourceLabel(kind: string): string {
    switch (kind) {
      case "mic":
        return "Mic";
      case "system":
        return "System audio";
      default:
        return kind;
    }
  }
  function sourceListLabel(kinds: string[]): string {
    return kinds.map(sourceLabel).join(" + ");
  }

  // Speaker label rendering for the inline transcript. Backend
  // writes "mic" / "system" / "Speaker N" / null; map the source-
  // derived ones to friendlier copy and let model-derived labels
  // pass through.
  function speakerCopy(label: string | null): string {
    switch (label) {
      case "mic":
        return "You";
      case "system":
        return "Remote";
      case null:
        return "Unknown";
      default:
        return label;
    }
  }

  // Show speaker labels only when there are ≥2 distinct labels
  // across the session's utterances (#478). Single-speaker
  // sessions render bare text — repeating the same label on every
  // line is noise. Once a second speaker is detected the labels
  // become useful context for the prior lines too.
  let showSpeakerLabels = $derived.by(() => {
    if (!detail) return false;
    const distinct = new Set(
      detail.utterances
        .map((u) => u.speakerLabel)
        .filter((l): l is string => !!l),
    );
    return distinct.size >= 2;
  });
</script>

<li class="history-row meeting-row" class:confirming-active={confirming} data-kind="meeting" data-meeting-id={session.id}>
  <div class="row-layout">
    <div class="row-content">
      <div class="meeting-meta">
        <span class="meeting-app">{session.appName}</span>
        <span class="meeting-started">{formatStarted(session.startedAt)}</span>
        <span class="meeting-duration">
          {formatDuration(session.startedAt, session.endedAt)}
        </span>
        <span class="meeting-utterances">
          {session.utteranceCount} utterance{session.utteranceCount === 1 ? "" : "s"}
        </span>
        {#if session.sources && session.sources.length > 0}
          <span class="meeting-sources" aria-label="Audio sources">
            {sourceListLabel(session.sources)}
          </span>
        {/if}
      </div>

      {#if session.appTitle && session.appTitle !== session.appName}
        <p class="meeting-app-title" title={session.appTitle}>{session.appTitle}</p>
      {/if}
      {#if session.notes}
        <p class="meeting-notes">{session.notes}</p>
      {/if}
    </div>

    <!-- Icon action cluster — always visible, right-aligned -->
    <div class="history-actions" role="group" aria-label="Row actions">
      <!-- Expand/collapse transcript -->
      <button
        class="icon-btn"
        class:expanded
        onclick={toggleExpand}
        aria-expanded={expanded}
        title={expanded ? "Hide transcript" : `Show transcript (${session.utteranceCount})`}
        aria-label={expanded ? "Hide transcript" : `Show transcript (${session.utteranceCount} utterances)`}
        data-testid="meeting-show-transcript-{session.id}"
      >
        <!-- Chevron: rotates 180° when expanded via CSS -->
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <polyline points="6 9 12 15 18 9"/>
        </svg>
      </button>

      {#if onCopy}
        <button
          class="icon-btn"
          disabled={copyPending}
          onclick={handleCopy}
          title={copyPending ? "Copying…" : "Copy transcript"}
          aria-label="Copy full transcript to clipboard"
          data-testid="meeting-copy-transcript-{session.id}"
        >
          <!-- Lucide Copy -->
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
          </svg>
        </button>
      {/if}

      {#if onExport}
        <div class="export-popover">
          <button
            type="button"
            class="icon-btn"
            onclick={() => (exportOpen = !exportOpen)}
            aria-haspopup="menu"
            aria-expanded={exportOpen}
            title="Export transcript"
            aria-label="Export transcript"
            data-testid="meeting-export-toggle-{session.id}"
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
              <li>
                <button
                  type="button"
                  role="menuitem"
                  class="export-menu-item"
                  onclick={() => pickFormat("text")}
                  data-testid="meeting-export-text-{session.id}"
                >
                  Plain text (.txt)
                </button>
              </li>
              <li>
                <button
                  type="button"
                  role="menuitem"
                  class="export-menu-item"
                  onclick={() => pickFormat("csv")}
                  data-testid="meeting-export-csv-{session.id}"
                >
                  CSV (.csv)
                </button>
              </li>
              <li>
                <button
                  type="button"
                  role="menuitem"
                  class="export-menu-item"
                  onclick={() => pickFormat("json")}
                  data-testid="meeting-export-json-{session.id}"
                >
                  JSON (.json)
                </button>
              </li>
            </ul>
          {/if}
        </div>
      {/if}

      <button
        class="icon-btn danger"
        class:confirming
        title={confirming ? "Click again to confirm delete" : "Delete meeting"}
        onclick={() => onDelete(session)}
        aria-label={confirming
          ? "Click again to confirm deleting this meeting"
          : "Delete this meeting"}
        data-testid="meeting-delete-{session.id}"
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

  {#if expanded}
    {#if detailLoading}
      <p class="meeting-detail-status">Loading transcript…</p>
    {:else if detailError}
      <p class="meeting-detail-status meeting-detail-error">
        Couldn't load transcript: {detailError}
      </p>
    {:else if detail && detail.utterances.length === 0}
      <p class="meeting-detail-status">
        This session didn't capture any speech.
      </p>
    {:else if detail}
      <ol class="meeting-transcript" aria-label="Meeting transcript">
        {#each detail.utterances as utt (utt.id)}
          <li class="utterance">
            {#if showSpeakerLabels}
              <span class="utterance-speaker">{speakerCopy(utt.speakerLabel)}</span>
            {/if}
            <span class="utterance-text">{utt.text}</span>
          </li>
        {/each}
      </ol>
    {/if}
  {/if}
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

  .meeting-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 0.6rem;
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .meeting-app {
    font-weight: 600;
    color: var(--text-primary);
  }
  .meeting-utterances,
  .meeting-sources {
    color: var(--text-muted);
  }
  .meeting-app-title {
    margin: 0.3rem 0 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
    font-style: italic;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .meeting-notes {
    margin: 0.3rem 0 0;
    font-size: 0.88rem;
    color: var(--text-secondary);
    line-height: 1.4;
  }

  /* Icon action cluster */
  .history-actions {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    flex-shrink: 0;
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
    background-color: var(--bg-sidebar);
  }

  .meeting-detail-status {
    margin: 0.6rem 0 0;
    font-size: 0.85rem;
    color: var(--text-muted);
    font-style: italic;
  }
  .meeting-detail-error {
    color: var(--danger);
    font-style: normal;
  }

  .meeting-transcript {
    list-style: none;
    margin: 0.6rem 0 0;
    padding: 0.6rem;
    background-color: var(--bg-app);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    max-height: 24rem;
    overflow-y: auto;
  }
  .utterance {
    font-size: 0.88rem;
    line-height: 1.45;
  }
  .utterance-speaker {
    font-weight: 600;
    color: var(--text-secondary);
    margin-right: 0.4rem;
  }
  .utterance-text {
    color: var(--text-primary);
    white-space: pre-wrap;
  }

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .meeting-utterances,
    :root:not([data-theme="light"]) .meeting-sources { color: #9a9aa0; }
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
    :root:not([data-theme="light"]) .meeting-detail-status { color: #9a9aa0; }
    :root:not([data-theme="light"]) .meeting-detail-error { color: #f0a0a0; }
  }
  :root[data-theme="dark"] .meeting-utterances,
  :root[data-theme="dark"] .meeting-sources { color: #9a9aa0; }
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
  :root[data-theme="dark"] .meeting-detail-status { color: #9a9aa0; }
  :root[data-theme="dark"] .meeting-detail-error { color: #f0a0a0; }
</style>
