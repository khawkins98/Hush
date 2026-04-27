<!--
  Recording HUD overlay. Loaded into the secondary `hud` Tauri
  window (label `hud`, configured in `tauri.conf.json`) — borderless,
  transparent, always-on-top. The window is hidden by default and
  shown/hidden by the backend `hud::show` / `hud::hide` calls in
  the IPC commands' `start_dictation` / `stop_dictation` paths.

  Renders a pulsing red dot + the word "Recording" + a level-meter
  bar driven by `audio:level` events. The backend pump (in
  `lib.rs::run`) emits an RMS sample at ~30 Hz; the bar's width is
  a simple amplification of that value, capped at 100 %.

  Why a separate route rather than reusing the main page in a
  different mode: the HUD's window config differs significantly
  (transparent, no decorations, not in the taskbar). Reusing
  `+page.svelte` would mean rendering the entire dictation UI inside
  the HUD window, which gets ignored but still pulls in code +
  fetches. A dedicated minimal page is faster to load and easier
  to reason about.
-->
<script lang="ts">
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onMount } from "svelte";

  // Latest RMS from the backend pump, in roughly [0, 1]. We hold
  // it as a runes-state float and let the meter's CSS width track
  // it directly. No throttling on the receive side — the backend
  // already throttles to ~30 Hz.
  let rms = $state(0);

  // Smoothed display value with a simple attack/release envelope so
  // the bar doesn't jitter on every callback. Attack is fast (the
  // user expects an instant reaction when they speak); release is
  // slow (silence between words shouldn't drop the bar to 0).
  let displayLevel = $state(0);

  onMount(() => {
    let unlisten: UnlistenFn | undefined;
    let raf: number | undefined;

    const ATTACK = 0.6;
    const RELEASE = 0.12;

    const tick = () => {
      const target = rms;
      const coeff = target > displayLevel ? ATTACK : RELEASE;
      displayLevel += (target - displayLevel) * coeff;
      raf = requestAnimationFrame(tick);
    };

    listen<number>("audio:level", (event) => {
      rms = event.payload ?? 0;
    }).then((fn) => {
      unlisten = fn;
    });

    raf = requestAnimationFrame(tick);

    return () => {
      unlisten?.();
      if (raf !== undefined) cancelAnimationFrame(raf);
    };
  });

  // Map RMS roughly into a visual bar fill. RMS for normal speech
  // sits in 0.05–0.2; we boost ×4 so casual talking lights the bar
  // about half-way and shouting saturates it. Capped at 100 %.
  let barWidth = $derived(Math.min(100, Math.max(0, displayLevel * 400)));

  // Dismiss the HUD without affecting the in-flight recording. The
  // backend's `hud::show` is the only thing that re-shows it, so
  // dismiss is a one-session opt-out: the next dictation/meeting
  // start will re-show the HUD on its own.
  async function dismiss() {
    try {
      await getCurrentWebviewWindow().hide();
    } catch {
      // Hide failure is non-fatal — recording continues regardless.
    }
  }
</script>

<!--
  `data-tauri-drag-region` on the root makes the whole pill act as a
  window-drag handle (Tauri 2 idiom; replaces the older
  `-webkit-app-region: drag` CSS that had macOS quirks). The dismiss
  button opts out via `data-tauri-drag-region="false"` so a click
  hides instead of starting a drag.
-->
<!--
  `role="status"` + `aria-live="polite"` so a screen reader hears
  "Recording" when the HUD appears, without re-announcing on every
  level-meter tick. The dismiss button inside is a real focusable
  control with its own aria-label; the previous `aria-hidden="true"`
  on the root masked everything (including the dismiss button) from
  AT, which we never wanted.
-->
<div
  class="hud-root"
  data-tauri-drag-region
  role="status"
  aria-live="polite"
  aria-label="Recording in progress"
>
  <span class="hud-dot"></span>
  <span class="hud-label">Recording</span>
  <div class="hud-meter" role="presentation">
    <div class="hud-meter-fill" style="width: {barWidth}%"></div>
  </div>
  <button
    type="button"
    class="hud-dismiss"
    aria-label="Hide recording overlay (recording continues)"
    title="Hide overlay"
    onclick={dismiss}
  >
    <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
      <path d="M2 2 L10 10 M10 2 L2 10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
    </svg>
  </button>
</div>

<style>
  /* Reset the body so the transparent window genuinely shows
     through. The default html / body background is white; we want
     transparent. */
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: transparent;
    overflow: hidden;
    color: white;
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
      Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  .hud-root {
    /* Centred in the small HUD window with a rounded pill background.
       The pill keeps the HUD readable on top of any desktop colour
       (black background = good on light desktops; semi-transparent
       so it doesn't feel heavyweight on dark desktops). */
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.65rem;
    height: 100vh;
    width: 100vw;
    background-color: rgba(15, 15, 15, 0.82);
    border-radius: 999px;
    /* The Tauri window itself is a rectangle; this pill draws the
       "shape" inside that rectangle. Subtle border keeps the edge
       visible against busy backgrounds. */
    border: 1px solid rgba(255, 255, 255, 0.12);
    /* Use box-shadow as a soft drop shadow rather than the OS
       window shadow (which is disabled in tauri.conf.json so the
       transparent window's edges aren't rectangular-shadow'd). */
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
    user-select: none;
    /* Drag handling is via `data-tauri-drag-region` on the markup —
       Tauri 2's preferred path. The cursor hint here makes the
       drag affordance discoverable. */
    cursor: grab;
  }
  .hud-root:active {
    cursor: grabbing;
  }

  /* Dismiss button. Sits flush to the right edge of the pill;
     ghosted by default and lit on hover so it doesn't compete with
     the recording dot for attention. */
  .hud-dismiss {
    margin-left: auto;
    padding: 0;
    width: 18px;
    height: 18px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background-color: rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.6);
    border-radius: 50%;
    cursor: pointer;
    transition: background-color 0.12s, color 0.12s;
  }
  .hud-dismiss:hover {
    background-color: rgba(255, 255, 255, 0.18);
    color: rgba(255, 255, 255, 0.95);
  }
  .hud-dismiss:focus-visible {
    outline: 2px solid rgba(255, 255, 255, 0.6);
    outline-offset: 1px;
  }

  .hud-dot {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 50%;
    background-color: #ff4040;
    box-shadow: 0 0 8px rgba(255, 64, 64, 0.55);
    animation: hud-pulse 1.2s ease-in-out infinite;
  }

  .hud-label {
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  /* Level meter. A short bar to the right of the label. The pill is
     small, so the meter is small too — its job is to convey "Hush
     is hearing you", not give a precise readout. The track is faint
     and the fill is a soft red gradient that matches the dot. */
  .hud-meter {
    width: 60px;
    height: 6px;
    background-color: rgba(255, 255, 255, 0.12);
    border-radius: 3px;
    overflow: hidden;
  }

  .hud-meter-fill {
    height: 100%;
    background: linear-gradient(90deg, #ff8080 0%, #ff4040 100%);
    border-radius: 3px;
    /* Width is set inline by the rAF envelope; no CSS transition
       so the smoothing stays in the JS attack/release loop and
       isn't double-smoothed by the renderer. */
    will-change: width;
  }

  /* Reduced-motion users still see the meter, but at the unsmoothed
     RMS — same behaviour as the dot's animation: convey the signal,
     skip the motion. The width still updates per `audio:level`
     event, just without the per-frame envelope. */
  @media (prefers-reduced-motion: reduce) {
    .hud-meter-fill {
      transition: none;
    }
  }

  @keyframes hud-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }

  @media (prefers-reduced-motion: reduce) {
    .hud-dot {
      animation: none;
    }
  }

  /*
    Light-desktop / light-OS-theme override. The pill stays dark
    (it's the contrast carrier for the white text + red dot), but
    the dot's red glow is bumped to nearly-opaque so it stays
    visible against a light desktop wallpaper, and the pill border
    flips to a darker rgba so the rectangle edge isn't lost on a
    bright background. Round-4 reviewer flagged the dim glow on
    light desktops; this is the targeted fix.
  */
  @media (prefers-color-scheme: light) {
    .hud-root {
      border-color: rgba(0, 0, 0, 0.2);
      box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
    }
    .hud-dot {
      box-shadow: 0 0 12px rgba(255, 64, 64, 0.9);
    }
  }
</style>
