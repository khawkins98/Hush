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
  - A visible current-dB label overlaid in the top-right corner
    of the waveform container so the level is readable at a
    glance while recording. Uses integer precision (floor -60 dB)
    with a semi-transparent backdrop pill for legibility on both
    light and dark themes.

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
    /// mapping to bar height %. Only used when `logScale` is false.
    /// Default 400 keeps the historical linear behaviour.
    levelScale?: number;
    /// Minimum bar height in %. Default 6 — the "honest silence"
    /// baseline on the main window's 88 px stage. The HUD passes
    /// 15 so bars render as a thin flat line rather than
    /// disappearing during gaps between words.
    silenceFloorPct?: number;
    /// Use a logarithmic (dBFS) scale for bar heights instead of
    /// the linear `level * levelScale` mapping. Default true.
    ///
    /// Motivation: speech sits between −50 and −6 dBFS. Linear
    /// amplitude at −38 dB is only ~1.3 % of full-scale, so even a
    /// 400× multiplier yields a barely-visible ~5 % bar. The log
    /// scale maps −60 dB → silenceFloorPct and −3 dB → 100 %, so
    /// normal conversational speech (~−38 dB) renders at ~38 %
    /// height — clearly visible without distorting the loud end.
    logScale?: boolean;
  };

  let {
    mode,
    active = true,
    metering = false,
    levelScale = 400,
    silenceFloorPct = 6,
    logScale = true,
  }: Props = $props();

  let effectiveMode = $derived<WaveformMode>(
    mode ?? (active ? "recording" : "idle"),
  );

  // Logarithmic + adaptive scaling constants.
  //
  // DB_FLOOR: anything quieter than this maps to silenceFloorPct.
  // DB_CEIL_DEFAULT: starting / maximum ceiling (very loud mic or
  //   first few frames before the adaptive tracker has data).
  // DB_CEIL_MIN: adaptive ceiling never goes below this, keeping
  //   the scale sane even on extremely quiet sources (< -50 dBFS).
  // ADAPTIVE_HEADROOM_DB: the ceiling sits this many dB above the
  //   tracked signal peak so bars don't permanently rail at 100 %.
  // ADAPTIVE_ATTACK: how quickly the ceiling rises to a new loud
  //   level (per rAF frame at ~60 Hz; 0.15 ≈ 3-4 frames = ~60 ms).
  // ADAPTIVE_RELEASE: how slowly the ceiling falls back during
  //   silence (0.0015/frame ≈ 11 s from −6 to −60 dBFS). Slow
  //   decay means a burst of loud speech doesn't shrink the scale
  //   the moment the speaker pauses between words.
  const DB_FLOOR = -70;
  const DB_CEIL_DEFAULT = -3;
  const DB_CEIL_MIN = -48;
  const ADAPTIVE_HEADROOM_DB = 6;
  const ADAPTIVE_ATTACK = 0.15;
  const ADAPTIVE_RELEASE = 0.0015;

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
  // Adaptive ceiling tracker. Initialised to 0.01 (≈ −40 dBFS) so
  // the first frame of speech doesn't rail the bars, then adapts
  // within ~60 ms to whatever level the mic or system audio is
  // actually delivering.
  let adaptivePeak = $state(0.01);

  let unlistenLevel: UnlistenFn | null = null;
  let raf: number | undefined;
  let flashTimer: ReturnType<typeof setTimeout> | null = null;
  let clipTimer: ReturnType<typeof setTimeout> | null = null;
  let peakSetAt = 0;

  // Fun loudness-context labels keyed by dBFS thresholds.
  // Ranges are intentionally coarse — the goal is a smile,
  // not metrological accuracy.
  function dbContext(db: number): string {
    if (db < -50) return "near silence 🤫";
    if (db < -40) return "rustling leaves 🍃";
    if (db < -30) return "quiet library 📚";
    if (db < -20) return "soft whisper 🌬️";
    if (db < -12) return "normal conversation 💬";
    if (db <  -6) return "raised voice 📣";
    if (db <  -3) return "lawnmower 🌿";
    if (db <  -1) return "baby crying 👶";
    return "rock concert 🎸";
  }

  // Map a 0..1 linear level to a peak-dB string. Floor at -60 dB
  // because anything below that is effectively silence and the
  // numeric value isn't useful in a tooltip.
  let peakDbLabel = $derived.by(() => {
    if (!metering || peak <= 0.001) return "";
    const db = 20 * Math.log10(peak);
    const clamped = Math.max(-60, db);
    return `Peak: ${clamped.toFixed(1)} dB`;
  });

  // Visible current-level dB label (integer, -60 dB floor).
  // Throttled to 200 ms so the readout is legible without strobing.
  // Only renders while metering + recording so idle/processing/
  // error states don't show a bogus number.
  let currentDbLabel = $state("");
  let lastDbLabelUpdateMs = 0;

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

      // Adaptive ceiling — only updated during recording so a long
      // silence between sessions doesn't quietly shrink the scale.
      if (logScale && effectiveMode === "recording") {
        if (displayLevel > adaptivePeak) {
          adaptivePeak += (displayLevel - adaptivePeak) * ADAPTIVE_ATTACK;
        } else {
          adaptivePeak = Math.max(
            0.001,
            adaptivePeak + (displayLevel - adaptivePeak) * ADAPTIVE_RELEASE,
          );
        }
      }

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
        if (now - lastDbLabelUpdateMs >= 200) {
          lastDbLabelUpdateMs = now;
          const db = 20 * Math.log10(Math.max(displayLevel, 0.001));
          const dbInt = Math.max(-60, db);
          currentDbLabel = displayLevel > 0.001
            ? `${dbInt.toFixed(0)} dB · ${dbContext(dbInt)}`
            : "";
        }
      } else {
        if (peak > 0) peak = 0;
        if (currentDbLabel !== "") currentDbLabel = "";
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
    {@const heightPct = (() => {
      if (!logScale) return Math.min(100, Math.max(silenceFloorPct, level * levelScale));
      if (level <= 0.001) return silenceFloorPct;
      const db = 20 * Math.log10(level);
      const adaptivePeakDb = 20 * Math.log10(adaptivePeak);
      const dynamicCeil = Math.min(
        DB_CEIL_DEFAULT,
        Math.max(DB_CEIL_MIN, adaptivePeakDb + ADAPTIVE_HEADROOM_DB),
      );
      const normalized = (Math.max(DB_FLOOR, db) - DB_FLOOR) / (dynamicCeil - DB_FLOOR);
      return Math.min(100, Math.max(silenceFloorPct, normalized * 100));
    })()}
    <span class="audio-waveform-bar" style="height: {heightPct}%"></span>
  {/each}
  {#if currentDbLabel !== ""}
    <span class="audio-waveform-db" aria-hidden="true">{currentDbLabel}</span>
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

  /* dB readout — compact current-level label overlaid below the
     waveform bars. Positioned at the bottom so the wider context
     string doesn't overlap the bars; left-anchored so it reads
     naturally and grows rightward into open space. */
  .audio-waveform-db {
    position: absolute;
    bottom: -1.35rem;
    left: 0;
    font-size: 0.65rem;
    font-variant-numeric: tabular-nums;
    line-height: 1;
    padding: 2px 5px;
    border-radius: 3px;
    background: rgba(0, 0, 0, 0.35);
    color: rgba(255, 255, 255, 0.92);
    pointer-events: none;
    user-select: none;
    white-space: nowrap;
    letter-spacing: 0.01em;
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
  }
</style>
