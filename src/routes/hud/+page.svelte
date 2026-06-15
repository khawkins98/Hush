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
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onDestroy, onMount } from "svelte";
  import { Events } from "$lib/events";
  import AudioWaveform from "$lib/AudioWaveform.svelte";
  // Shared timer formatting (multi-agent review follow-up): the HUD and
  // the Transcribe panel must render identical elapsed strings for the
  // same recording, so both import the single tested helper.
  import { formatElapsed } from "$lib/recording-time";

  // Wire shape mirrors `HudStatePayload` in `src-tauri/src/hud/mod.rs`
  // (camelCase per `serde(rename_all = "camelCase")`). `startedAtMs`
  // is only present on Recording transitions; `endsAtMs` is only
  // present on Pending transitions. Processing and Done transitions
  // omit both.
  type HudStatePayload = {
    state: "recording" | "processing" | "done" | "pending";
    startedAtMs?: number;
    endsAtMs?: number;
  };

  // HUD lifecycle state (#291). Backend emits `hud:state` with
  // `"recording"`, `"processing"`, or `"done"`. Recording renders
  // the pulsing dot + waveform; Processing replaces the waveform
  // with a shimmer; Done shows a green "Copied!" confirmation that
  // self-dismisses after ~1.5 s (#669).
  //
  // Defaults to `null` (no state yet) rather than `"recording"` so
  // AudioWaveform only mounts after the backend explicitly fires the
  // first `hud:state` event — which happens when the window is
  // already visible. If we default to "recording", AudioWaveform
  // mounts while the window is hidden, WebKit throttles/stops
  // requestAnimationFrame, and the rAF loop never recovers when the
  // window becomes visible, leaving the bars permanently frozen at
  // the silence floor.
  let hudState = $state<"recording" | "processing" | "done" | "pending" | null>(null);

  // True while the inline "Stop recording?" confirmation is shown.
  let confirmingStop = $state(false);

  // Timer handle for the "done" → auto-dismiss sequence (#669).
  // Cancelled if a new recording starts before the timer fires.
  let doneTimer: ReturnType<typeof setTimeout> | null = null;

  // Pending countdown state. `pendingEndsAtMs` is set when the backend emits
  // `hud:state === "pending"` and cleared on any other transition. `pendingTick`
  // is incremented each rAF frame while pending so the progress bar expression
  // re-evaluates on every frame — Svelte 5 won't re-run a template expression
  // unless a reactive dependency changes, and `Date.now()` alone isn't reactive.
  let pendingEndsAtMs = $state<number | null>(null);
  let pendingTick = $state(0);
  let pendingRaf: number | undefined;
  // Derived progress [0,1] for the countdown bar. `pendingTick` is the reactive
  // dependency that forces re-evaluation on each rAF frame.
  let pendingProgress = $derived(
    pendingTick >= 0 && pendingEndsAtMs !== null
      ? Math.max(0, Math.min(1, (pendingEndsAtMs - Date.now()) / 3000))
      : 1
  );

  // Transcription progress 0–100, set while hudState === "processing" (#566).
  // Reset to null on each new recording cycle so back-to-back dictations
  // start without a stale percentage. Null means "no progress yet" and
  // keeps the label as plain "Processing…" until the first tick arrives.
  let transcriptionProgress = $state<number | null>(null);

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
  // Drive the countdown bar animation while in pending state. The effect
  // starts/stops the rAF loop automatically when `hudState` changes.
  $effect(() => {
    if (hudState === "pending") {
      const loop = () => {
        pendingTick += 1;
        pendingRaf = requestAnimationFrame(loop);
      };
      pendingRaf = requestAnimationFrame(loop);
    } else {
      if (pendingRaf !== undefined) {
        cancelAnimationFrame(pendingRaf);
        pendingRaf = undefined;
      }
    }
    return () => {
      if (pendingRaf !== undefined) {
        cancelAnimationFrame(pendingRaf);
        pendingRaf = undefined;
      }
    };
  });

  let unlistenState: UnlistenFn | null = null;
  let unlistenProgress: UnlistenFn | null = null;
  let raf: number | undefined;

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
        if (next === "recording" || next === "processing" || next === "done" || next === "pending") {
          // Cancel any pending done-dismiss timer when state changes.
          if (doneTimer !== null) {
            clearTimeout(doneTimer);
            doneTimer = null;
          }
          hudState = next;
          // Any non-recording/non-pending transition clears the stop confirmation strip.
          if (next !== "recording") confirmingStop = false;
          // Clear pending countdown unless we're entering pending state.
          if (next !== "pending") {
            pendingEndsAtMs = null;
          }
          if (next === "done") {
            // Auto-dismiss after 1.5 s so the user sees "Copied!" before
            // the HUD disappears (#669). A new recording cancels this.
            doneTimer = setTimeout(async () => {
              doneTimer = null;
              try {
                await getCurrentWebviewWindow().hide();
              } catch {
                // Non-fatal — the window will still be visible but won't
                // block anything.
              }
            }, 1500);
          } else if (next === "processing") {
            // Freeze the timer (don't reset) — the user still sees
            // the final duration of the just-finished capture during
            // the post-stop transcription window. A back-to-back
            // dictation will reset on the next `recording` event.
            // The waveform's own freeze-on-flip-off behaviour is
            // driven by `active={hudState === "recording"}` on the
            // AudioWaveform component below.
            recordingStartedAt = null;
          } else if (next === "pending") {
            // Pending — set the countdown end time from the backend payload.
            // `pendingTick` drives re-evaluation via the rAF $effect above.
            pendingEndsAtMs = payload.endsAtMs ?? (Date.now() + 3000);
          } else {
            // Recording — anchor the timer to the backend-supplied
            // `startedAtMs` (#481). The persistent HUD page can
            // race the show/emit pair, so seeding from `Date.now()`
            // here drifts across cycles. The Rust path always sends
            // a fresh timestamp on every Recording transition;
            // missing field is a defensive fallback.
            confirmingStop = false;
            recordingStartedAt = payload.startedAtMs ?? Date.now();
            elapsedLabel = "0:00";
            // Reset progress from previous cycle so we don't show
            // a stale percentage on the next Processing transition.
            transcriptionProgress = null;
          }
        }
      },
    );

    unlistenProgress = await listen<number>(
      Events.TranscriptionProgress,
      (event) => {
        transcriptionProgress = event.payload;
      },
    );

    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    unlistenState?.();
    unlistenState = null;
    unlistenProgress?.();
    unlistenProgress = null;
    if (doneTimer !== null) {
      clearTimeout(doneTimer);
      doneTimer = null;
    }
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
    confirmingStop = false;
    try {
      await getCurrentWebviewWindow().hide();
    } catch {
      // Hide failure is non-fatal — recording continues regardless.
    }
  }

  // Double-click the HUD pill to bring the main Hush window forward.
  // Useful when the user wants to check history or settings without
  // navigating away from whatever app they're dictating into.
  async function raiseMainWindow() {
    try {
      await invoke("show_main_window");
    } catch {
      // Best-effort — main window will still be accessible via tray.
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
  class:hud-done={hudState === "done"}
  data-tauri-drag-region
  role="status"
  aria-live="polite"
  aria-label={hudState === "processing"
    ? transcriptionProgress !== null
      ? `Processing transcription ${transcriptionProgress}%`
      : "Processing transcription"
    : hudState === "done"
      ? "Copied to clipboard"
      : hudState === "pending"
        ? "Meeting detected, recording will start soon"
        : "Recording in progress"}
  ondblclick={raiseMainWindow}
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
    {hudState === "processing"
      ? transcriptionProgress !== null
        ? `Processing… ${transcriptionProgress}%`
        : "Processing…"
      : hudState === "done"
        ? "Copied!"
        : hudState === "pending"
          ? "Meeting detected"
          : "Recording"}
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
  {:else if hudState === "pending"}
    <!--
      Pending state: a draining orange progress bar shows how much
      time remains before auto-recording starts. Progress = fraction
      of the 3-second countdown window remaining (1 = full, 0 = empty).
      `pendingProgress` (a $derived that reads `pendingTick`) is the reactive
      dependency that forces re-evaluation on every rAF frame.
    -->
    {#if pendingEndsAtMs !== null}
      <div
        class="hud-countdown-bar"
        style="--progress: {pendingProgress}"
        role="presentation"
        data-testid="hud-countdown-bar"
      ></div>
    {/if}
  {:else if hudState === "done"}
    <!--
      Done state (#669): a brief green check glyph replaces the
      shimmer so the user gets a clear "safe to paste" signal
      before the HUD self-dismisses.
    -->
    <svg
      class="hud-done-check"
      viewBox="0 0 16 16"
      width="16"
      height="16"
      aria-hidden="true"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
    >
      <polyline points="2.5,8.5 6.5,12.5 13.5,3.5" />
    </svg>
  {/if}
  {#if hudState === "recording" && confirmingStop}
    <span class="hud-confirm-stop" data-tauri-drag-region="false">
      <span class="hud-confirm-label">Stop recording?</span>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--stop"
        onclick={async () => {
          try { await invoke("meeting_stop_manual"); } catch { /* best-effort */ }
          await dismiss();
        }}
        ondblclick={(e) => e.stopPropagation()}
      >Stop</button>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--keep"
        onclick={() => { confirmingStop = false; }}
        ondblclick={(e) => e.stopPropagation()}
      >Keep recording</button>
    </span>
  {:else if hudState === "recording"}
    <button
      type="button"
      class="hud-stop"
      aria-label="Stop recording"
      title="Stop recording"
      data-tauri-drag-region="false"
      onclick={() => { confirmingStop = true; }}
      ondblclick={(e) => e.stopPropagation()}
    >
      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
        <rect x="2" y="2" width="8" height="8" fill="currentColor" rx="1" />
      </svg>
    </button>
  {/if}
  {#if hudState === "pending"}
    <button
      type="button"
      class="hud-cancel-pending"
      aria-label="Cancel auto-recording"
      title="Cancel auto-recording"
      data-tauri-drag-region="false"
      onclick={async () => {
        try { await invoke("meeting_cancel_pending"); } catch { /* best-effort */ }
        await dismiss();
      }}
      ondblclick={(e) => e.stopPropagation()}
    >Cancel</button>
  {/if}
  <button
    type="button"
    class="hud-dismiss"
    aria-label="Hide recording overlay (recording continues)"
    title="Hide overlay"
    onclick={dismiss}
    ondblclick={(e) => e.stopPropagation()}
    data-tauri-drag-region="false"
  >
    <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
      <path d="M2 2 L10 10 M10 2 L2 10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
    </svg>
  </button>
</div>

<style>
  /* Transparent window — override the global body background. */
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: transparent;
    overflow: hidden;
    color: #f5efe8;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  .hud-root {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.65rem;
    height: 100vh;
    width: 100vw;
    box-sizing: border-box;
    padding: 0 0.65rem;
    /* Neutral near-black pill, glassy — matches the app's dark canvas */
    background-color: rgba(19, 18, 17, 0.88);
    border-radius: 999px;
    border: 1px solid rgba(244, 158, 23, 0.28);
    box-shadow:
      0 4px 16px rgba(0, 0, 0, 0.45),
      0 0 0 0.5px rgba(244, 158, 23, 0.12);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    user-select: none;
    --audio-waveform-height: 24px;
    cursor: grab;
  }
  .hud-root:active {
    cursor: grabbing;
  }

  .hud-stop {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: none;
    background: transparent;
    color: #f87171;
    cursor: pointer;
    padding: 0;
    opacity: 0.75;
    transition: opacity 0.15s;
    flex-shrink: 0;
  }
  .hud-stop:hover { opacity: 1; }

  .hud-confirm-stop {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .hud-confirm-label {
    font-size: 11px;
    opacity: 0.85;
    white-space: nowrap;
  }
  .hud-confirm-btn {
    font-size: 11px;
    border-radius: 10px;
    border: none;
    cursor: pointer;
    padding: 2px 8px;
    transition: opacity 0.15s;
  }
  .hud-confirm-btn--stop {
    background: #f87171;
    color: #1a1a1a;
  }
  .hud-confirm-btn--keep {
    background: rgba(255,255,255,0.15);
    color: #f5efe8;
  }
  .hud-confirm-btn:hover { opacity: 0.85; }

  .hud-dismiss {
    margin-left: auto;
    padding: 0;
    width: 18px;
    height: 18px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background-color: rgba(255, 255, 255, 0.14);
    color: rgba(255, 255, 255, 0.75);
    border-radius: 50%;
    cursor: pointer;
    transition: background-color 0.12s, color 0.12s;
  }
  .hud-dismiss:hover {
    background-color: rgba(255, 255, 255, 0.28);
    color: #ffffff;
  }
  .hud-dismiss:focus-visible {
    outline: 2px solid rgba(244, 158, 23, 0.7);
    outline-offset: 1px;
  }

  .hud-grip {
    display: inline-flex;
    align-items: center;
    color: rgba(255, 255, 255, 0.25);
    transition: color 0.12s;
  }
  .hud-root:hover .hud-grip {
    color: rgba(255, 255, 255, 0.55);
  }

  .hud-dot {
    width: 0.85rem;
    height: 0.85rem;
    /* Never let the flex row compress the dot's width — once the elapsed
       timer grows to H:MM:SS (calls over an hour) the pill gets tight and
       a shrinkable dot renders as an ellipse (#989). */
    flex-shrink: 0;
    border-radius: 50%;
    background-color: #e85050;
    box-shadow: 0 0 8px rgba(232, 80, 80, 0.6);
    animation: hud-pulse 1.2s ease-in-out infinite;
  }

  .hud-label {
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  .hud-elapsed {
    font-size: 0.85rem;
    font-weight: 500;
    color: rgba(245, 239, 232, 0.72);
    font-variant-numeric: tabular-nums;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    letter-spacing: 0.01em;
  }

  @keyframes hud-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }
  @media (prefers-reduced-motion: reduce) {
    .hud-dot { animation: none; }
  }

  /* Processing: dot turns orange (accent), shimmer replaces waveform */
  .hud-processing .hud-dot {
    animation: none;
    background-color: #f49e17;
    box-shadow: 0 0 6px rgba(244, 158, 23, 0.6);
  }

  .hud-shimmer {
    width: 60px;
    height: var(--audio-waveform-height, 16px);
    background-color: rgba(255, 255, 255, 0.10);
    border-radius: 3px;
    overflow: hidden;
  }
  .hud-shimmer-fill {
    height: 100%;
    border-radius: 3px;
    background: linear-gradient(
      90deg,
      rgba(244, 158, 23, 0.1) 0%,
      rgba(244, 158, 23, 0.55) 50%,
      rgba(244, 158, 23, 0.1) 100%
    );
    background-size: 200% 100%;
    background-position: 100% 0;
    animation: hud-shimmer 1.6s linear infinite;
  }
  @keyframes hud-shimmer {
    0%   { background-position: 100% 0; }
    100% { background-position: -100% 0; }
  }
  @media (prefers-reduced-motion: reduce) {
    .hud-shimmer-fill { animation: none; background-position: 50% 0; }
  }

  /* Pending: draining orange bar showing countdown to auto-recording. */
  .hud-countdown-bar {
    width: calc(var(--progress, 1) * 60px);
    height: 3px;
    background: #f49e17;
    border-radius: 2px;
    flex-shrink: 0;
    transition: none; /* rAF-driven — no CSS transition needed */
  }
  @media (prefers-reduced-motion: reduce) {
    .hud-countdown-bar {
      /* Show a static half-width bar instead of animating */
      width: 30px;
    }
  }

  /* Cancel button shown in pending state */
  .hud-cancel-pending {
    font-size: 11px;
    border-radius: 10px;
    border: none;
    cursor: pointer;
    padding: 2px 8px;
    background: rgba(255,255,255,0.15);
    color: #f5efe8;
    transition: opacity 0.15s;
    flex-shrink: 0;
  }
  .hud-cancel-pending:hover { opacity: 0.85; }

  /* Done: green dot + check. HUD-local green, tuned for the dark pill —
     the light-theme --success-text (#2f7a35) would be too dark here. */
  .hud-done .hud-dot {
    animation: none;
    background-color: #74b06c;
    box-shadow: 0 0 6px rgba(116, 176, 108, 0.55);
  }
  .hud-done-check {
    color: #74b06c;
    width: 16px;
    height: 16px;
  }
</style>
