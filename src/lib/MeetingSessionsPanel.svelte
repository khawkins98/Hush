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

  import type { MeetingSession } from "./types";

  type Props = {
    sessions: MeetingSession[];
    sessionsLoaded: boolean;
    sessionsError: string | null;
    /// Active session id from the backend's `meeting_active_session`
    /// command. `null` means no session is in flight; renders Start
    /// button. Non-null means a session is open; renders Stop button
    /// + a live status indicator.
    activeSessionId: number | null;
    busy: boolean;
    onDelete: (session: MeetingSession) => void | Promise<void>;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
  };

  let {
    sessions,
    sessionsLoaded,
    sessionsError,
    activeSessionId,
    busy,
    onDelete,
    onStart,
    onStop,
  }: Props = $props();

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
    Manual session lifecycle controls (#110 MVP). Auto-detect from
    foreground app is a follow-up; today the user clicks Start, dictates
    as usual, each transcript lands as an utterance under the active
    session, then they click Stop.
  -->
  <div class="meeting-controls" role="group" aria-label="Meeting session controls">
    {#if activeSessionId !== null}
      <span class="meeting-active-indicator" role="status" aria-live="polite">
        <span class="meeting-active-dot" aria-hidden="true"></span>
        Session in progress — each transcription is appended below.
      </span>
      <button type="button" class="primary" onclick={onStop} disabled={busy}>
        Stop session
      </button>
    {:else}
      <button type="button" class="primary" onclick={onStart} disabled={busy}>
        Start a session
      </button>
      <span class="meeting-controls-hint">
        Click before your meeting; dictate as usual; click Stop when done.
      </span>
    {/if}
  </div>

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
          <div class="session-actions">
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
  align-items: center;
  gap: 0.6rem;
  margin: 0.5rem 0 1rem;
}

.meeting-controls-hint {
  font-size: 0.85rem;
  color: #777;
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
