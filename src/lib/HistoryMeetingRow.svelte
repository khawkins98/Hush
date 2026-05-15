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
  import { invoke } from "@tauri-apps/api/core";
  import type {
    MeetingExportFormat,
    MeetingSession,
    MeetingSessionDetail,
    SpeakerIdentity,
  } from "./types";
  import HistoryActionRow, { type ExpandAction, type ExportMenuEntry } from "./HistoryActionRow.svelte";

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
    /// Set or clear the user-defined short label for this session.
    onSetName?: (id: number, name: string | null) => void | Promise<void>;
  };

  let { session, confirming, onLoadDetail, onDelete, onExport, onCopy, onSetName }: Props = $props();

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

  // Open/close state for the Export popover is now managed by HistoryActionRow.
  // Export items derived from the onExport callback availability.
  let meetingExportItems = $derived<ExportMenuEntry[]>(
    onExport
      ? [
          {
            kind: "item",
            label: "Plain text (.txt)",
            onSelect: () => void onExport?.(session, "text"),
            testId: `meeting-export-text-${session.id}`,
          },
          {
            kind: "item",
            label: "CSV (.csv)",
            onSelect: () => void onExport?.(session, "csv"),
            testId: `meeting-export-csv-${session.id}`,
          },
          {
            kind: "item",
            label: "JSON (.json)",
            onSelect: () => void onExport?.(session, "json"),
            testId: `meeting-export-json-${session.id}`,
          },
        ]
      : [],
  );

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
      // Load speaker identities if any utterance was linked to one.
      if (detail.utterances.some((u) => u.speakerIdentityId !== null)) {
        const list = await invoke<SpeakerIdentity[]>("speaker_list");
        const m = new Map<number, SpeakerIdentity>();
        for (const s of list) m.set(s.id, s);
        speakerIdentityMap = m;
      }
    } catch (e) {
      detailError = e instanceof Error ? e.message : String(e);
    } finally {
      detailLoading = false;
    }
  }

  // ExpandAction prop for HistoryActionRow — re-derived after expanded/$state is live.
  let expandAction = $derived<ExpandAction>({
    expanded,
    onClick: toggleExpand,
    title: expanded
      ? "Hide transcript"
      : `Show transcript (${session.utteranceCount})`,
    ariaLabel: expanded
      ? "Hide transcript"
      : `Show transcript (${session.utteranceCount} utterances)`,
    testId: `meeting-show-transcript-${session.id}`,
  });

  // Speaker identity map: identity id → SpeakerIdentity. Populated after
  // expand when at least one utterance has a speakerIdentityId (#667).
  let speakerIdentityMap = $state<Map<number, SpeakerIdentity>>(new Map());

  // Map from raw speakerLabel (e.g. "Speaker 1") to the identity id for
  // that label in this session. Built from the first utterance per label
  // that carries a speakerIdentityId.
  let labelToIdentityId = $derived.by((): Map<string, number> => {
    const m = new Map<string, number>();
    if (!detail) return m;
    for (const u of detail.utterances) {
      if (u.speakerLabel !== null && u.speakerIdentityId !== null && !m.has(u.speakerLabel)) {
        m.set(u.speakerLabel, u.speakerIdentityId);
      }
    }
    return m;
  });

  // Ordered list of identified speakers in this session (label → identity),
  // deduped and in first-appearance order.
  let identifiedSpeakers = $derived.by((): Array<{ label: string; identity: SpeakerIdentity }> => {
    const result: Array<{ label: string; identity: SpeakerIdentity }> = [];
    for (const [label, identityId] of labelToIdentityId) {
      const identity = speakerIdentityMap.get(identityId);
      if (identity) result.push({ label, identity });
    }
    return result;
  });

  // Resolve the display name for an utterance's speaker label. Uses the
  // identity's displayName if present, otherwise falls back to speakerCopy.
  function resolvedSpeakerName(label: string | null): string {
    if (label !== null) {
      const identityId = labelToIdentityId.get(label);
      if (identityId !== undefined) {
        const identity = speakerIdentityMap.get(identityId);
        if (identity?.displayName) return identity.displayName;
      }
    }
    return speakerCopy(label);
  }

  // Inline speaker rename state. `editingIdentityId` is the id of the
  // identity currently being renamed; null when no rename is in progress.
  let editingIdentityId = $state<number | null>(null);
  let renameValue = $state("");

  function startRename(identity: SpeakerIdentity, e: Event) {
    e.stopPropagation();
    editingIdentityId = identity.id;
    renameValue = identity.displayName ?? "";
  }

  async function commitRename() {
    const id = editingIdentityId;
    if (id === null) return;
    editingIdentityId = null;
    const trimmed = renameValue.trim();
    const newName = trimmed === "" ? null : trimmed;
    try {
      await invoke("speaker_rename", { id, displayName: newName });
      // Update local map so the transcript re-renders immediately.
      const existing = speakerIdentityMap.get(id);
      if (existing) {
        const updated = new Map(speakerIdentityMap);
        updated.set(id, { ...existing, displayName: newName });
        speakerIdentityMap = updated;
      }
    } catch {
      // Rename failed silently — user can retry.
    }
  }

  function handleRenameKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") void commitRename();
    else if (e.key === "Escape") editingIdentityId = null;
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

  // Inline name editing.
  let editingName = $state(false);
  let nameInputValue = $state(session.name ?? "");

  function startNameEdit(e: Event) {
    e.stopPropagation();
    nameInputValue = session.name ?? "";
    editingName = true;
  }

  async function commitName() {
    editingName = false;
    const trimmed = nameInputValue.trim();
    const newName = trimmed === "" ? null : trimmed;
    if (newName === session.name) return;
    await onSetName?.(session.id, newName);
  }

  function handleNameKeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") { void commitName(); }
    else if (e.key === "Escape") { editingName = false; }
  }
</script>

<li class="history-row meeting-row" class:confirming-active={confirming} data-kind="meeting" data-meeting-id={session.id}>
  <div class="row-layout">
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      class="row-content"
      role="button"
      tabindex="0"
      onclick={toggleExpand}
      onkeydown={(e) => (e.key === "Enter" || e.key === " ") && toggleExpand()}
      aria-expanded={expanded}
      title={expanded ? "Hide transcript" : `Show transcript (${session.utteranceCount} utterances)`}
    >
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
            class="name-badge"
            class:name-badge--set={session.name !== null}
            onclick={startNameEdit}
            title={session.name ? "Edit label" : "Add a label"}
            aria-label={session.name ? `Label: ${session.name} — click to edit` : "Add a label"}
          >{session.name ?? "Add label…"}</button>
        {/if}
      {/if}
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
    <HistoryActionRow
      {expandAction}
      onCopy={onCopy ? handleCopy : undefined}
      copyPending={copyPending}
      copyTitle={copyPending ? "Copying…" : "Copy transcript"}
      copyAriaLabel="Copy full transcript to clipboard"
      copyTestId="meeting-copy-transcript-{session.id}"
      exportItems={meetingExportItems.length ? meetingExportItems : undefined}
      exportTitle="Export transcript"
      exportAriaLabel="Export transcript"
      exportTestId="meeting-export-toggle-{session.id}"
      {confirming}
      onDelete={() => onDelete(session)}
      deleteTitle="Delete meeting"
      confirmTitle="Click again to confirm delete"
      deleteAriaLabel="Delete this meeting"
      confirmAriaLabel="Click again to confirm deleting this meeting"
      deleteTestId="meeting-delete-{session.id}"
    />
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
      {#if identifiedSpeakers.length > 0}
        <div class="speakers-bar" aria-label="Speakers in this meeting">
          <span class="speakers-bar-label">Speakers</span>
          {#each identifiedSpeakers as { label, identity } (identity.id)}
            <span class="speaker-chip">
              {#if editingIdentityId === identity.id}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  class="speaker-rename-input"
                  type="text"
                  bind:value={renameValue}
                  placeholder="Enter name…"
                  autofocus
                  onblur={() => void commitRename()}
                  onkeydown={handleRenameKeydown}
                  onclick={(e) => e.stopPropagation()}
                  onpointerdown={(e) => e.stopPropagation()}
                  data-testid="speaker-rename-input-{identity.id}"
                />
              {:else}
                <button
                  class="speaker-chip-btn"
                  class:speaker-chip-btn--named={identity.displayName !== null}
                  onclick={(e) => startRename(identity, e)}
                  title={identity.displayName
                    ? `${identity.displayName} (${label}) — click to rename`
                    : `${label} — click to add name`}
                  aria-label={identity.displayName
                    ? `Rename ${identity.displayName}`
                    : `Name ${label}`}
                  data-testid="speaker-chip-{identity.id}"
                >
                  {identity.displayName ?? label}
                  <svg class="pencil-icon" aria-hidden="true" width="11" height="11" viewBox="0 0 16 16" fill="currentColor">
                    <path d="M11.013 1.427a1.75 1.75 0 0 1 2.474 0l1.086 1.086a1.75 1.75 0 0 1 0 2.474l-8.61 8.61c-.21.21-.47.364-.756.445l-3.251.93a.75.75 0 0 1-.927-.928l.929-3.25c.081-.286.235-.547.445-.758l8.61-8.61zm1.414 1.06a.25.25 0 0 0-.354 0L10.811 3.75l1.439 1.44 1.263-1.263a.25.25 0 0 0 0-.354zm.262 2.794-1.439-1.44-6.194 6.193a.268.268 0 0 0-.063.108l-.655 2.295 2.294-.656a.268.268 0 0 0 .108-.063z"/>
                  </svg>
                </button>
              {/if}
            </span>
          {/each}
        </div>
      {/if}
      <ol class="meeting-transcript" aria-label="Meeting transcript">
        {#each detail.utterances as utt (utt.id)}
          <li class="utterance">
            {#if showSpeakerLabels}
              <span class="utterance-speaker">{resolvedSpeakerName(utt.speakerLabel)}</span>
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
    cursor: pointer;
    user-select: none;
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

  .name-badge {
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
  }
  .name-badge--set {
    border-style: solid;
    border-color: var(--accent, #5a7fff);
    color: var(--text-primary);
    background-color: color-mix(in srgb, var(--accent, #5a7fff) 12%, transparent);
  }
  .name-badge:hover {
    border-color: var(--accent, #5a7fff);
    color: var(--text-primary);
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

  /* Identified-speakers bar above the transcript (#667). */
  .speakers-bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.35rem;
    margin-top: 0.6rem;
    padding: 0.45rem 0.6rem;
    background-color: var(--bg-app);
    border-radius: 6px;
    font-size: 0.82rem;
  }
  .speakers-bar-label {
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin-right: 0.15rem;
    flex-shrink: 0;
  }
  .speaker-chip {
    display: inline-flex;
    align-items: center;
  }
  .speaker-chip-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.15rem 0.5rem;
    font-size: 0.8rem;
    font-weight: 500;
    border-radius: 4px;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    line-height: 1.5;
  }
  .speaker-chip-btn--named {
    border-color: var(--accent, #5a7fff);
    color: var(--text-primary);
    background-color: color-mix(in srgb, var(--accent, #5a7fff) 10%, transparent);
  }
  .speaker-chip-btn:hover {
    border-color: var(--accent, #5a7fff);
    color: var(--text-primary);
  }
  .pencil-icon {
    opacity: 0.55;
    flex-shrink: 0;
  }
  .speaker-chip-btn:hover .pencil-icon {
    opacity: 0.85;
  }
  .speaker-rename-input {
    padding: 0.12rem 0.4rem;
    font-size: 0.8rem;
    border: 1px solid var(--accent, #5a7fff);
    border-radius: 4px;
    background-color: var(--bg-surface);
    color: var(--text-primary);
    outline: none;
    width: 11rem;
  }

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .meeting-utterances,
    :root:not([data-theme="light"]) .meeting-sources { color: #9a9aa0; }
    :root:not([data-theme="light"]) .meeting-detail-status { color: #9a9aa0; }
    :root:not([data-theme="light"]) .meeting-detail-error { color: #f0a0a0; }
    :root:not([data-theme="light"]) .speaker-rename-input { background-color: #1e1e24; }
  }
  :root[data-theme="dark"] .meeting-utterances,
  :root[data-theme="dark"] .meeting-sources { color: #9a9aa0; }
  :root[data-theme="dark"] .meeting-detail-status { color: #9a9aa0; }
  :root[data-theme="dark"] .meeting-detail-error { color: #f0a0a0; }
  :root[data-theme="dark"] .speaker-rename-input { background-color: #1e1e24; }
</style>
