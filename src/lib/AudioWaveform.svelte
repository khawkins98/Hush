<!--
  Live audio-level waveform (#411 phase B).

  Self-contained leaf that subscribes to the backend's `audio:level`
  pump (~30 Hz RMS samples in [0, 1]), runs an attack/release
  envelope over the raw value, and writes the smoothed result into
  a small ring buffer. The component renders one centered bar per
  buffer entry; the most recent sample is the rightmost bar.

  Two consumers today: the HUD pill (always rendered while
  `hudState === "recording"`) and the main window's recording
  status row (rendered while `recording === true`). Both pass
  `active` to gate the animation — when `active` flips false the
  ring buffer is flushed to flat so the next recording starts at
  baseline rather than picking up where the previous run ended.

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
-->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";
  import { Events } from "./events";

  type Props = {
    /// When true, the waveform tracks the live RMS feed. When
    /// false, the target collapses to 0 and the ring buffer
    /// flattens — pre-#411 the HUD inlined this same flush on
    /// the recording → processing transition to keep a stray
    /// late-arriving level event from briefly relighting the
    /// bar. Default true so the simplest consumer ("show me a
    /// waveform whenever this is mounted") doesn't have to
    /// thread a flag.
    active?: boolean;
  };

  let { active = true }: Props = $props();

  // 14 bars × 80 ms push interval = ~1.1 s visible window. Enough
  // to capture the rhythm of speech, short enough that the bars
  // visibly scroll. Locked as a constant rather than a prop —
  // both consumers want the same density today and a one-off
  // re-derivation isn't worth a public API surface.
  const BAR_COUNT = 14;
  const ATTACK = 0.6;
  const RELEASE = 0.12;
  const WAVEFORM_INTERVAL_MS = 80;

  let rms = $state(0);
  let displayLevel = $state(0);
  let waveform = $state<number[]>(new Array(BAR_COUNT).fill(0));

  let unlistenLevel: UnlistenFn | null = null;
  let raf: number | undefined;

  onMount(async () => {
    unlistenLevel = await listen<number>(Events.AudioLevel, (event) => {
      rms = event.payload ?? 0;
    });

    let lastWaveformPush = 0;
    const tick = () => {
      // When inactive, decay toward zero rather than tracking the
      // (possibly still-arriving) live level. Same envelope so
      // the visual settle on flip-off matches the settle on a
      // genuine drop in mic level.
      const target = active ? rms : 0;
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

  // Hard-flush on flip-off so a back-to-back re-arming starts the
  // ring buffer at flat rather than wherever the decay envelope
  // had landed. Matches the HUD's pre-#411 explicit reset on the
  // recording → processing transition.
  $effect(() => {
    if (!active) {
      waveform = new Array(BAR_COUNT).fill(0);
      displayLevel = 0;
      rms = 0;
    }
  });

  onDestroy(() => {
    unlistenLevel?.();
    unlistenLevel = null;
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
  });
</script>

<div
  class="audio-waveform"
  data-testid="audio-waveform"
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

  /* Reduced-motion: keep the bars but drop the inter-sample
     glide. Same policy as the HUD's pre-#411 inline rule and the
     dot pulse — convey the signal, skip the motion. */
  @media (prefers-reduced-motion: reduce) {
    .audio-waveform-bar {
      transition: none;
    }
  }
</style>
