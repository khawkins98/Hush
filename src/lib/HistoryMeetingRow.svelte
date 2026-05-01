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
-->
<script lang="ts">
  import type { MeetingSession, MeetingSessionDetail } from "./types";

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
  };

  let { session, confirming, onLoadDetail, onDelete }: Props = $props();

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

  // Same source-list mapping the deprecated MeetingSessionsPanel
  // used: persisted as `mic` / `system` in the DB; surfaced to the
  // user as friendlier names. Unknown values pass through so a
  // future source kind still renders something.
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
</script>

<li class="history-row meeting-row" data-kind="meeting" data-meeting-id={session.id}>
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

  <div class="history-actions">
    <button
      class="ghost"
      onclick={toggleExpand}
      aria-expanded={expanded}
      data-testid="meeting-show-transcript-{session.id}"
    >
      {expanded ? "Hide transcript" : `Show transcript (${session.utteranceCount})`}
    </button>
    <button
      class="ghost danger"
      class:confirming
      onclick={() => onDelete(session)}
      aria-label={confirming
        ? "Click again to confirm deleting this meeting"
        : "Delete this meeting"}
      data-testid="meeting-delete-{session.id}"
    >
      {confirming ? "Click to confirm" : "Delete"}
    </button>
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
            <span class="utterance-speaker">{speakerCopy(utt.speakerLabel)}</span>
            <span class="utterance-text">{utt.text}</span>
          </li>
        {/each}
      </ol>
    {/if}
  {/if}
</li>

<style>
  .history-row {
    padding: 0.75rem 1rem;
    background-color: white;
    border: 1px solid #e1e1e1;
    border-radius: 8px;
  }

  .meeting-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem 0.6rem;
    font-size: 0.82rem;
    color: #5a5a5a;
    margin-bottom: 0.5rem;
  }
  .meeting-app {
    font-weight: 600;
    color: #2a2a2a;
  }
  .meeting-utterances,
  .meeting-sources {
    color: #6b6b6b;
  }
  .meeting-app-title {
    margin: 0 0 0.4rem;
    font-size: 0.85rem;
    color: #4a4a4a;
    font-style: italic;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .meeting-notes {
    margin: 0 0 0.5rem;
    font-size: 0.88rem;
    color: #444;
    line-height: 1.4;
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

  .meeting-detail-status {
    margin: 0.75rem 0 0;
    font-size: 0.85rem;
    color: #6b6b6b;
    font-style: italic;
  }
  .meeting-detail-error {
    color: #b03030;
    font-style: normal;
  }

  .meeting-transcript {
    list-style: none;
    margin: 0.75rem 0 0;
    padding: 0.6rem;
    background-color: #fafafa;
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
    color: #444;
    margin-right: 0.4rem;
  }
  .utterance-text {
    color: #2a2a2a;
    white-space: pre-wrap;
  }

  @media (prefers-color-scheme: dark) {
    .history-row {
      background-color: #1f1f22;
      border-color: #2f2f33;
    }
    .meeting-meta { color: #a8a8a8; }
    .meeting-app { color: #e8e8e8; }
    .meeting-utterances,
    .meeting-sources { color: #9a9aa0; }
    .meeting-app-title { color: #b0b0b8; }
    .meeting-notes { color: #c0c0c0; }
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
    .meeting-detail-status { color: #9a9aa0; }
    .meeting-detail-error { color: #f0a0a0; }
    .meeting-transcript {
      background-color: #18181b;
    }
    .utterance-speaker { color: #b8b8b8; }
    .utterance-text { color: #e8e8e8; }
  }
</style>
