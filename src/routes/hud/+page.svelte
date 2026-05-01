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
  import { onDestroy, onMount } from "svelte";
  import { Events } from "$lib/events";

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

  // HUD lifecycle state (#291). Backend emits `hud:state` with
  // `"recording"` or `"processing"`. Recording is the existing
  // visual (pulsing dot + level meter); Processing replaces the
  // meter with a static label so the user knows transcription is
  // in flight and pasting too early would be premature. Defaults
  // to recording — start_dictation always fires the explicit
  // recording state too, so this is just a safe initial render.
  let hudState = $state<"recording" | "processing">("recording");

  // Pre-#330 these were closure-locals inside `onMount`'s synchronous
  // teardown, populated by `.then()`. Hoisted to module scope and
  // assigned via `await listen(...)` inside an async `onMount` so the
  // teardown in `onDestroy` always sees the resolved unlisten fns —
  // even when the HUD is hidden + recreated faster than the listen
  // promises resolve. Pre-fix the listeners leaked across HUD
  // lifecycles, accumulating one extra `audio:level` handler per
  // dictation cycle (#330).
  let unlistenLevel: UnlistenFn | null = null;
  let unlistenState: UnlistenFn | null = null;
  let raf: number | undefined;

  onMount(async () => {
    const ATTACK = 0.6;
    const RELEASE = 0.12;

    const tick = () => {
      const target = rms;
      const coeff = target > displayLevel ? ATTACK : RELEASE;
      displayLevel += (target - displayLevel) * coeff;
      raf = requestAnimationFrame(tick);
    };

    unlistenLevel = await listen<number>(Events.AudioLevel, (event) => {
      rms = event.payload ?? 0;
    });

    unlistenState = await listen<string>(Events.HudState, (event) => {
      const next = event.payload;
      if (next === "recording" || next === "processing") {
        hudState = next;
        // Freeze the level meter on transition into Processing
        // so a stray late-arriving `audio:level` event (the pump
        // ticks at ~30 Hz and may have one in flight) doesn't
        // briefly relight the bar after capture has stopped.
        if (next === "processing") {
          rms = 0;
          displayLevel = 0;
        }
      }
    });

    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    unlistenLevel?.();
    unlistenLevel = null;
    unlistenState?.();
    unlistenState = null;
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
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
  class:hud-processing={hudState === "processing"}
  data-tauri-drag-region
  role="status"
  aria-live="polite"
  aria-label={hudState === "processing"
    ? "Processing transcription"
    : "Recording in progress"}
>
  <!--
    Subtle 6-dot grip glyph at the leading edge. The whole pill is a
    drag region (data-tauri-drag-region on the root), but without a
    visual cue users can't tell — the grip dots are the standard
    macOS / web idiom (see Finder sidebar, Notion blocks, draggable
    list rows). aria-hidden because screen readers already get
    "Recording in progress" from the root's aria-label and this is
    pure visual affordance.
  -->
  <span class="hud-grip" aria-hidden="true">
    <svg viewBox="0 0 6 12" width="6" height="12">
      <circle cx="1.5" cy="2" r="0.9" fill="currentColor" />
      <circle cx="4.5" cy="2" r="0.9" fill="currentColor" />
      <circle cx="1.5" cy="6" r="0.9" fill="currentColor" />
      <circle cx="4.5" cy="6" r="0.9" fill="currentColor" />
      <circle cx="1.5" cy="10" r="0.9" fill="currentColor" />
      <circle cx="4.5" cy="10" r="0.9" fill="currentColor" />
    </svg>
  </span>
  <span class="hud-dot"></span>
  <span class="hud-label">
    {hudState === "processing" ? "Processing…" : "Recording"}
  </span>
  {#if hudState === "recording"}
    <div class="hud-meter" role="presentation">
      <div class="hud-meter-fill" style="width: {barWidth}%"></div>
    </div>
  {:else}
    <!--
      Processing state: replace the level meter with a slim
      shimmer bar — same width / position as the meter so the
      pill doesn't reflow on transition. The shimmer reuses the
      same gradient pattern as the Meeting panel's listening
      pill so the visual idiom is consistent ("Hush is still
      working but isn't capturing audio right now").
    -->
    <div class="hud-shimmer" role="presentation">
      <div class="hud-shimmer-fill"></div>
    </div>
  {/if}
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
    background-color: rgba(255, 255, 255, 0.18);
    color: rgba(255, 255, 255, 0.85);
    border-radius: 50%;
    cursor: pointer;
    transition: background-color 0.12s, color 0.12s;
  }
  .hud-dismiss:hover {
    background-color: rgba(255, 255, 255, 0.32);
    color: rgba(255, 255, 255, 1);
  }
  .hud-dismiss:focus-visible {
    outline: 2px solid rgba(255, 255, 255, 0.6);
    outline-offset: 1px;
  }

  /* Drag grip. Ghosted by default and lit on hover so it stays
     discoverable without competing with the recording dot for
     attention. inline-flex keeps the SVG vertically centred. */
  .hud-grip {
    display: inline-flex;
    align-items: center;
    color: rgba(255, 255, 255, 0.35);
    margin-right: -0.15rem;
    transition: color 0.12s;
  }
  .hud-root:hover .hud-grip {
    color: rgba(255, 255, 255, 0.65);
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

  /* Processing state (#291). The pill stays the same size and
     shape; the dot stops pulsing (transcription isn't capturing
     anything new) and the level meter is replaced with a calm
     shimmer bar so the user knows Hush is still working. The
     dismiss button + drag region stay live. */
  .hud-processing .hud-dot {
    animation: none;
    background-color: #ffb84a;
    box-shadow: 0 0 6px rgba(255, 184, 74, 0.5);
  }

  .hud-shimmer {
    width: 60px;
    height: 6px;
    background-color: rgba(255, 255, 255, 0.12);
    border-radius: 3px;
    overflow: hidden;
  }

  .hud-shimmer-fill {
    height: 100%;
    border-radius: 3px;
    background: linear-gradient(
      90deg,
      rgba(255, 255, 255, 0.15) 0%,
      rgba(255, 255, 255, 0.6) 50%,
      rgba(255, 255, 255, 0.15) 100%
    );
    background-size: 200% 100%;
    background-position: 100% 0;
    animation: hud-shimmer 1.6s linear infinite;
  }

  @keyframes hud-shimmer {
    0% { background-position: 100% 0; }
    100% { background-position: -100% 0; }
  }

  @media (prefers-reduced-motion: reduce) {
    .hud-shimmer-fill {
      animation: none;
      background-position: 50% 0;
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
