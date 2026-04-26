<script lang="ts">
  // Meeting Mode panel — Phase C scaffold (refs #33 / #109).
  //
  // Surfaces the data layer landed in this PR (meeting_sessions /
  // utterances tables + Repository + IPC commands). The panel
  // renders a placeholder state today because the streaming pump
  // that fills the table (#110) hasn't shipped — every component
  // of the UI is wired against real types so the panel will start
  // showing data the moment Phase C's session manager begins
  // inserting rows, with no further frontend changes needed.
  //
  // What's pending and where to look:
  //
  //   - Session creation: SessionManager (#110) detects meeting
  //     apps, opens sessions, writes utterances as the streaming
  //     transcriber emits them. Until that lands, the sessions
  //     list is always empty.
  //
  //   - Per-platform SystemAudio: Phase A2/A3/A4 (#105 / #106 /
  //     #107). Without one of those landing the meeting flow
  //     can't capture meeting audio (only mic audio works) — but
  //     mic-only single-speaker meeting transcription would work
  //     once #110 lands, so this isn't a hard prerequisite.
  //
  //   - Streaming inference: #108 (Whisper sliding-window). Without
  //     it, sessions emit one utterance per recording (the default
  //     impl behaviour). The panel renders fine either way.
  //
  //   - Diarization (per-speaker labels): Phase D (#111). Until
  //     then `speakerLabel` is null and the panel renders all
  //     utterances as "Unknown speaker."

  import type {
    AudioSourceListing,
    MeetingSession,
    MeetingSessionDetail,
    PersistedUtterance,
  } from "./types";

  type Props = {
    sessions: MeetingSession[];
    sessionsLoaded: boolean;
    sessionsError: string | null;
    /// Active session id from the backend's `meeting_active_session`
    /// command. `null` means no session is in flight; renders Start
    /// button. Non-null means a session is open; renders Stop button
    /// + a live status indicator.
    activeSessionId: number | null;
    /// Active session's full detail — utterances + metadata —
    /// polled by the parent every ~3 s while a session is in
    /// flight (#122 PR4 live transcript). `null` while no session
    /// is active OR before the first poll completes.
    activeDetail: MeetingSessionDetail | null;
    busy: boolean;
    /// Audio source listings (mic devices + system-audio). Surfaced
    /// here so the meeting panel can run an independent multi-source
    /// picker — Phase 3 of #122 promotes mic + system-audio in
    /// parallel as the meeting default. The dictation hot path's
    /// own (single-source) picker lives in `ControlsSection` and
    /// reads its own state, so changes in either don't move the
    /// other.
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    /// Mic device id chosen for the next meeting session. Single-
    /// select — meetings record at most one mic at a time, the
    /// "multi-source" axis is mic-vs-system-audio rather than
    /// mic-vs-mic. Two-way bound so the parent owns the state.
    meetingMicId: string | null;
    /// Whether the next meeting session also captures system audio
    /// alongside the mic. Defaults to `true` when the backend
    /// reports `is_supported`, `false` otherwise. Surfaced as a
    /// checkbox.
    meetingIncludeSystemAudio: boolean;
    onDelete: (session: MeetingSession) => void | Promise<void>;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    /// Lazy-load the detail for a historical session row. Returns
    /// the detail (utterances + metadata) so the panel can render
    /// the transcript inline. The parent caches results in a
    /// `Map<id, MeetingSessionDetail>` so re-expanding a row
    /// doesn't re-hit the IPC.
    onLoadDetail: (id: number) => Promise<MeetingSessionDetail>;
  };

  let {
    sessions,
    sessionsLoaded,
    sessionsError,
    activeSessionId,
    activeDetail,
    busy,
    sources,
    sourcesLoaded,
    meetingMicId = $bindable(),
    meetingIncludeSystemAudio = $bindable(),
    onDelete,
    onStart,
    onStop,
    onLoadDetail,
  }: Props = $props();

  /**
   * Per-row expand-state for historical sessions (#122 PR5). Keyed
   * by session id; presence of an entry means the row is currently
   * showing its transcript. The cached detail is stored alongside
   * so a toggle-close-then-toggle-open round-trip doesn't re-issue
   * the IPC. `null` value means "expand requested, fetch in flight"
   * (renders a Loading line until the promise resolves).
   */
  let expandedDetails = $state<Map<number, MeetingSessionDetail | null>>(
    new Map(),
  );

  /**
   * Toggle a historical session row's transcript view. First open
   * lazy-fetches via the `onLoadDetail` callback the parent owns;
   * subsequent toggles just flip the entry in/out of the map.
   */
  async function toggleSessionDetail(id: number) {
    if (expandedDetails.has(id)) {
      const next = new Map(expandedDetails);
      next.delete(id);
      expandedDetails = next;
      return;
    }
    // Optimistically mark as "loading" so the row immediately
    // shows feedback. Map swap-in for Svelte reactivity.
    const loading = new Map(expandedDetails);
    loading.set(id, null);
    expandedDetails = loading;
    try {
      const detail = await onLoadDetail(id);
      const done = new Map(expandedDetails);
      // Guard against the user collapsing the row mid-fetch — only
      // commit if they're still expecting it.
      if (done.has(id)) {
        done.set(id, detail);
        expandedDetails = done;
      }
    } catch (e) {
      // Drop the loading marker on error so the row falls back to
      // collapsed; the parent's error region is the right place to
      // surface the failure (already wired via `sessionsError`).
      const after = new Map(expandedDetails);
      after.delete(id);
      expandedDetails = after;
      console.error("toggleSessionDetail:", e);
    }
  }

  /**
   * Display label for an utterance's speaker. Pre-real-diarization
   * (#111) the pump tags utterances with `"mic"` / `"system"` based
   * on the source the chunk came from — a coarse but useful split
   * that maps to "you" vs "remote participants" on a typical Zoom
   * call. Until #111 lands, that's the best signal we have.
   */
  function speakerLabel(u: PersistedUtterance): string {
    switch (u.speakerLabel) {
      case "mic":
        return "You";
      case "system":
        return "Remote";
      case null:
      case undefined:
        return "Speaker";
      default:
        return u.speakerLabel;
    }
  }

  /**
   * Format a chunk-relative timestamp (`started_at_ms` measured
   * from session-open) as `mm:ss`. Read by the live-transcript
   * timeline so the user can scrub through the conversation.
   */
  function formatOffset(ms: number): string {
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
  }

  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));
  let pickableCount = $derived(mics.length + (systemAudio ? 1 : 0));

  // Effective source-list summary for the active-session line.
  // Phase 3 makes this multi-source: typically "Microphone +
  // System audio" by default. Computed off the panel's own state
  // so the line accurately reflects what the pump is currently
  // recording (the active session locked in these settings at
  // start time; mid-session toggles don't take effect until next
  // start, by design).
  let activeSourceSummary = $derived.by(() => {
    const labels: string[] = [];
    if (meetingMicId !== null) {
      const mic = mics.find((m) => m.id === meetingMicId);
      labels.push(mic?.name ?? meetingMicId);
    }
    if (meetingIncludeSystemAudio && systemAudio?.isSupported) {
      labels.push(systemAudio.name);
    }
    return labels.length === 0 ? "no source picked" : labels.join(" + ");
  });

  // Active session row for live-status reads (utterance counter,
  // source label). The page re-fetches `sessions` after each
  // utterance lands, so this row's `utteranceCount` is the live
  // count without any extra wiring.
  let activeSession = $derived(
    activeSessionId === null
      ? null
      : sessions.find((s) => s.id === activeSessionId) ?? null,
  );

  // Validation for the Start button: at least one source must
  // resolve to something the backend can capture. Mic-with-no-mic
  // (a host with zero mic devices) AND no system audio = nothing
  // to record, so disable Start with a clear hint.
  let canStart = $derived(
    (meetingMicId !== null && mics.length > 0) ||
      (meetingIncludeSystemAudio && (systemAudio?.isSupported ?? false)),
  );

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

  function formatStarted(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    // Compact local-time format: "Apr 26, 14:32"
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function appKindLabel(kind: MeetingSession["appKind"]): string {
    switch (kind) {
      case "meeting":
        return "Meeting";
      case "media":
        return "Media";
      default:
        return "Other";
    }
  }
</script>

<section class="meetings panel-meetings" aria-labelledby="meetings-heading">
  <header class="meetings-header">
    <h2 id="meetings-heading">
      <span class="panel-tag panel-tag-meetings" aria-hidden="true">M</span>
      Meeting transcripts
      <span class="panel-subtitle">live capture, never recorded</span>
    </h2>
  </header>

  <!--
    Permanent privacy line. Round-7 UX reviewer noted the previous
    framing leaked implementation trivia ("30s ring buffer") into a
    user-facing line. Lead with the user benefit (text appears
    instantly), then the promise (nothing stored). The buffer
    detail moves into the "How it works" disclosure below for users
    who want it.
  -->
  <p class="privacy-line" role="note">
    Hush transcribes meeting audio live and never saves the audio
    itself — only the transcript and timestamps persist.
  </p>

  <p class="hint-prose">
    When a meeting app is in the foreground (Zoom, Teams, Meet,
    Discord, Slack-call) and you opt in, Hush opens a session and
    streams the transcript here. Sessions are searchable and editable
    after the meeting ends.
  </p>

  <!--
    Manual session lifecycle controls (#110 MVP). Source picker lives
    here too (Phase 1 of #122) so the user picks mic vs system-audio
    in the same place where they start the session. While a session
    is active the picker is hidden — switching sources mid-session
    isn't supported today and the source is shown as a static label
    so the user can see what the next dictation will capture from.

    Auto-detect from foreground app is a follow-up (#112); today the
    user clicks Start, dictates with the hotkey, each transcript lands
    as an utterance under the active session, then they click Stop.
  -->
  <div class="meeting-controls" role="group" aria-label="Meeting session controls">
    {#if activeSessionId !== null}
      <div class="meeting-active-stack">
        <span class="meeting-active-indicator" role="status" aria-live="polite">
          <span class="meeting-active-dot" aria-hidden="true"></span>
          Session in progress
          {#if activeSession}
            — {activeSession.utteranceCount} utterance{activeSession.utteranceCount === 1
              ? ""
              : "s"} so far
          {/if}
        </span>
        <p class="meeting-dictate-prompt">
          <strong>Auto-recording</strong> from <code>{activeSourceSummary}</code>.
          Each chunk transcribes and lands as an utterance every ~10
          seconds.
        </p>
      </div>
      <button type="button" class="primary" onclick={onStop} disabled={busy}>
        Stop session
      </button>
    {:else}
      <div class="meeting-source-stack">
        <label class="meeting-source-label">
          Microphone
          {#if !sourcesLoaded}
            <span class="meeting-source-loading">Loading sources…</span>
          {:else if mics.length === 0}
            <span class="meeting-source-empty">
              No microphones detected.
            </span>
          {:else}
            <select bind:value={meetingMicId} disabled={busy}>
              {#each mics as mic (mic.id)}
                <option value={mic.id}>
                  {mic.name}{mic.isDefault ? " (default)" : ""}
                </option>
              {/each}
            </select>
          {/if}
        </label>
        {#if systemAudio}
          <label class="meeting-system-audio-toggle">
            <input
              type="checkbox"
              bind:checked={meetingIncludeSystemAudio}
              disabled={busy || !systemAudio.isSupported}
            />
            <span>
              Also record system audio
              {#if !systemAudio.isSupported}
                <span class="coming-soon-hint">
                  (coming soon on this platform — #33)
                </span>
              {:else}
                <span class="meeting-source-meta">
                  — captures the other side of Zoom / Meet / Teams calls
                </span>
              {/if}
            </span>
          </label>
        {/if}
      </div>
      <button
        type="button"
        class="primary"
        onclick={onStart}
        disabled={busy || !canStart}
        title={canStart ? undefined : "Pick at least one audio source"}
      >
        Start a session
      </button>
      <span class="meeting-controls-hint">
        Click Start to begin auto-recording. Each chunk transcribes
        every ~10 seconds and lands as an utterance below. Click Stop
        when done.
      </span>
    {/if}
  </div>

  {#if activeSessionId !== null}
    <!--
      Live transcript view (#122 PR4). The parent polls the
      `meeting_session_get` IPC every ~3 s while a session is in
      flight; new utterances render here as they finalise. Each
      row carries a coarse "You" / "Remote" badge derived from
      the source the chunk came from (mic / system) — primitive
      diarization ahead of #111's per-speaker model.
    -->
    {#if activeDetail && activeDetail.utterances.length > 0}
      <ol
        class="live-transcript"
        aria-live="polite"
        aria-label="Live meeting transcript"
      >
        {#each activeDetail.utterances as utt (utt.id)}
          <li class="utterance speaker-row-{utt.speakerLabel ?? 'unknown'}">
            <div class="utterance-meta">
              <span
                class="speaker-badge speaker-{utt.speakerLabel ?? 'unknown'}"
              >
                {speakerLabel(utt)}
              </span>
              <span class="utterance-time">{formatOffset(utt.startedAtMs)}</span>
            </div>
            <p class="utterance-text">{utt.text}</p>
          </li>
        {/each}
      </ol>
    {:else}
      <p class="live-transcript-empty">
        Listening… utterances will appear here as the pump finalises
        each chunk (every ~10 seconds).
      </p>
    {/if}
  {/if}

  <details class="how-it-works">
    <summary>How it works</summary>
    <p>
      Audio enters a small in-memory buffer (about 30 seconds at a
      time) where Hush's local Whisper model transcribes it. Once a
      window is transcribed, those audio samples are overwritten by
      the next window — the bytes never reach disk. The transcript
      and per-utterance timestamps are what gets persisted, plus
      the meeting-app name and an optional note you can add after
      the meeting ends.
    </p>
  </details>

  {#if sessionsError}
    <p class="error scoped-error" role="alert">
      <strong>Meeting sessions:</strong>
      {sessionsError}
    </p>
  {/if}

  {#if !sessionsLoaded}
    <p class="empty-meetings">Loading sessions…</p>
  {:else if sessions.length === 0}
    <!--
      No-sessions placeholder. Round-7 UX reviewer noted the previous
      framing read as a GitHub-ticket summary, not product copy.
      Lead with the user-facing message ("coming soon"), bury the
      developer-facing tracking-issue list under a disclosure for
      readers who want to follow along.
    -->
    <div class="meetings-placeholder">
      <p class="placeholder-headline">
        Live meeting transcripts are coming soon.
      </p>
      <p>
        Hush will automatically detect when you're on Zoom, Teams,
        Meet, or similar apps and start capturing the conversation —
        with the same privacy stance: audio in memory only, transcript
        on disk. Rolling out in phases over the coming weeks.
      </p>
      <details class="dev-notes">
        <summary>Developer notes — what's pending and where to follow along</summary>
      <ul class="placeholder-list">
        <li>
          <strong>Session manager + app classifier</strong> — detects
          when you're in a meeting and opens a session. Tracked in
          <a
            href="https://github.com/khawkins98/Hush/issues/110"
            target="_blank"
            rel="noopener noreferrer">#110</a
          >.
        </li>
        <li>
          <strong>System-audio capture per platform</strong> — needed
          for capturing the other side of a Zoom/Teams call. macOS:
          <a
            href="https://github.com/khawkins98/Hush/issues/105"
            target="_blank"
            rel="noopener noreferrer">#105</a
          >. Linux:
          <a
            href="https://github.com/khawkins98/Hush/issues/106"
            target="_blank"
            rel="noopener noreferrer">#106</a
          >. Windows:
          <a
            href="https://github.com/khawkins98/Hush/issues/107"
            target="_blank"
            rel="noopener noreferrer">#107</a
          >.
        </li>
        <li>
          <strong>Streaming transcription (Whisper sliding-window)</strong>
          — emits per-utterance partials so the panel updates live.
          Tracked in
          <a
            href="https://github.com/khawkins98/Hush/issues/108"
            target="_blank"
            rel="noopener noreferrer">#108</a
          >. Without it sessions still work, but each emits one
          utterance per recording instead of a live-updating timeline.
        </li>
        <li>
          <strong>Speaker diarization</strong> — labels per-speaker
          turns. Tracked in
          <a
            href="https://github.com/khawkins98/Hush/issues/111"
            target="_blank"
            rel="noopener noreferrer">#111</a
          >. Until it ships every utterance reads as "Unknown speaker."
        </li>
      </ul>
      <p class="placeholder-tail">
        The architectural shape of all this lives in
        <a
          href="https://github.com/khawkins98/Hush/blob/main/docs/system-audio-meeting-mode-proposal.md"
          target="_blank"
          rel="noopener noreferrer">docs/system-audio-meeting-mode-proposal.md</a
        >.
      </p>
      </details>
    </div>
  {:else}
    <ul class="sessions-list">
      {#each sessions as session (session.id)}
        <li class="session-row">
          <div class="session-meta">
            <span class="session-app">{session.appName}</span>
            <span class="session-kind session-kind-{session.appKind}">
              {appKindLabel(session.appKind)}
            </span>
            <span class="session-started">{formatStarted(session.startedAt)}</span>
            <span class="session-duration">
              {formatDuration(session.startedAt, session.endedAt)}
            </span>
            <span class="session-utterances">
              {session.utteranceCount} utterance{session.utteranceCount === 1 ? "" : "s"}
            </span>
          </div>
          {#if session.notes}
            <p class="session-notes">{session.notes}</p>
          {/if}
          {#if expandedDetails.has(session.id)}
            {@const detail = expandedDetails.get(session.id)}
            {#if detail === null}
              <p class="session-detail-loading">Loading transcript…</p>
            {:else if detail && detail.utterances.length === 0}
              <p class="session-detail-empty">
                This session has no utterances yet — likely a
                start-and-stop with no audio captured.
              </p>
            {:else if detail}
              <ol
                class="live-transcript session-detail-transcript"
                aria-label={`Transcript for ${session.appName} session`}
              >
                {#each detail.utterances as utt (utt.id)}
                  <li
                    class="utterance speaker-row-{utt.speakerLabel ?? 'unknown'}"
                  >
                    <div class="utterance-meta">
                      <span
                        class="speaker-badge speaker-{utt.speakerLabel ?? 'unknown'}"
                      >
                        {speakerLabel(utt)}
                      </span>
                      <span class="utterance-time">
                        {formatOffset(utt.startedAtMs)}
                      </span>
                    </div>
                    <p class="utterance-text">{utt.text}</p>
                  </li>
                {/each}
              </ol>
            {/if}
          {/if}
          <div class="session-actions">
            <button
              type="button"
              class="ghost"
              onclick={() => void toggleSessionDetail(session.id)}
              aria-expanded={expandedDetails.has(session.id)}
              aria-label={`${expandedDetails.has(session.id) ? "Hide" : "Show"} transcript for ${session.appName} session`}
            >
              {#if expandedDetails.has(session.id)}
                Hide transcript
              {:else if session.utteranceCount > 0}
                Show transcript ({session.utteranceCount})
              {:else}
                Show transcript
              {/if}
            </button>
            <button
              type="button"
              class="ghost"
              onclick={() => void onDelete(session)}
              aria-label={`Delete session from ${session.appName}`}
            >
              Delete
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.meetings {
  margin-top: 2rem;
}

.meetings-header h2 {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 1.1rem;
  margin: 0 0 0.5rem;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.5rem;
  height: 1.5rem;
  border-radius: 4px;
  background-color: #6a8cf0;
  color: white;
  font-size: 0.85rem;
  font-weight: 600;
}

.panel-tag-meetings {
  /* Distinct hue from history (H), replacements (R), vocabulary (V),
     models (M-already), so the at-a-glance icon column reads
     uniformly. */
  background-color: #8a5cf0;
}

.panel-subtitle {
  font-size: 0.8rem;
  font-weight: 400;
  color: #777;
  margin-left: 0.25rem;
}

/*
  Privacy line. Always visible at the top of the panel — the
  load-bearing UX commitment that meeting-mode never persists raw
  audio. Visually a quiet, framed line so it doesn't compete with
  the session list, but obviously deliberate.
*/
.privacy-line {
  margin: 0.25rem 0 0.75rem;
  padding: 0.6rem 0.85rem;
  border-left: 3px solid #6a8cf0;
  background-color: rgba(106, 140, 240, 0.08);
  border-radius: 4px;
  font-size: 0.9rem;
  line-height: 1.45;
  color: #333;
}

.hint-prose {
  margin: 0 0 1rem;
  font-size: 0.9rem;
  line-height: 1.5;
  color: #555;
}

.meeting-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 0.6rem;
  margin: 0.5rem 0 1rem;
}

.meeting-controls-hint {
  font-size: 0.85rem;
  color: #777;
  flex-basis: 100%;
}

.meeting-source-stack {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  flex: 1 1 18rem;
}

.meeting-source-label {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  font-size: 0.85rem;
  color: #555;
  min-width: 14rem;
}

.meeting-system-audio-toggle {
  display: flex;
  align-items: flex-start;
  gap: 0.45rem;
  font-size: 0.85rem;
  color: #333;
  line-height: 1.4;
  cursor: pointer;
  user-select: none;
}

.meeting-system-audio-toggle input[type="checkbox"] {
  margin: 0.2rem 0 0 0;
  flex-shrink: 0;
  cursor: pointer;
}

.meeting-system-audio-toggle input[type="checkbox"]:disabled {
  cursor: not-allowed;
}

.meeting-source-meta {
  color: #777;
  font-size: 0.8rem;
}

.coming-soon-hint {
  color: #aa6600;
  font-size: 0.8rem;
  font-style: italic;
}

.meeting-source-label select {
  padding: 0.45em 0.7em;
  font-size: 0.9rem;
  border-radius: 6px;
  border: 1px solid #d1d1d1;
  background-color: #ffffff;
  color: #0f0f0f;
  font-family: inherit;
}

.meeting-source-loading,
.meeting-source-empty {
  font-size: 0.85rem;
  color: #777;
  padding: 0.4rem 0;
}

.meeting-active-stack {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  flex: 1 1 18rem;
}

.meeting-dictate-prompt {
  margin: 0;
  font-size: 0.88rem;
  line-height: 1.45;
  color: #333;
}

.meeting-dictate-prompt code {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 0.82rem;
  background-color: rgba(0, 0, 0, 0.05);
  padding: 0.05em 0.35em;
  border-radius: 3px;
  color: #1a1a1a;
}

/*
  Live transcript — appears under the active-session controls
  while a meeting is in flight. Granola-style coloured bubbles for
  the You / Remote split. Auto-scrolls naturally as new utterances
  push older ones up; an explicit max-height lets long meetings
  remain scannable without taking over the whole window.
*/
.live-transcript {
  list-style: none;
  margin: 0.5rem 0 1rem;
  padding: 0.5rem 0.75rem;
  border: 1px solid #e0e0e0;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.01);
  max-height: 22rem;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.live-transcript-empty {
  margin: 0.5rem 0 1rem;
  padding: 0.6rem 0.85rem;
  border: 1px dashed #c7c7c7;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #555;
  font-size: 0.88rem;
  font-style: italic;
}

.utterance {
  padding: 0.35rem 0.5rem;
  border-radius: 6px;
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
}

.utterance.speaker-row-mic {
  background-color: rgba(106, 140, 240, 0.08);
  border-left: 3px solid #6a8cf0;
}

.utterance.speaker-row-system {
  background-color: rgba(216, 58, 58, 0.06);
  border-left: 3px solid #d83a3a;
}

.utterance.speaker-row-unknown {
  background-color: rgba(0, 0, 0, 0.03);
  border-left: 3px solid #aaa;
}

.utterance-meta {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  font-size: 0.78rem;
}

.speaker-badge {
  display: inline-flex;
  align-items: center;
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  padding: 0.1em 0.5em;
  border-radius: 3px;
}

.speaker-badge.speaker-mic {
  background-color: rgba(106, 140, 240, 0.18);
  color: #2a4cb0;
}

.speaker-badge.speaker-system {
  background-color: rgba(216, 58, 58, 0.16);
  color: #8a0000;
}

.speaker-badge.speaker-unknown {
  background-color: rgba(0, 0, 0, 0.08);
  color: #555;
}

.utterance-time {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: #888;
  font-size: 0.74rem;
}

.utterance-text {
  margin: 0;
  font-size: 0.92rem;
  line-height: 1.5;
  color: #1a1a1a;
}

.meeting-active-indicator {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  font-size: 0.9rem;
  color: #4a6cd0;
  font-weight: 500;
}

.meeting-active-dot {
  width: 0.6rem;
  height: 0.6rem;
  border-radius: 50%;
  background-color: #d83a3a;
  animation: meeting-pulse 1.4s ease-in-out infinite;
}

@keyframes meeting-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(0.85); }
}

@media (prefers-reduced-motion: reduce) {
  .meeting-active-dot {
    animation: none;
  }
}

.how-it-works,
.dev-notes {
  margin: 0.5rem 0 0.75rem;
}

.how-it-works summary,
.dev-notes summary {
  cursor: pointer;
  font-size: 0.85rem;
  color: #666;
  user-select: none;
  padding: 0.25rem 0;
}

.how-it-works summary:hover,
.dev-notes summary:hover {
  color: #1a1a1a;
}

.how-it-works[open] summary,
.dev-notes[open] summary {
  margin-bottom: 0.5rem;
}

.how-it-works > p {
  margin: 0;
  padding: 0.5rem 0.75rem;
  background-color: rgba(0, 0, 0, 0.02);
  border-radius: 4px;
  font-size: 0.85rem;
  line-height: 1.55;
  color: #555;
}

.empty-meetings {
  margin: 0;
  padding: 0.65rem 0.85rem;
  background-color: #fff7e6;
  border: 1px solid #f0c87b;
  border-radius: 6px;
  color: #6a4a00;
  font-size: 0.9rem;
}

.meetings-placeholder {
  padding: 1rem 1.1rem;
  border: 1px dashed #c7c7c7;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #444;
  font-size: 0.9rem;
  line-height: 1.55;
}

.placeholder-headline {
  margin: 0 0 0.5rem;
  font-weight: 600;
  color: #1a1a1a;
}

.placeholder-list {
  margin: 0.5rem 0 0.75rem 1.2rem;
  padding: 0;
}

.placeholder-list li {
  margin-bottom: 0.4rem;
}

.placeholder-list a {
  color: #4a6cd0;
}

.placeholder-tail {
  margin: 0.5rem 0 0;
  font-size: 0.85rem;
  color: #555;
}

.placeholder-tail a {
  color: #4a6cd0;
}

.sessions-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.session-row {
  border: 1px solid #e0e0e0;
  border-radius: 8px;
  padding: 0.75rem 1rem;
  background-color: rgba(0, 0, 0, 0.01);
}

.session-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem 0.85rem;
  align-items: center;
  font-size: 0.85rem;
  color: #555;
}

.session-app {
  font-weight: 600;
  color: #1a1a1a;
}

.session-kind {
  padding: 0.1em 0.5em;
  border-radius: 3px;
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.session-kind-meeting {
  background-color: rgba(106, 140, 240, 0.15);
  color: #2a4cb0;
}

.session-kind-media {
  background-color: rgba(216, 58, 58, 0.12);
  color: #8a0000;
}

.session-kind-other {
  background-color: rgba(0, 0, 0, 0.06);
  color: #555;
}

.session-notes {
  margin: 0.5rem 0 0;
  padding: 0.4rem 0.6rem;
  background-color: rgba(255, 235, 150, 0.3);
  border-radius: 4px;
  font-size: 0.9rem;
  color: #333;
}

.session-actions {
  margin-top: 0.5rem;
  display: flex;
  justify-content: flex-end;
  gap: 0.4rem;
}

.session-detail-transcript {
  /* Inherits the live-transcript shell. Override max-height since
     a closed session doesn't grow during display — show as much
     as fits naturally before clamping. */
  margin: 0.6rem 0 0.4rem;
  max-height: 28rem;
}

.session-detail-loading,
.session-detail-empty {
  margin: 0.5rem 0;
  padding: 0.5rem 0.75rem;
  border: 1px dashed #c7c7c7;
  border-radius: 6px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #555;
  font-size: 0.85rem;
  font-style: italic;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.4em 0.8em;
  font-size: 0.85rem;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
}

button.primary {
  background-color: #6a8cf0;
  color: white;
  border-color: #6a8cf0;
  font-weight: 600;
  padding: 0.5em 1em;
  font-size: 0.9rem;
}

button.primary:hover:not(:disabled) {
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

button.ghost {
  background-color: transparent;
}

button:hover:not(:disabled) {
  border-color: #396cd8;
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.error {
  margin: 0 0 0.75rem;
  padding: 0.65rem 0.85rem;
  border: 1px solid #d83a3a;
  background-color: #fee;
  color: #8a0000;
  border-radius: 6px;
  font-size: 0.9rem;
}

@media (prefers-color-scheme: dark) {
  .panel-subtitle {
    color: #999;
  }
  .privacy-line {
    color: #ddd;
    background-color: rgba(106, 140, 240, 0.12);
  }
  .hint-prose {
    color: #aaa;
  }
  .meeting-source-label,
  .meeting-source-loading,
  .meeting-source-empty,
  .meeting-controls-hint {
    color: #aaa;
  }
  .meeting-source-label select {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .meeting-dictate-prompt {
    color: #ddd;
  }
  .meeting-dictate-prompt code {
    background-color: rgba(255, 255, 255, 0.08);
    color: #f0f0f0;
  }
  .meeting-system-audio-toggle {
    color: #ddd;
  }
  .meeting-source-meta {
    color: #aaa;
  }
  .coming-soon-hint {
    color: #d4a040;
  }
  .live-transcript {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.02);
  }
  .live-transcript-empty {
    border-color: #444;
    background-color: rgba(255, 255, 255, 0.03);
    color: #aaa;
  }
  .utterance.speaker-row-mic {
    background-color: rgba(106, 140, 240, 0.14);
  }
  .utterance.speaker-row-system {
    background-color: rgba(216, 58, 58, 0.12);
  }
  .utterance.speaker-row-unknown {
    background-color: rgba(255, 255, 255, 0.04);
  }
  .speaker-badge.speaker-mic {
    background-color: rgba(106, 140, 240, 0.25);
    color: #c8d4f8;
  }
  .speaker-badge.speaker-system {
    background-color: rgba(216, 58, 58, 0.22);
    color: #f8b8b8;
  }
  .speaker-badge.speaker-unknown {
    background-color: rgba(255, 255, 255, 0.1);
    color: #aaa;
  }
  .utterance-time {
    color: #888;
  }
  .utterance-text {
    color: #f0f0f0;
  }
  .empty-meetings {
    background-color: #3a2e10;
    border-color: #7a5a20;
    color: #f0d090;
  }
  .meetings-placeholder {
    border-color: #444;
    background-color: rgba(255, 255, 255, 0.03);
    color: #bbb;
  }
  .placeholder-headline {
    color: #f0f0f0;
  }
  .placeholder-tail {
    color: #999;
  }
  .placeholder-list a,
  .placeholder-tail a {
    color: #6a8cf0;
  }
  .session-row {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.02);
  }
  .session-meta {
    color: #aaa;
  }
  .session-app {
    color: #f0f0f0;
  }
  .session-kind-meeting {
    background-color: rgba(106, 140, 240, 0.2);
    color: #c8d4f8;
  }
  .session-kind-media {
    background-color: rgba(216, 58, 58, 0.2);
    color: #f8b8b8;
  }
  .session-kind-other {
    background-color: rgba(255, 255, 255, 0.08);
    color: #aaa;
  }
  .session-notes {
    background-color: rgba(255, 235, 150, 0.1);
    color: #ddd;
  }
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: #6a8cf0;
  }
  .error {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
}
</style>
