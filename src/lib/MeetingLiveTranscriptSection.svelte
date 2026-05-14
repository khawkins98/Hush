<!--
  Live meeting transcript pane and waiting-for-speech placeholder.

  Rendered below RecordPanel while a meeting-pump session is in flight.
  This component owns everything meeting-specific that RecordPanel
  previously inlined: the `liveTranscriptText` derivation, the
  `showLiveTranscript` / `showMeetingWaiting` guards, and the typing
  indicator (#670) that shows a pulsing "…" after 1.5 s of silence
  between Whisper inference cycles.

  RecordPanel itself keeps the shared waveform, elapsed timer,
  record/stop button, and status label — all of which are relevant to
  both dictation and meeting modes.
-->
<script lang="ts">
  import { joinUtterances } from "./transcript-format";
  import type { MeetingSessionDetail } from "./types";

  type Props = {
    /// Live session detail polled by the orchestrator while a meeting-pump
    /// session is active. Null when no session is in progress.
    meetingActiveDetail: MeetingSessionDetail | null;
    /// True while the record button is in "recording" state. Used by the
    /// live-transcript visibility guard (we only show transcript while
    /// actively recording) and the typing indicator's idle-timer logic.
    recording: boolean;
    /// True when the session is meeting-only (no separate dictation phase).
    /// Used to show the waiting-for-speech placeholder before any utterances
    /// have arrived.
    meetingOnlyActive: boolean;
  };

  let { meetingActiveDetail, recording, meetingOnlyActive }: Props = $props();

  // Join finalized utterances + in-flight partials into a single string.
  // Speaker labels are prefixed only when ≥2 distinct speakers appear
  // (joinUtterances handles that internally). Mirrors the copy behaviour
  // in `copyMeetingSessionToClipboard` so live and clipboard text match.
  let liveTranscriptText = $derived.by(() => {
    if (!meetingActiveDetail) return "";
    const finals = meetingActiveDetail.utterances ?? [];
    const partials = meetingActiveDetail.currentPartials ?? [];
    return joinUtterances([...finals, ...partials], "\n");
  });

  let showLiveTranscript = $derived(
    recording && liveTranscriptText.trim().length > 0,
  );

  // Show the placeholder when a meeting-only session is active but no
  // utterances have arrived yet (room is quiet, or mic just opened).
  let showMeetingWaiting = $derived(
    meetingOnlyActive && liveTranscriptText.trim().length === 0,
  );

  // Typing indicator (#670): after ~1.5 s with no new partial, show a
  // pulsing "…" so the transcript doesn't look frozen during Whisper
  // inference cycles.
  let showTypingIndicator = $state(false);

  $effect(() => {
    const text = liveTranscriptText;
    const isRecording = recording;

    if (!isRecording || !text) {
      showTypingIndicator = false;
      return;
    }

    // Text changed — hide indicator and restart the idle timer.
    showTypingIndicator = false;
    const timer = setTimeout(() => {
      showTypingIndicator = true;
    }, 1500);
    return () => clearTimeout(timer);
  });
</script>

{#if showLiveTranscript}
  <!--
    Live transcript pane during meeting-pump recording. The streaming pump
    produces partials every few seconds and finalises them once the language
    model resolves a chunk — text appears with a 3–5 s delay against speech
    but updates continuously so the user sees what's been captured. Empty
    while no utterances have landed yet (silence, very short sessions).
    Idle / non-meeting recording paths skip this surface entirely.
  -->
  <section
    class="live-transcript"
    aria-label="Live transcript"
    aria-live="polite"
    data-testid="live-transcript"
  >
    <header class="live-transcript-header">
      <span class="live-transcript-dot" aria-hidden="true"></span>
      Live transcript
    </header>
    <p class="live-transcript-body">
      {liveTranscriptText}<!--
        Typing indicator: aria-hidden because the transcript text already
        conveys state to screen readers.
      -->{#if showTypingIndicator}<span class="typing-indicator" aria-hidden="true"> …</span>{/if}
    </p>
  </section>
{:else if showMeetingWaiting}
  <p class="meeting-waiting" aria-live="polite">Waiting for speech…</p>
{/if}

<style>
  /* Waiting-for-speech placeholder — quiet italic copy so the
     live-transcript area's absence doesn't look like a layout gap. */
  .meeting-waiting {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-muted);
    font-style: italic;
    text-align: center;
  }

  /* Live transcript panel. Renders beneath the record stage while a
     meeting-pump session is streaming utterances. The header row carries
     a pulsing red dot to reinforce "this is live". */
  .live-transcript {
    width: 100%;
    max-height: 180px;
    overflow-y: auto;
    border: 1px solid var(--border-color, #3d3d3d);
    border-radius: 8px;
    padding: 0.65rem 0.85rem;
    box-sizing: border-box;
    background: var(--bg-secondary, #1e1e1e);
  }
  .live-transcript-header {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.68rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-muted);
    margin-bottom: 0.45rem;
  }
  .live-transcript-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #dc2626;
    animation: pulse-dot 1.4s ease-in-out infinite;
    flex-shrink: 0;
  }
  @keyframes pulse-dot {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.3;
    }
  }
  .live-transcript-body {
    margin: 0;
    font-size: 0.88rem;
    line-height: 1.55;
    color: var(--text-primary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  /* Pulsing "…" appended after the last partial when no new text has
     arrived for 1.5 s. Subtle opacity animation; the transcript body's
     own font-size and colour are inherited. */
  .typing-indicator {
    animation: blink-indicator 1.2s step-start infinite;
  }
  @keyframes blink-indicator {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0;
    }
  }
</style>
