<!--
  Content-column session controls: record button (with mic-only
  badge), aria-live recording status text, mood-driven waveform,
  and the F5 status line. Pulled out of `ControlsSection` in
  #468 slice B so the two-column layout (slice C) can place this
  in the content column while `AudioSourcePicker` lives in the
  sidebar.

  Several derived values (badge visibility, will-record-meeting
  hint, has-usable-source guard) live upstream — the parent owns
  the cross-component selection state and computes them once
  rather than each leaf re-deriving from `selected` + `sources`.
-->
<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import type { UnlistenFn } from "@tauri-apps/api/event";

  import AudioWaveform from "./AudioWaveform.svelte";
  import StatusLine from "./StatusLine.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import {
    listenForStatusLineChanges,
    readStatusLineEnabled,
  } from "./status-line";

  type Props = {
    recording: boolean;
    busy: boolean;
    transcribing: boolean;
    /// True iff at least one selectable, supported source exists
    /// — drives the Start button's disabled state. Computed
    /// upstream from `sources` + the system-audio capability
    /// flag.
    hasUsableSource: boolean;
    noModelInstalled: boolean;
    /// True iff a click on Record would upgrade to the meeting
    /// pump (mic + system audio) — shows the "Record meeting"
    /// label variant. Upstream derives this from the picker
    /// selection + Screen Recording health.
    willRecordMeeting: boolean;
    /// Mic-only badge: surfaces when SCK is `stale` /
    /// `not-granted` and a mic is selected. Two visual variants
    /// driven by `badgeIsStale`.
    badgeVisible: boolean;
    badgeIsStale: boolean;
    /// Active recording mode (#409) — drives the inline mode
    /// label on the recording status pill. `null` when not
    /// recording.
    recordMode: "dictation" | "meeting" | null;
    /// Display name of the active source, used by the F5 status
    /// line. Resolved upstream from `sources` + selected id so
    /// this component doesn't need either.
    selectedSourceLabel: string | null;
    activeModelName: string | null;
    /// Last error to surface. The waveform's mood derives off
    /// this — non-null flips the bars to error-flash; the parent
    /// renders the actual `<ErrorDisplay>` separately.
    error: ErrorDisplayShape | null;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    onOpenPermissions?: () => void;
  };

  let {
    recording,
    busy,
    transcribing,
    hasUsableSource,
    noModelInstalled,
    willRecordMeeting,
    badgeVisible,
    badgeIsStale,
    recordMode,
    selectedSourceLabel,
    activeModelName,
    error,
    onStart,
    onStop,
    onOpenPermissions,
  }: Props = $props();

  // F5 status line — opt-in display gated by a localStorage flag,
  // re-applied via Tauri event when the Settings toggle flips so
  // the open main window updates without reload.
  let statusLineEnabled = $state(false);
  let unlistenStatusLine: UnlistenFn | null = null;

  onMount(async () => {
    statusLineEnabled = readStatusLineEnabled();
    unlistenStatusLine = await listenForStatusLineChanges((next) => {
      statusLineEnabled = next;
    });
  });

  onDestroy(() => {
    unlistenStatusLine?.();
    unlistenStatusLine = null;
  });

  // Waveform mood priority: error > recording > processing > idle.
  // Error wins so a stop-time failure flashes the bars even while
  // `transcribing` is still true on its way down.
  let waveformMode = $derived<"idle" | "recording" | "processing" | "error">(
    error !== null
      ? "error"
      : recording
        ? "recording"
        : transcribing
          ? "processing"
          : "idle",
  );
</script>

<div class="record-stage" data-recording={recording ? "true" : "false"}>
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

  <!--
    Circular Record / Stop button — fixed-size icon button that
    sits below the waveform. Single button instance toggling its
    state class so the spring-on-hover transitions stay
    consistent across start/stop. aria-label drives test
    `getByRole("button", { name: "Start recording" })` and the
    Settings-window dictation specs.
  -->
  {#if !recording}
    <button
      class="record-btn"
      onclick={onStart}
      disabled={busy || !hasUsableSource || noModelInstalled}
      aria-label={busy
        ? "Working"
        : noModelInstalled
          ? "Choose a model first"
          : willRecordMeeting
            ? "Record meeting (mic plus system audio)"
            : "Start recording"}
      title={noModelInstalled ? "Choose a model first" : undefined}
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
      aria-label="Stop recording and transcribe"
    >
      <span class="record-icon record-icon-stop" aria-hidden="true"></span>
    </button>
  {/if}

  <!--
    Status label sits under the button — the verb the user is
    primed to do. aria-live so screen readers announce the
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
      — release hotkey or press Stop
    {:else if transcribing}
      Transcribing…
    {:else if willRecordMeeting}
      Record meeting <span class="record-mode-hint">mic + system audio</span>
    {:else if !noModelInstalled && hasUsableSource}
      Press to record
    {/if}
  </p>
</div>

{#if badgeVisible}
  <button
    type="button"
    class="record-mode-badge"
    data-health={badgeIsStale ? "stale" : "not-granted"}
    onclick={onOpenPermissions}
    aria-label="Open Permissions in Settings"
    data-testid="record-mode-badge"
  >
    <span class="record-mode-badge-dot" aria-hidden="true"></span>
    {#if badgeIsStale}
      Mic only · Screen Recording access expired — re-grant to
      also capture other people's audio in calls.
    {:else}
      Mic only · grant Screen Recording to also capture other
      people's audio in calls.
    {/if}
  </button>
{/if}

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
    background: #c02e2e;
    border-color: #c02e2e;
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

  /* Status label below the button — the verb / state copy. */
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

  .record-mode-badge {
    display: flex;
    align-items: flex-start;
    gap: 0.45rem;
    /* Use the full content-column width and align text left so the
       pre-r2 "centre an inline-flex pill" trick stops squishing the
       multi-line copy past the column boundary. */
    align-self: stretch;
    padding: 0.55rem 0.85rem;
    font-size: 0.82rem;
    line-height: 1.4;
    font-family: inherit;
    /* `--radius-md` (8 px) reads cleanly when the copy wraps;
       the pre-r2 999 px pill stretched into an oblong on
       multi-line text. */
    border-radius: var(--radius-md);
    border: 1px solid #d1d1d8;
    background-color: var(--bg-surface);
    color: var(--text-secondary);
    text-align: left;
    cursor: pointer;
    text-align: left;
    max-width: 100%;
    transition: background-color 0.12s, border-color 0.12s, color 0.12s;
  }
  .record-mode-badge:hover {
    background-color: var(--bg-elevated);
    border-color: var(--accent-hover);
    color: var(--text-primary);
  }
  .record-mode-badge:focus-visible {
    outline: none;
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }
  .record-mode-badge-dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background-color: #c0c0c5;
    flex-shrink: 0;
  }
  .record-mode-badge[data-health="stale"] .record-mode-badge-dot {
    background-color: #e0a020;
  }
  .record-mode-badge[data-health="not-granted"] .record-mode-badge-dot {
    background-color: #d83a3a;
  }
  .record-mode-badge[data-health="stale"] {
    background-color: #fdf6e3;
    border-color: #e7c887;
    color: #7a4e00;
  }
  .record-mode-badge[data-health="stale"]:hover {
    background-color: #f9efce;
    border-color: #d8b46a;
    color: #5a3700;
  }
  @media (prefers-color-scheme: dark) {
    .record-mode-badge[data-health="stale"] {
      background-color: #3d2f12;
      color: #f0c878;
      border-color: #6c4e1a;
    }
    .record-mode-badge[data-health="stale"]:hover {
      background-color: #4a3a18;
      color: #ffd790;
      border-color: #8a6520;
    }
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
</style>
