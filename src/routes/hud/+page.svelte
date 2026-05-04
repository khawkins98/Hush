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
  import AudioWaveform from "$lib/AudioWaveform.svelte";

  // Wire shape mirrors `HudStatePayload` in `src-tauri/src/hud/mod.rs`
  // (camelCase per `serde(rename_all = "camelCase")`). `startedAtMs`
  // is only present on Recording transitions; Processing transitions
  // omit it so the frontend keeps the freeze-at-final-value behaviour.
  type HudStatePayload = {
    state: "recording" | "processing";
    startedAtMs?: number;
  };

  // HUD lifecycle state (#291). Backend emits `hud:state` with
  // `"recording"` or `"processing"`. Recording renders the pulsing
  // dot + waveform; Processing replaces the waveform with a
  // shimmer so the user knows transcription is in flight and
  // pasting too early would be premature.
  //
  // Defaults to `null` (no state yet) rather than `"recording"` so
  // AudioWaveform only mounts after the backend explicitly fires the
  // first `hud:state` event — which happens when the window is
  // already visible. If we default to "recording", AudioWaveform
  // mounts while the window is hidden, WebKit throttles/stops
  // requestAnimationFrame, and the rAF loop never recovers when the
  // window becomes visible, leaving the bars permanently frozen at
  // the silence floor.
  let hudState = $state<"recording" | "processing" | null>(null);

  // Recording-duration timer (#360). `recordingStartedAt` is set
  // when the backend emits `hud:state === "recording"`, freezes
  // when state flips to `processing`, and resets between cycles so
  // back-to-back dictations each start at 0:00. The visible
  // `elapsedLabel` is recomputed on every rAF tick — separate
  // from the AudioWaveform's internal animation loop because the
  // timer label is HUD-specific.
  let recordingStartedAt = $state<number | null>(null);
  let elapsedLabel = $state("0:00");

  // Pre-#330 the unlisten handle was a closure-local inside
  // `onMount`'s synchronous teardown, populated by `.then()`.
  // Hoisted to module scope and assigned via `await listen(...)`
  // inside an async `onMount` so the teardown in `onDestroy` always
  // sees the resolved unlisten fn — even when the HUD is hidden +
  // recreated faster than the listen promise resolves. Pre-fix the
  // listener leaked across HUD lifecycles, accumulating one extra
  // `hud:state` handler per dictation cycle (#330). The
  // `audio:level` listener that previously lived here moved into
  // `AudioWaveform.svelte` along with the rest of the waveform
  // logic in #411 phase B.
  let unlistenState: UnlistenFn | null = null;
  let raf: number | undefined;

  // Format a millisecond duration as `M:SS` (under an hour) or
  // `H:MM:SS` (one hour and beyond — meetings hit this routinely).
  function formatElapsed(ms: number): string {
    const totalSeconds = Math.max(0, Math.floor(ms / 1000));
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;
    if (hours > 0) {
      return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
    }
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
  }

  onMount(async () => {
    const tick = () => {
      const now = Date.now();
      if (recordingStartedAt !== null) {
        elapsedLabel = formatElapsed(now - recordingStartedAt);
      }
      raf = requestAnimationFrame(tick);
    };

    unlistenState = await listen<HudStatePayload>(
      Events.HudState,
      (event) => {
        const payload = event.payload;
        const next = payload?.state;
        if (next === "recording" || next === "processing") {
          hudState = next;
          if (next === "processing") {
            // Freeze the timer (don't reset) — the user still sees
            // the final duration of the just-finished capture during
            // the post-stop transcription window. A back-to-back
            // dictation will reset on the next `recording` event.
            // The waveform's own freeze-on-flip-off behaviour is
            // driven by `active={hudState === "recording"}` on the
            // AudioWaveform component below.
            recordingStartedAt = null;
          } else {
            // Recording — anchor the timer to the backend-supplied
            // `startedAtMs` (#481). The persistent HUD page can
            // race the show/emit pair, so seeding from `Date.now()`
            // here drifts across cycles. The Rust path always sends
            // a fresh timestamp on every Recording transition;
            // missing field is a defensive fallback.
            recordingStartedAt = payload.startedAtMs ?? Date.now();
            elapsedLabel = "0:00";
          }
        }
      },
    );

    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    unlistenState?.();
    unlistenState = null;
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
  });

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
    <span class="hud-elapsed" data-testid="hud-elapsed" aria-hidden="true">
      {elapsedLabel}
    </span>
  {/if}
  {#if hudState === "recording"}
    <!--
      Waveform visualiser (#362). The component owns its own
      audio:level subscription, attack/release smoothing, and ring
      buffer; we just gate it with `active` so the post-stop
      shimmer doesn't flash the bars. Extracted to $lib in #411
      phase B so the main window's recording status row can render
      the same affordance.
      Only mounted when hudState is explicitly "recording" (set by
      the backend event) so the rAF loop starts in a visible window.
    -->
    <AudioWaveform mode="recording" levelScale={480} silenceFloorPct={15} />
  {:else if hudState === "processing"}
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
    box-sizing: border-box;
    /* Side padding keeps the grip dots and dismiss button off the
       pill edges. box-sizing: border-box ensures the padding is
       absorbed into the 100vw width rather than overflowing. */
    padding: 0 0.55rem;
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
    /* The AudioWaveform height/scale are taller in the HUD than the
       defaults so bars are clearly visible in the compact pill.
       Custom properties cascade into the child component. */
    --audio-waveform-height: 24px;
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
    transition: color 0.12s;
  }
  .hud-root:hover .hud-grip {
    color: rgba(255, 255, 255, 0.65);
  }

  .hud-dot {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 50%;
    background-color: var(--danger);
    box-shadow: 0 0 8px rgba(255, 64, 64, 0.55);
    animation: hud-pulse 1.2s ease-in-out infinite;
  }

  .hud-label {
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  /* Elapsed-time counter (#360). Sits between the label and the
     level meter; tabular-numbers prevent the column from wobbling
     as digits change width. Slightly dimmer than the label so the
     visual hierarchy is "Recording" → time → meter. */
  .hud-elapsed {
    font-size: 0.85rem;
    font-weight: 500;
    color: rgba(255, 255, 255, 0.78);
    font-variant-numeric: tabular-nums;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    letter-spacing: 0.01em;
  }

  /* Waveform visualiser (#362) styles moved to AudioWaveform.svelte
     in #411 phase B. The default palette (red gradient) matches the
     pre-extraction look exactly so the HUD pill renders unchanged. */

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
    /* Match the waveform height so the pill doesn't reflow when
       state flips between recording and processing. Anchored to
       the same custom property set on .hud-root. */
    height: var(--audio-waveform-height, 16px);
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
