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

{#if !recording}
  <button
    class="start-btn"
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
      <span class="spinner" aria-hidden="true"></span> Transcribing…
    {:else if willRecordMeeting}
      <span class="rec-dot idle" aria-hidden="true"></span> Record meeting
      <span class="record-mode-hint">mic + system audio</span>
    {:else}
      <span class="rec-dot idle" aria-hidden="true"></span> Start recording
    {/if}
  </button>
{:else}
  <button
    class="start-btn stop"
    onclick={onStop}
    disabled={busy}
    aria-label="Stop recording and transcribe"
  >
    <span class="rec-dot stop" aria-hidden="true"></span> Stop and transcribe
  </button>
{/if}

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

<p class="status" aria-live="polite">
  {#if recording}
    <span class="recording-dot" aria-hidden="true"></span> Recording
    {#if recordMode === "meeting"}
      <span class="status-mode" data-record-mode="meeting"
        >· mic + system audio</span
      >
    {:else if recordMode === "dictation"}
      <span class="status-mode" data-record-mode="dictation"
        >· mic only</span
      >
    {/if}
    — release the hotkey or press Stop to transcribe.
  {:else if transcribing}
    Transcribing — this can take a few seconds for short clips,
    longer for big models.
  {/if}
</p>

<div class="status-waveform">
  <AudioWaveform mode={waveformMode} metering />
</div>

{#if statusLineEnabled}
  <StatusLine
    audioSourceLabel={selectedSourceLabel}
    modelName={activeModelName}
  />
{/if}

<style>
  /* Record button — Panic spring on hover; Rogue Amoeba live-
     indicator pulse while recording. */
  .start-btn {
    border-radius: var(--radius-md);
    border: 1px solid var(--border-input);
    height: var(--control-height);
    padding: 0 1.2em;
    font-size: 1em;
    font-family: inherit;
    color: var(--text-primary);
    background-color: var(--bg-surface);
    cursor: pointer;
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
    /* Panic-flavoured overshoot easing on the transform — tiny
       "pop" at the top of the hover scale, physical not linear.
       Other property transitions stay ease-y. */
    transition:
      transform 200ms cubic-bezier(0.34, 1.56, 0.64, 1),
      border-color 150ms ease,
      background-color 150ms ease,
      box-shadow 150ms ease;
    width: 100%;
  }
  .start-btn:hover:not(:disabled) {
    transform: scale(1.02);
    border-color: var(--accent-hover);
    box-shadow:
      0 2px 6px rgba(0, 0, 0, 0.18),
      0 0 0 3px var(--accent-subtle);
  }
  .start-btn:active:not(:disabled) {
    /* Press damping — lands the spring on click rather than
       leaving the button in its hover-scaled state mid-press. */
    transform: scale(0.99);
    transition: transform 80ms ease-out;
  }
  .start-btn:focus-visible {
    outline: none;
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }
  .start-btn:disabled {
    opacity: 0.55;
    cursor: not-allowed;
    transform: none;
  }
  .start-btn.stop {
    background-color: var(--danger);
    color: white;
    border-color: var(--danger);
    /* Rogue Amoeba live-indicator pulse — Stop IS the live
       recording marker. One slow heartbeat / 2 s reads as
       "active" without strobing. */
    animation: recording-pulse 2s ease-out infinite;
  }
  .start-btn.stop:hover:not(:disabled) {
    background-color: #c02e2e;
    border-color: #c02e2e;
    /* Recording-state hover keeps the pulse — overriding
       box-shadow would freeze the keyframe. Only the colours
       shift on hover here. */
  }

  @keyframes recording-pulse {
    0% {
      box-shadow: 0 0 0 0 rgba(216, 58, 58, 0.45);
    }
    70% {
      box-shadow: 0 0 0 8px rgba(216, 58, 58, 0);
    }
    100% {
      box-shadow: 0 0 0 0 rgba(216, 58, 58, 0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .start-btn,
    .start-btn:hover:not(:disabled),
    .start-btn:active:not(:disabled) {
      transform: none;
      transition: border-color 100ms ease, background-color 100ms ease;
    }
    .start-btn.stop {
      animation: none;
    }
  }

  .record-mode-hint {
    font-size: 0.78rem;
    font-weight: 500;
    padding: 0.1rem 0.5rem;
    margin-left: 0.45rem;
    background-color: var(--accent-subtle);
    color: var(--accent);
    border-radius: 999px;
    white-space: nowrap;
  }

  .rec-dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    display: inline-block;
    flex-shrink: 0;
  }
  .rec-dot.idle {
    background-color: var(--text-secondary);
    opacity: 0.6;
  }
  .rec-dot.stop {
    background-color: white;
  }

  .record-mode-badge {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    align-self: center;
    padding: 0.4rem 0.75rem;
    font-size: 0.82rem;
    line-height: 1.35;
    font-family: inherit;
    border-radius: 999px;
    border: 1px solid #d1d1d8;
    background-color: var(--bg-surface);
    color: var(--text-secondary);
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

  .status {
    margin: 0;
    min-height: 1.4em;
    font-size: 0.88rem;
    color: var(--text-muted);
    text-align: center;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.45rem;
  }
  .recording-dot {
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 50%;
    background-color: var(--danger);
    display: inline-block;
    animation: pulse 1.2s ease-in-out infinite;
  }
  .status-mode {
    font-weight: 500;
    color: var(--text-secondary);
  }
  .status-mode[data-record-mode="meeting"] {
    color: var(--accent);
    font-weight: 600;
  }

  .status-waveform {
    display: flex;
    justify-content: center;
    margin-top: 0.5rem;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }

  @media (prefers-reduced-motion: reduce) {
    .recording-dot,
    .spinner {
      animation: none;
    }
  }

  .spinner {
    width: 0.85rem;
    height: 0.85rem;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    display: inline-block;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
