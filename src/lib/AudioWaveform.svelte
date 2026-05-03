<!--
  Live audio-level waveform (#411 phase B + phase F1 moods).

  Self-contained leaf that subscribes to the backend's `audio:level`
  pump (~30 Hz RMS samples in [0, 1]), runs an attack/release
  envelope over the raw value, and writes the smoothed result into
  a small ring buffer. The component renders one centered bar per
  buffer entry; the most recent sample is the rightmost bar.

  Two consumers today: the HUD pill (always rendered while
  `hudState === "recording"`) and the main window's recording
  status row.

  Lives in $lib so future surfaces (a future quick-settings dialog,
  a feedback toast, etc.) can drop it in without re-deriving the
  smoothing constants. The bar palette is overridable via the
  `--audio-waveform-bar-color` custom property; default is the HUD's
  red gradient so the existing pill renders unchanged.

  Constants extracted from the HUD's pre-#411 inline implementation
  to keep the visual rhythm identical: ATTACK 0.6 / RELEASE 0.12
  feel "instant on speech, slow to fall on silence between words";
  WAVEFORM_INTERVAL_MS 80 (~12 Hz pushes against a 60 Hz rAF) keeps
  the bars moving across the strip without blur.

  ## Moods (#411 phase F1)

  The waveform expresses app state, not just live audio level. The
  `mode` prop selects one of four behaviours:

  - `idle`        — bars track a slow, low-amplitude sine
                    oscillation so an always-mounted waveform still
                    feels alive while nothing's recording. Bars
                    are dimmed.
  - `recording`   — live RMS feed via attack/release envelope
                    (the historical behaviour).
  - `processing`  — the ring buffer freezes and the strip pulses
                    opacity. Communicates "still working, not
                    capturing" across the transcription gap
                    without a layout shift.
  - `error`       — bars flash red then settle to idle. Triggered
                    once per mode → error transition; the parent
                    can hold mode === "error" indefinitely while
                    its surrounding error UI is visible.

  Back-compat: the legacy `active` prop is honoured when `mode` is
  not given. `active=true` ⇒ `recording`, `active=false` ⇒ `idle`,
  matching the pre-F1 track-vs-flatten semantics for callers that
  haven't migrated.
-->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import { Events } from "./events";

  export type WaveformMode = "idle" | "recording" | "processing" | "error";

  type Props = {
    /// Explicit mood. When omitted, falls back to the legacy
    /// `active` flag so existing call sites keep working.
    mode?: WaveformMode;
    /// Legacy gating flag (#411 phase B). `true` ⇒ recording mood,
    /// `false` ⇒ idle. Ignored when `mode` is set.
    active?: boolean;
  };

  let { mode, active = true }: Props = $props();

  let effectiveMode = $derived<WaveformMode>(
    mode ?? (active ? "recording" : "idle"),
  );

  // 14 bars × 80 ms push interval = ~1.1 s visible window.
  const BAR_COUNT = 14;
  const ATTACK = 0.6;
  const RELEASE = 0.12;
  const WAVEFORM_INTERVAL_MS = 80;

  // Idle breathing: low-amplitude sine wave. 2 s cycle is slower
  // than typical UI motion so it reads as ambient rather than
  // active.
  const IDLE_BASELINE = 0.06;
  const IDLE_AMPLITUDE = 0.04;
  const IDLE_PERIOD_MS = 2000;

  // Error flash: long enough to register, short enough that the
  // surrounding error message becomes the focal point.
  const ERROR_FLASH_MS = 600;

  let rms = $state(0);
  let displayLevel = $state(0);
  let waveform = $state<number[]>(new Array(BAR_COUNT).fill(0));
  let flashing = $state(false);

  let unlistenLevel: UnlistenFn | null = null;
  let raf: number | undefined;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;

  onMount(async () => {
    unlistenLevel = await listen<number>(Events.AudioLevel, (event) => {
      rms = event.payload ?? 0;
    });

    let lastWaveformPush = 0;
    const tick = () => {
      // Processing: freeze the buffer entirely. The CSS pulse on
      // the wrapper carries the "still alive" signal so the
      // user gets continuous feedback across the transcription
      // gap without a layout shift.
      if (effectiveMode === "processing") {
        raf = requestAnimationFrame(tick);
        return;
      }

      let target: number;
      if (effectiveMode === "idle" || effectiveMode === "error") {
        const phase = (Date.now() % IDLE_PERIOD_MS) / IDLE_PERIOD_MS;
        target = IDLE_BASELINE + Math.sin(phase * Math.PI * 2) * IDLE_AMPLITUDE;
      } else {
        target = rms;
      }

      const coeff = target > displayLevel ? ATTACK : RELEASE;
      displayLevel += (target - displayLevel) * coeff;

      const now = Date.now();
      if (now - lastWaveformPush >= WAVEFORM_INTERVAL_MS) {
        waveform = [...waveform.slice(1), displayLevel];
        lastWaveformPush = now;
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
  });

  // Trigger the one-shot flash on each mode → error transition.
  // The CSS class governs the visual; the timer just clears it so
  // a long-held error state doesn't keep flashing the bars red.
  $effect(() => {
    if (effectiveMode === "error") {
      flashing = true;
      if (flashTimer !== null) clearTimeout(flashTimer);
      flashTimer = setTimeout(() => {
        flashing = false;
      }, ERROR_FLASH_MS);
    }
  });

  onDestroy(() => {
    unlistenLevel?.();
    unlistenLevel = null;
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
    if (flashTimer !== null) {
      clearTimeout(flashTimer);
      flashTimer = null;
    }
  });
</script>

<div
  class="audio-waveform"
  data-testid="audio-waveform"
  data-mode={effectiveMode}
  class:flashing
  role="presentation"
>
  {#each waveform as level, i (i)}
    {@const heightPct = Math.min(100, Math.max(6, level * 400))}
    <span class="audio-waveform-bar" style="height: {heightPct}%"></span>
  {/each}
</div>

<style>
  .audio-waveform {
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    width: var(--audio-waveform-width, 60px);
    height: var(--audio-waveform-height, 16px);
    gap: 1px;
    flex-shrink: 0;
  }
  .audio-waveform-bar {
    flex: 1 1 0;
    min-width: 2px;
    background: var(
      --audio-waveform-bar-color,
      linear-gradient(180deg, #ff8080 0%, #ff4040 100%)
    );
    border-radius: 1px;
    transition: height 60ms linear;
    will-change: height;
  }

  /* Idle: dim the bars so the breathing oscillation reads as
     ambient rather than as captured signal. Gives consumers a
     clear "nothing's happening yet" idiom. */
  .audio-waveform[data-mode="idle"] .audio-waveform-bar {
    background: var(--text-muted, #888);
    opacity: 0.45;
  }

  /* Processing: bars are JS-frozen; the wrapper opacity pulse is
     the live signal. 1.4 s cycle matches a "I'm working" tempo
     close to the existing meeting-pump shimmer. */
  .audio-waveform[data-mode="processing"] {
    animation: audio-waveform-processing 1.4s ease-in-out infinite;
  }
  .audio-waveform[data-mode="processing"] .audio-waveform-bar {
    background: var(--text-muted, #888);
    opacity: 0.85;
    transition: none;
  }

  /* Error: settled (post-flash) the bars look like idle so the
     surrounding error UI does the talking. While `flashing` is
     set the danger token paints over the bars. */
  .audio-waveform[data-mode="error"] .audio-waveform-bar {
    background: var(--text-muted, #888);
    opacity: 0.45;
  }
  .audio-waveform.flashing .audio-waveform-bar {
    background: var(--danger, #d92626);
    opacity: 1;
    transition: background 80ms linear, opacity 80ms linear;
  }

  @keyframes audio-waveform-processing {
    0%, 100% { opacity: 0.85; }
    50% { opacity: 0.45; }
  }

  /* Reduced-motion: keep the bars but drop the inter-sample
     glide. Same policy as the HUD's pre-#411 inline rule and the
     dot pulse — convey the signal, skip the motion. The
     processing pulse and error flash also collapse. */
  @media (prefers-reduced-motion: reduce) {
    .audio-waveform-bar {
      transition: none;
    }
    .audio-waveform[data-mode="processing"] {
      animation: none;
      opacity: 0.7;
    }
    .audio-waveform.flashing .audio-waveform-bar {
      transition: none;
    }
  }
</style>
