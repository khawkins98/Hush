<!--
  Recording HUD overlay. Loaded into the secondary `hud` Tauri
  window (label `hud`, configured in `tauri.conf.json`) — borderless,
  transparent, always-on-top. The window is hidden by default and
  shown/hidden by the backend `hud::show` / `hud::hide` calls in
  the IPC commands' `start_dictation` / `stop_dictation` paths.

  This page renders a single fixed widget: a pulsing red dot + the
  word "Recording". No interactivity, no fetches, no state — the
  presence of this page is the signal. (The level meter half of
  #21 lands when the audio module exposes a per-chunk level
  callback; this page will then animate a bar driven by `audio:level`
  events.)

  Why a separate route rather than reusing the main page in a
  different mode: the HUD's window config differs significantly
  (transparent, no decorations, not in the taskbar). Reusing
  `+page.svelte` would mean rendering the entire dictation UI inside
  the HUD window, which gets ignored but still pulls in code +
  fetches. A dedicated minimal page is faster to load and easier
  to reason about.
-->
<div class="hud-root" aria-hidden="true">
  <span class="hud-dot"></span>
  <span class="hud-label">Recording</span>
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
    font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
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
    -webkit-app-region: drag; /* allow click-drag on macOS */
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

  @keyframes hud-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }

  @media (prefers-reduced-motion: reduce) {
    .hud-dot {
      animation: none;
    }
  }
</style>
