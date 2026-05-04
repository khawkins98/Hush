<!--
  Live audio-level waveform (#411 phase B + F1 moods + F4 metering).

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

  ## Metering (#411 phase F4)

  Opt-in via `metering={true}`. Adds:

  - A peak-hold line — thin horizontal marker at the highest
    `displayLevel` seen recently. Holds for `PEAK_HOLD_MS` then
    falls at `PEAK_DECAY_PER_FRAME`. Off-screen (peak === 0)
    renders nothing.
  - A clip warning — brief border flash when the level crosses
    `CLIP_THRESHOLD`. Self-clears after `CLIP_FLASH_MS`.
  - A peak-dB readout on the wrapper's `title` for tooltip-on-
    hover (re: F4's "dB readout on hover").

  Metering is a no-op outside `recording` mode so the breathing
  idle wave doesn't trip the meter.
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
    /// Phase F4: enable peak-hold marker, clip warning, and the
    /// peak-dB title readout. Off by default so the HUD's compact
    /// pill stays unchanged; the main window opts in.
    metering?: boolean;
    /// Amplification factor applied to the 0..1 RMS level before
    /// mapping to bar height %. Default 400 keeps the historical
    /// behaviour. The HUD passes 480 (~20% louder appearance) so
    /// the compact 24 px bars reach the top on normal speech
    /// without affecting the main window's 88 px waveform.
    levelScale?: number;
    /// Minimum bar height in %. Default 6 — the "honest silence"
    /// baseline on the main window's 88 px stage. The HUD passes
    /// 15 so bars render as a thin flat line rather than
    /// disappearing during gaps between words.
    silenceFloorPct?: number;
  };

  let { mode, active = true, metering = false, levelScale = 400, silenceFloorPct = 6 }: Props = $props();

  let effectiveMode = $derived<WaveformMode>(
    mode ?? (active ? "recording" : "idle"),
  );

  // 14 bars × 80 ms push interval = ~1.1 s visible window.
  const BAR_COUNT = 14;
  const ATTACK = 0.6;
  const RELEASE = 0.12;
  const WAVEFORM_INTERVAL_MS = 80;

  // Idle: bars sit at the minimum render height. Pre-r2 ran a
  // 2-second sine breath; that misread as "the app is listening"
  // since no audio is being captured. The next pass set a static
  // 0.06 baseline — flat but still chunky on the 88 px stage,
  // reading as "low signal" rather than "silent." Now zeroed so
  // the bar heights fall to the 6 % floor in the render math —
  // ~5 px on the centerpiece scale, an honest silence.
  const IDLE_BASELINE = 0;

  // Error flash: long enough to register, short enough that the
  // surrounding error message becomes the focal point.
  const ERROR_FLASH_MS = 600;

  // Peak-hold: holds at the high-water mark for ~800 ms before
  // falling. Per-frame decay coefficient at ~60 Hz works out to
  // roughly -0.9 dB/s — fast enough to track varying speech, slow
  // enough to read as a held "you peaked here" marker.
  const PEAK_HOLD_MS = 800;
  const PEAK_DECAY_PER_FRAME = 0.985;
  // Clip warning: fires when the post-envelope level crosses the
  // threshold. 0.9 leaves headroom — RMS values from the backend
  // pump rarely sit at 1.0 even on saturated inputs, so a stricter
  // ceiling would never fire. 250 ms is enough to register without
  // strobing.
  const CLIP_THRESHOLD = 0.9;
  const CLIP_FLASH_MS = 250;

  let rms = $state(0);
  let displayLevel = $state(0);
  let waveform = $state<number[]>(new Array(BAR_COUNT).fill(0));
  let flashing = $state(false);
  let peak = $state(0);
  let clipping = $state(false);

  let unlistenLevel: UnlistenFn | null = null;
  let raf: number | undefined;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  let clipTimer: ReturnType<typeof setTimeout> | null = null;
  let peakSetAt = 0;

  // Map a 0..1 linear level to a peak-dB string. Floor at -60 dB
  // because anything below that is effectively silence and the
  // numeric value isn't useful in a tooltip.
  let peakDbLabel = $derived.by(() => {
    if (!metering || peak <= 0.001) return "";
    const db = 20 * Math.log10(peak);
    const clamped = Math.max(-60, db);
    return `Peak: ${clamped.toFixed(1)} dB`;
  });

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
        // Static low baseline — see comment near IDLE_BASELINE
        // for why we no longer animate at idle.
        target = IDLE_BASELINE;
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

      // F4 metering — only meaningful while we're tracking live
      // audio. Idle's sine-wave target would otherwise paint a
      // bogus -28 dB peak while nothing's recording.
      if (metering && effectiveMode === "recording") {
        if (displayLevel > peak) {
          peak = displayLevel;
          peakSetAt = now;
        } else if (now - peakSetAt > PEAK_HOLD_MS) {
          peak *= PEAK_DECAY_PER_FRAME;
          if (peak < 0.001) peak = 0;
        }
        if (displayLevel >= CLIP_THRESHOLD && !clipping) {
          clipping = true;
          if (clipTimer !== null) clearTimeout(clipTimer);
          clipTimer = setTimeout(() => {
            clipping = false;
          }, CLIP_FLASH_MS);
        }
      } else if (peak > 0) {
        peak = 0;
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
    if (clipTimer !== null) {
      clearTimeout(clipTimer);
      clipTimer = null;
    }
  });
</script>

<div
  class="audio-waveform"
  data-testid="audio-waveform"
  data-mode={effectiveMode}
  data-metering={metering ? "on" : null}
  class:flashing
  class:clipping
  title={peakDbLabel || undefined}
  role="presentation"
>
  {#each waveform as level, i (i)}
    {@const heightPct = Math.min(100, Math.max(silenceFloorPct, level * levelScale))}
    <span class="audio-waveform-bar" style="height: {heightPct}%"></span>
  {/each}
  {#if metering && peak > 0.05}
    {@const peakPct = Math.min(100, Math.max(silenceFloorPct, peak * levelScale))}
    <span
      class="audio-waveform-peak"
      data-testid="audio-waveform-peak"
      style="bottom: {peakPct}%"
      aria-hidden="true"
    ></span>
  {/if}
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
    position: relative;
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

  /* F4: peak-hold line, painted as a thin absolutely-positioned
     marker so it floats above the bars without disturbing flex
     layout. Horizontal extent matches the bar strip. The bottom
     offset is set inline by the script in % units, anchored to
     the wrapper bottom so the line tracks the same bar-height
     scale as the bars themselves. */
  .audio-waveform-peak {
    position: absolute;
    left: 0;
    right: 0;
    height: 1px;
    background: var(--audio-waveform-peak-color, rgba(255, 255, 255, 0.85));
    pointer-events: none;
    transition: bottom 80ms linear;
    will-change: bottom;
  }

  /* F4 clip warning — thin red outline on the wrapper for
     CLIP_FLASH_MS. Border, not background, so the bars still read
     through clearly. Padded out via outline so the wrapper's
     inline width doesn't shift on the flash on/off transition. */
  .audio-waveform.clipping {
    outline: 1px solid var(--danger, #d92626);
    outline-offset: 1px;
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
     processing pulse, error flash, and peak-line glide also
     collapse. */
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
    .audio-waveform-peak {
      transition: none;
    }
  }
</style>
