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
    onDelete: (session: MeetingSession) => void | Promise<void>;
  };

  let { sessions, sessionsLoaded, sessionsError, onDelete }: Props = $props();

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
    Permanent privacy line. Per the design memo, meeting-mode is a
    real product surface where the privacy stance is the load-bearing
    differentiator; surface it as a permanent UX line, not a banner
    that disappears.
  -->
  <p class="privacy-line" role="note">
    Audio is transcribed live and never saved. Only transcripts and
    timestamps persist — the audio itself stays in memory for ~30 s
    during inference, then is discarded.
  </p>

  <p class="hint-prose">
    When a meeting app is in the foreground (Zoom, Teams, Meet,
    Discord, Slack-call) and you opt in, Hush opens a session and
    streams the transcript here. Sessions are searchable and editable
    after the meeting ends.
  </p>

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
      No-sessions placeholder. Spelled out explicitly because Meeting
      Mode is in active development — a user landing here without
      this context would otherwise wonder if their meeting was
      captured. Surface what's pending and where to follow along.
    -->
    <div class="meetings-placeholder">
      <p class="placeholder-headline">No meeting sessions yet.</p>
      <p>
        Meeting Mode is rolling out in phases. The data layer for
        sessions and per-utterance transcripts is shipped (the panel
        you're reading is real), but the runtime that detects
        meetings and creates sessions is still in flight:
      </p>
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
