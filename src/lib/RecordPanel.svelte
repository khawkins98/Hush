<!--
  Centerpiece dictation controls: full-width waveform on top, a
  three-column row beneath with `leftAdjunct` (audio source) on
  the left, the circular Record / Stop button in the centre, and
  `rightAdjunct` (model chip) on the right. Status copy + mic-
  only badge + F5 status line render below the row.

  Several derived values (badge visibility, will-record-meeting
  hint, has-usable-source guard) live upstream — the parent owns
  the cross-component selection state and computes them once
  rather than each leaf re-deriving.
-->
<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import type { Snippet } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  import AudioWaveform from "./AudioWaveform.svelte";
  import MeetingLiveTranscriptSection from "./MeetingLiveTranscriptSection.svelte";
  import StatusLine from "./StatusLine.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import { Events } from "./events";
  import {
    listenForStatusLineChanges,
    readStatusLineEnabled,
  } from "./status-line";
  import type { MeetingSessionDetail } from "./types";

  type Props = {
    recording: boolean;
    busy: boolean;
    transcribing: boolean;
    hasUsableSource: boolean;
    noModelInstalled: boolean;
    willRecordMeeting: boolean;
    recordMode: "dictation" | "meeting" | null;
    selectedSourceLabel: string | null;
    activeModelName: string | null;
    error: ErrorDisplayShape | null;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    onOpenPermissions?: () => void;
    /// Live meeting-session detail polled by the orchestrator
    /// while a meeting-pump session is in flight. The streaming
    /// pump writes finalized utterances to `utterances` and
    /// in-flight ones to `currentPartials`; we join + render
    /// them as a live transcript while recording. `null` when no
    /// meeting session is active (dictation-only PTT or click-
    /// recording without SCK).
    meetingActiveDetail?: MeetingSessionDetail | null;
    /// True when a meeting session is auto-running but dictation
    /// is completely idle — no separate dictation phase. Used to
    /// apply meeting-specific styling (red waveform bars, "Meeting"
    /// button label, "press Stop" hint) and to show the waiting-
    /// for-speech placeholder when the live transcript is empty.
    meetingOnlyActive?: boolean;
    /// Left adjunct slot — audio source picker. Optional so the
    /// component still renders standalone in the test harness or
    /// any future minimal surface.
    leftAdjunct?: Snippet;
    /// Right adjunct slot — model chip.
    rightAdjunct?: Snippet;
  };

  let {
    recording,
    busy,
    transcribing,
    hasUsableSource,
    noModelInstalled,
    willRecordMeeting,
    recordMode,
    selectedSourceLabel,
    activeModelName,
    error,
    onStart,
    onStop,
    onOpenPermissions,
    meetingActiveDetail = null,
    meetingOnlyActive = false,
    leftAdjunct,
    rightAdjunct,
  }: Props = $props();

  // F5 status line — opt-in display gated by a localStorage flag,
  // re-applied via Tauri event when the Settings toggle flips so
  // the open main window updates without reload.
  let statusLineEnabled = $state(false);
  let unlistenStatusLine: UnlistenFn | null = null;
  // Transcription progress 0–100 (#566). Non-null only while
  // `transcribing` is true and the backend has fired at least one
  // progress tick. Resets when `recording` flips back to true so
  // back-to-back dictations each start with a clean bar.
  let transcriptionProgress = $state<number | null>(null);
  let unlistenProgress: UnlistenFn | null = null;

  // "Copied!" confirmation flash (#928). Mirrors the HUD's done state
  // by subscribing to the same `hud:state` events. Reverts to null
  // after ~1.5 s so the label doesn't get stuck on "Copied!" for the
  // next idle/ready state.
  let hudDone = $state(false);
  let doneTimer: ReturnType<typeof setTimeout> | null = null;
  let unlistenHudState: UnlistenFn | null = null;

  // Recording-duration timer. Mirrors the HUD's pattern (#360):
  // wall-clock start stamp when `recording` flips true, rAF
  // refresh of the label, reset to `0:00` on stop. Lets the user
  // see how long they've been recording without checking the HUD.
  let recordingStartedAt: number | null = null;
  let elapsedLabel = $state("0:00");
  let raf: number | undefined;

  function formatElapsed(ms: number): string {
    const totalSeconds = Math.max(0, Math.floor(ms / 1000));
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    const mm = minutes.toString().padStart(2, "0");
    const ss = seconds.toString().padStart(2, "0");
    if (hours > 0) return `${hours}:${mm}:${ss}`;
    return `${mm}:${ss}`;
  }

  $effect(() => {
    if (recording) {
      recordingStartedAt = Date.now();
      elapsedLabel = "00:00";
      transcriptionProgress = null;
      // Reset the "Copied!" flash when a new recording starts.
      if (doneTimer !== null) {
        clearTimeout(doneTimer);
        doneTimer = null;
      }
      hudDone = false;
    } else {
      recordingStartedAt = null;
      elapsedLabel = "00:00";
    }
  });

  onMount(async () => {
    statusLineEnabled = readStatusLineEnabled();
    unlistenStatusLine = await listenForStatusLineChanges((next) => {
      statusLineEnabled = next;
    });
    unlistenProgress = await listen<number>(Events.TranscriptionProgress, (event) => {
      transcriptionProgress = event.payload;
    });
    // Mirror the HUD's "done" state (#928): subscribe to the same
    // `hud:state` backend events so we can flash "Copied!" when the
    // clipboard write completes, then self-dismiss after ~1.5 s.
    unlistenHudState = await listen<{ state: string }>("hud:state", (event) => {
      if (event.payload.state === "done") {
        hudDone = true;
        if (doneTimer !== null) clearTimeout(doneTimer);
        doneTimer = setTimeout(() => {
          hudDone = false;
          doneTimer = null;
        }, 1500);
      }
    });
    const tick = () => {
      if (recordingStartedAt !== null) {
        elapsedLabel = formatElapsed(Date.now() - recordingStartedAt);
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    unlistenStatusLine?.();
    unlistenStatusLine = null;
    unlistenProgress?.();
    unlistenProgress = null;
    unlistenHudState?.();
    unlistenHudState = null;
    if (doneTimer !== null) {
      clearTimeout(doneTimer);
      doneTimer = null;
    }
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
  });

  // Waveform mood priority: error > recording > processing > idle.
  // `busy` covers starting/stopping/transcribing so the waveform stays
  // in processing mode throughout the entire post-stop pipeline (#928).
  // Error wins so a stop-time failure flashes the bars even while
  // `busy` is still true on its way down.
  let waveformMode = $derived<"idle" | "recording" | "processing" | "error">(
    error !== null
      ? "error"
      : recording
        ? "recording"
        : busy
          ? "processing"
          : "idle",
  );
</script>

<div class="record-stage" class:meeting={meetingOnlyActive} data-recording={recording ? "true" : "false"}>
  <!--
    Big expressive waveform as the visual centerpiece (#411
    mockup target). Width is 100 % of the content column,
    height bumps to 88 px, and the bars get a purple→cyan
    gradient while recording. Idle / processing / error moods
    keep the muted bar treatment from F1.
  -->
  <div class="record-waveform">
    <AudioWaveform mode={waveformMode} metering />
  </div>
  {#if transcribing && transcriptionProgress !== null}
    <!--
      Thin progress bar shown while whisper.cpp is running (#566).
      Only visible once the backend has fired at least one progress
      tick so we don't flash a 0% bar on very short clips where
      inference finishes before the first event arrives.
    -->
    <div class="transcription-progress-bar" aria-hidden="true">
      <div
        class="transcription-progress-fill"
        style="width: {transcriptionProgress}%"
      ></div>
    </div>
  {/if}

  <!--
    Three-column row: source picker on the left, the circular
    Record / Stop button in the centre, model chip on the right.
    The adjunct slots are filled by `DictationSection` via the
    `leftAdjunct` / `rightAdjunct` snippet props; this component
    has no knowledge of source or model state.
  -->
  <div class="record-row">
    <div class="record-row-adjunct record-row-adjunct--left">
      {@render leftAdjunct?.()}
    </div>

    <div class="record-btn-cell">
      <!--
        Visible "RECORD" label above the button gives the centre
        column the same vertical rhythm as the source / model
        adjuncts (label above, control below). Without it the
        button floats with empty space above where the field
        labels sit on the flanking columns. aria-hidden because
        the button itself carries an aria-label.
      -->
      <span class="record-btn-label" aria-hidden="true">{meetingOnlyActive ? 'Meeting' : 'Record'}</span>
      {#if !recording}
        <button
          class="record-btn"
          data-testid="record-start-btn"
          onclick={onStart}
          disabled={busy || !hasUsableSource || noModelInstalled}
          aria-label={busy
            ? "Working"
            : noModelInstalled
              ? "Choose a model first"
              : !hasUsableSource
                ? "No audio input available — connect a microphone to record"
                : willRecordMeeting
                  ? "Record meeting (mic plus system audio)"
                  : "Start recording"}
          title={noModelInstalled
            ? "Choose a model first"
            : !hasUsableSource
              ? "No audio input available"
              : undefined}
          data-record-mode={willRecordMeeting ? "meeting" : "dictation"}
        >
          {#if transcribing}
            <span class="spinner" aria-hidden="true"></span>
          {:else}
            <span class="record-icon record-icon-idle" aria-hidden="true"></span>
          {/if}
        </button>
      {:else}
        <button
          class="record-btn recording"
          onclick={onStop}
          disabled={busy}
          aria-label={meetingOnlyActive ? "Stop meeting recording" : "Stop recording and transcribe"}
        >
          <span class="record-icon record-icon-stop" aria-hidden="true"></span>
        </button>
      {/if}
    </div>

    <div class="record-row-adjunct record-row-adjunct--right">
      {@render rightAdjunct?.()}
    </div>
  </div>

  <!--
    Recording-duration readout. Tabular-nums so the digits don't
    jitter horizontally as they tick up. Always visible (shows
    "00:00" while idle) so the column header rhythm stays
    constant across recording / not-recording states.
  -->
  <p
    class="record-time"
    class:live={recording}
    aria-label={recording ? `Recording duration ${elapsedLabel}` : undefined}
  >
    {elapsedLabel}
  </p>

  <!--
    Status label sits under the time readout — the verb the user
    is primed to do. aria-live so screen readers announce the
    state change when a hotkey toggles recording from another
    app. Stays empty in idle so the focal weight goes to the
    button.
  -->
  <p class="record-label" aria-live="polite">
    {#if recording}
      Recording
      {#if recordMode === "meeting"}
        <span class="status-mode" data-record-mode="meeting"
          >· mic + system audio</span
        >
      {:else if recordMode === "dictation"}
        <span class="status-mode" data-record-mode="dictation"
          >· mic only</span
        >
      {/if}
      {meetingOnlyActive ? "— press Stop" : "— release hotkey or press Stop"}
    {:else if busy}
      Processing…
    {:else if hudDone}
      Copied!
    {:else if willRecordMeeting}
      Record meeting <span class="record-mode-hint">mic + system audio</span>
    {:else if !noModelInstalled && hasUsableSource}
      Press to record
    {:else if !hasUsableSource && !noModelInstalled}
      No microphone detected — connect one and reopen Hush.
    {/if}
  </p>
</div>

<MeetingLiveTranscriptSection
  {meetingActiveDetail}
  {recording}
  {meetingOnlyActive}
/>

{#if statusLineEnabled}
  <StatusLine
    audioSourceLabel={selectedSourceLabel}
    modelName={activeModelName}
  />
{/if}

<style>
  /* The content column's centerpiece: big expressive waveform
     above a circular Record / Stop button, with status copy
     below. Sits inside a flex-column stage so the three pieces
     stack with even spacing regardless of which states are
     showing. */
  .record-stage {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1rem;
    padding: 0.5rem 0 0.25rem;
  }

  /* Three-column row: audio source (left) | Record button (centre)
     | model chip (right). Adjuncts share equal flex weight so the
     button stays centred regardless of which adjunct is wider.
     `align-items: end` aligns the bottoms of the controls so the
     dropdown trigger and the button are on the same baseline. */
  .record-row {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: end;
    gap: 1.25rem;
    width: 100%;
  }

  /* Centre-column wrapper that gives the Record button a label
     above (matching the source/model field-label rhythm) so the
     three columns share the same visual structure: caption +
     control. */
  .record-btn-cell {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
  }
  .record-btn-label {
    font-size: 0.68rem;
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .record-row-adjunct {
    display: flex;
    min-width: 0;
  }
  .record-row-adjunct--left {
    justify-content: flex-end;
  }
  .record-row-adjunct--right {
    justify-content: flex-start;
  }
  .record-row-adjunct > :global(*) {
    width: 100%;
    max-width: 16rem;
  }

  /* Below ~520 px the three-column row would crowd the centerpiece.
     Stack instead, button on top so the visual hierarchy still
     reads, source then model below. */
  @media (max-width: 520px) {
    .record-row {
      grid-template-columns: 1fr;
      gap: 0.85rem;
      justify-items: center;
    }
    .record-row-adjunct {
      justify-content: stretch;
      width: 100%;
    }
    .record-row-adjunct > :global(*) {
      max-width: 100%;
    }
  }

  /* Meeting-only mode: override the waveform bar colour to red so
     the visual palette matches the pulsing Stop button and makes it
     immediately clear this is a meeting capture, not dictation. */
  .record-stage.meeting .record-waveform {
    --audio-waveform-bar-color: #dc2626;
  }

  /* Big waveform — overrides AudioWaveform's default 60 × 16 px
     compact strip with a content-column-filling 88 px stage so
     the bars become the visual anchor. While recording the bars
     pick up the purple → cyan gradient from the spec; idle /
     processing / error keep their muted treatments owned by
     AudioWaveform itself. */
  .record-waveform {
    width: 100%;
    --audio-waveform-width: 100%;
    --audio-waveform-height: 88px;
    --audio-waveform-bar-color: linear-gradient(
      to top,
      #8b5cf6 0%,
      #06b6d4 100%
    );
  }
  /* Bars feel taller / chunkier in the centerpiece role. */
  .record-waveform :global(.audio-waveform) {
    gap: 4px;
  }
  .record-waveform :global(.audio-waveform-bar) {
    border-radius: 3px;
  }

  /* Circular record button — fixed-size icon button, replaces
     the pre-r2 wide-pill `.start-btn`. Reads as a hardware-style
     control rather than a form button. Spring hover + press
     damping carry over from the prior treatment. */
  .record-btn {
    width: 76px;
    height: 76px;
    border-radius: 50%;
    border: 1px solid var(--border-input);
    background: var(--bg-surface);
    color: var(--text-primary);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    /* Resting shadow gives the idle button presence per the
       #468 spec ("Idle: Confident, filled. Not ghosted"). */
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.06);
    transition:
      transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1),
      border-color 150ms ease,
      background-color 150ms ease,
      box-shadow 150ms ease;
  }
  .record-btn:hover:not(:disabled) {
    transform: scale(1.04);
    border-color: var(--accent);
    box-shadow:
      0 6px 14px rgba(0, 0, 0, 0.12),
      0 0 0 3px var(--accent-subtle);
  }
  .record-btn:active:not(:disabled) {
    transform: scale(0.97);
    transition: transform 80ms ease-out;
  }
  .record-btn:focus-visible {
    outline: none;
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }
  .record-btn:disabled {
    opacity: 0.55;
    cursor: not-allowed;
    transform: none;
  }
  /* Recording state: red fill + heartbeat pulse, square stop
     glyph inside. The pulse owns the box-shadow during this
     state so hover only changes the fill colour — overriding
     the shadow would freeze the keyframe. */
  .record-btn.recording {
    background: var(--danger);
    border-color: var(--danger);
    color: white;
    animation: recording-pulse 2s ease-out infinite;
  }
  .record-btn.recording:hover:not(:disabled) {
    background: var(--danger);
    border-color: var(--danger);
  }

  /* Idle state glyph: a small filled dot — Audio Hijack-style
     "press to record" indicator. */
  .record-icon-idle {
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--danger);
    display: inline-block;
  }
  /* Recording state glyph: a small white square (universal
     "stop" affordance). */
  .record-icon-stop {
    width: 14px;
    height: 14px;
    border-radius: 2px;
    background: white;
    display: inline-block;
  }

  /* Recording-duration readout. tabular-nums prevents the digits
     from jittering horizontally as they tick up. Idle "00:00"
     reads as a quiet placeholder; the `.live` variant tints
     accent-red so the eye knows time is advancing. */
  .record-time {
    margin: 0.3rem 0 0;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
    font-size: 1.05rem;
    font-weight: 500;
    color: var(--text-secondary);
    text-align: center;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.02em;
  }
  .record-time.live {
    color: var(--danger);
  }

  /* Status label below the time — the verb / state copy. */
  .record-label {
    margin: 0;
    min-height: 1.2em;
    font-size: 0.85rem;
    color: var(--text-muted);
    text-align: center;
    line-height: 1.35;
    max-width: 30rem;
  }

  @keyframes recording-pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(216, 58, 58, 0.45);
    }
    70% {
      box-shadow: 0 0 0 14px rgba(216, 58, 58, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(216, 58, 58, 0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .record-btn,
    .record-btn:hover:not(:disabled),
    .record-btn:active:not(:disabled) {
      transform: none;
      transition: border-color 100ms ease, background-color 100ms ease;
    }
    .record-btn.recording {
      animation: none;
    }
  }

  .record-mode-hint {
    font-size: 0.78rem;
    font-weight: 500;
    padding: 0.1rem 0.5rem;
    margin-left: 0.35rem;
    background-color: var(--accent-subtle);
    color: var(--accent);
    border-radius: 999px;
    white-space: nowrap;
  }

  .status-mode {
    font-weight: 500;
    color: var(--text-secondary);
  }
  .status-mode[data-record-mode="meeting"] {
    color: var(--accent);
    font-weight: 600;
  }

  /* Spinner inside the circular button while transcribing. */
  .spinner {
    width: 22px;
    height: 22px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    display: inline-block;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
  }

  /* Thin progress bar shown under the waveform while whisper.cpp
     is running (#566). Only rendered once the first progress tick
     arrives so very short clips don't flash a 0% bar. */
  .transcription-progress-bar {
    width: 100%;
    height: 3px;
    background: var(--surface-raised, #e0e0e0);
    border-radius: 2px;
    overflow: hidden;
    margin-top: -0.5rem;
  }
  .transcription-progress-fill {
    height: 100%;
    background: var(--accent, #8b5cf6);
    border-radius: 2px;
    transition: width 0.3s ease;
  }
</style>
