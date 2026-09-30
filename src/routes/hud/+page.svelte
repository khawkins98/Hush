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
  type HudPhase =
    | "recording"
    | "processing"
    | "done"
    | "pending"
    | "call-may-have-ended"
    | "stopping"
    | "stopped"
    | "stop-failed";
  type HudStatePayload = {
    state: HudPhase;
    startedAtMs?: number;
    endsAtMs?: number;
    confidence?: "high" | "medium";
    kind?: "dictation" | "meeting";
  };

  // How long the "Stopped" confirmation stays up before self-dismissing,
  // and the ceiling on "Stopping…" if the backend's Stopped event never
  // arrives (stop_manual's own audio-release timeout is 10 s).
  const STOPPED_DISMISS_MS = 1800;
  const STOPPING_FALLBACK_MS = 15_000;
  // Confirm buttons ignore input for this long after appearing, so the
  // second half of a double-click on ■ can't land on "Stop" (#1001
  // follow-up). Rendered as `disabled` — Playwright waits for enabled.
  const CONFIRM_ARM_MS = 300;
  // A double-click on the pill raises the main window — but not when
  // either click hit a control. The first click of a double-click on ■
  // swaps the button for the confirm strip, so the second click lands on
  // the pill and the dblclick bubbles to the root with no button in its
  // path. Remembering the last control press closes that gap.
  const CONTROL_DBLCLICK_GUARD_MS = 700;

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
  let hudState = $state<HudPhase | null>(null);

  // What the current Recording pill is capturing. Only a meeting can be
  // stopped from the HUD: dictation is owned by the main window's hotkey
  // state machine, and pre-fix ■ called `meeting_stop_manual` during a
  // dictation — the HUD hid and the dictation kept recording.
  // Defaults to "dictation" so an unlabelled Recording fails closed (no ■)
  // — the fail-open direction is exactly the bug described above.
  let recordingKind = $state<"dictation" | "meeting">("dictation");

  // True while the inline "Stop recording?" confirmation is shown.
  let confirmingStop = $state(false);

  // Confirm-strip arming (see CONFIRM_ARM_MS). Re-armed whenever a
  // prompt appears.
  let promptArmed = $state(false);
  let armTimer: ReturnType<typeof setTimeout> | null = null;
  function armPrompt() {
    promptArmed = false;
    if (armTimer !== null) clearTimeout(armTimer);
    armTimer = setTimeout(() => {
      armTimer = null;
      promptArmed = true;
    }, CONFIRM_ARM_MS);
  }

  // Shown inline for a few seconds when a stop attempt fails.
  let stopError = $state<string | null>(null);
  let stopErrorTimer: ReturnType<typeof setTimeout> | null = null;

  let lastControlPressAt = 0;

  // Elapsed-timer anchor saved when a stop begins, so a failed stop can
  // resume the live counter instead of freezing it.
  let startedAtBeforeStop: number | null = null;

  // Call-end detector state. `callEndConfidence` is set when the backend
  // emits `call-may-have-ended`; `prevHudState` tracks what state to
  // restore if the user clicks "Keep recording"; `sessionCallEndSuppressed`
  // is set for the rest of the session once the user explicitly opts to
  // keep recording — prevents the same session from re-prompting.
  let callEndConfidence = $state<"high" | "medium" | null>(null);
  let prevHudState = $state<"recording" | null>(null);
  let sessionCallEndSuppressed = $state(false);

  // Timer handle for the "done"/"stopped" → auto-dismiss sequence
  // (#669), and the "stopping" fallback. Cancelled on any state change.
  let doneTimer: ReturnType<typeof setTimeout> | null = null;

  function hideAfter(ms: number) {
    if (doneTimer !== null) clearTimeout(doneTimer);
    doneTimer = setTimeout(async () => {
      doneTimer = null;
      try {
        await getCurrentWebviewWindow().hide();
      } catch {
        // Non-fatal — the window will still be visible but won't
        // block anything.
      }
    }, ms);
  }

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
  let unlistenCallEndCancelled: UnlistenFn | null = null;
  let unlistenCallMayHaveEnded: UnlistenFn | null = null;
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
        // Handle call-may-have-ended before the main state switch — it
        // doesn't follow the same clear/reset sequence as the other states.
        if (next === "call-may-have-ended") {
          showCallEndPrompt(payload.confidence ?? "high");
          return;
        }
        if (next === "stop-failed") {
          resumeAfterFailedStop();
          return;
        }
        if (
          next === "recording"
          || next === "processing"
          || next === "done"
          || next === "pending"
          || next === "stopping"
          || next === "stopped"
        ) {
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
            hideAfter(1500);
          } else if (next === "stopped") {
            // Meeting audio released; the transcript tail keeps
            // finalizing in the background. Confirm, then get out of the way.
            recordingStartedAt = null;
            hideAfter(STOPPED_DISMISS_MS);
          } else if (next === "stopping") {
            if (recordingStartedAt !== null) startedAtBeforeStop = recordingStartedAt;
            recordingStartedAt = null;
            hideAfter(STOPPING_FALLBACK_MS);
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
            clearStopError();
            recordingKind = payload.kind ?? "dictation";
            // A fresh Recording transition starts a new session — allow
            // call-end prompts to fire again.
            sessionCallEndSuppressed = false;
            callEndConfidence = null;
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

    // The call-end detector broadcasts `meeting:call-may-have-ended`
    // (meeting/events.rs); it never emits a `hud:state` for it. Pre-fix
    // the HUD only handled the `hud:state` form, so its prompt never
    // appeared in the real app — only the main window's banner did.
    unlistenCallMayHaveEnded = await listen<{ confidence: "high" | "medium" }>(
      Events.CallMayHaveEnded,
      (event) => showCallEndPrompt(event.payload?.confidence ?? "high"),
    );

    // When a reversal signal arrives (mic goes active again, loud audio
    // spike, real speech detected) the backend fires CallEndCancelled.
    // Dismiss the call-end prompt and restore the previous state.
    unlistenCallEndCancelled = await listen(Events.CallEndCancelled, () => {
      if (hudState === "call-may-have-ended") {
        hudState = prevHudState ?? "recording";
        callEndConfidence = null;
      }
    });

    raf = requestAnimationFrame(tick);
  });

  onDestroy(() => {
    unlistenState?.();
    unlistenState = null;
    unlistenProgress?.();
    unlistenProgress = null;
    unlistenCallEndCancelled?.();
    unlistenCallEndCancelled = null;
    unlistenCallMayHaveEnded?.();
    unlistenCallMayHaveEnded = null;
    if (doneTimer !== null) {
      clearTimeout(doneTimer);
      doneTimer = null;
    }
    if (armTimer !== null) {
      clearTimeout(armTimer);
      armTimer = null;
    }
    clearStopError();
    if (raf !== undefined) {
      cancelAnimationFrame(raf);
      raf = undefined;
    }
  });

  let promptVisible = $derived(
    hudState === "call-may-have-ended" || (hudState === "recording" && confirmingStop),
  );

  let label = $derived.by(() => {
    switch (hudState) {
      case "processing":
        return transcriptionProgress !== null
          ? `Transcribing… ${Math.round(transcriptionProgress)}%`
          : "Processing…";
      case "done":
        return "Copied!";
      case "pending":
        return "Meeting detected";
      case "stopping":
        return "Stopping…";
      case "stopped":
        return "Stopped · saving transcript";
      default:
        return stopError ?? "Recording";
    }
  });

  let ariaLabel = $derived.by(() => {
    switch (hudState) {
      case "pending":
        return "Meeting detected, recording will start soon";
      case "call-may-have-ended":
        return "Your call may have ended";
      case "recording":
        return stopError ?? "Recording in progress";
      default:
        return label;
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

  function clearStopError() {
    if (stopErrorTimer !== null) {
      clearTimeout(stopErrorTimer);
      stopErrorTimer = null;
    }
    stopError = null;
  }

  // Only interrupt a live meeting recording: a prompt over "Stopping…",
  // "Copied!" or a dictation pill would be wrong or would be hidden by
  // that state's pending dismiss timer moments later.
  function showCallEndPrompt(confidence: "high" | "medium") {
    if (sessionCallEndSuppressed) return;
    if (hudState !== "recording" || recordingKind !== "meeting") return;
    confirmingStop = false;
    callEndConfidence = confidence;
    prevHudState = "recording";
    hudState = "call-may-have-ended";
    armPrompt();
  }

  function resumeAfterFailedStop() {
    if (doneTimer !== null) {
      clearTimeout(doneTimer);
      doneTimer = null;
    }
    hudState = "recording";
    recordingKind = "meeting";
    recordingStartedAt = startedAtBeforeStop ?? Date.now();
    clearStopError();
    stopError = "Couldn't stop — try again";
    stopErrorTimer = setTimeout(() => {
      stopErrorTimer = null;
      stopError = null;
    }, 4000);
  }

  function beginConfirmStop() {
    confirmingStop = true;
    armPrompt();
  }

  // Stop the meeting from the HUD. Flips to "Stopping…" immediately so the
  // click visibly registers; the backend then emits `stopped` (→ brief
  // confirmation + self-dismiss). Pre-fix the HUD hid itself before the
  // stop IPC resolved, so a failed stop was indistinguishable from a
  // successful one.
  async function stopMeeting() {
    if (hudState === "stopping" || hudState === "stopped") return;
    confirmingStop = false;
    clearStopError();
    hudState = "stopping";
    startedAtBeforeStop = recordingStartedAt;
    recordingStartedAt = null;
    hideAfter(STOPPING_FALLBACK_MS);
    try {
      await invoke("meeting_stop_manual");
    } catch (e) {
      // "No session active" means a concurrent stop (auto-stop, the main
      // window) already won — nothing is recording, so just go away.
      if (JSON.stringify(e ?? "").includes("no meeting session active")) {
        hideAfter(0);
        return;
      }
      // The backend also emits `stop-failed` when the session is still
      // live; both paths land in the same idempotent resume.
      resumeAfterFailedStop();
    }
  }

  function noteControlPress(e: PointerEvent) {
    if ((e.target as Element | null)?.closest("button")) {
      lastControlPressAt = performance.now();
    }
  }

  // Double-click the HUD pill to bring the main Hush window forward,
  // routed to the Transcribe screen where the live recording and its
  // Stop control are. Pre-fix the main window reopened on whatever
  // section it was last left on (often Settings), so the user couldn't
  // see whether their recording was still going.
  async function raiseMainWindow(e: MouseEvent) {
    if ((e.target as Element | null)?.closest("button, .hud-prompt")) return;
    if (performance.now() - lastControlPressAt < CONTROL_DBLCLICK_GUARD_MS) return;
    try {
      await invoke("show_main_window", { section: "dictation" });
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
  class:hud-processing={hudState === "processing" || hudState === "stopping"}
  class:hud-done={hudState === "done" || hudState === "stopped"}
  class:hud-pending={hudState === "pending"}
  class:hud-prompting={promptVisible}
  data-tauri-drag-region
  role="status"
  aria-live="polite"
  aria-label={ariaLabel}
  onpointerdowncapture={noteControlPress}
  ondblclick={raiseMainWindow}
>
  {#if !promptVisible && hudState !== "pending"}
    <!--
      Subtle 6-dot grip glyph at the leading edge. The pill is a drag
      region (data-tauri-drag-region on the root), but without a visual
      cue users can't tell — the grip dots are the standard macOS / web
      idiom. aria-hidden: pure visual affordance. Dropped while a prompt
      is up so the prompt's buttons get the room.
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
  {/if}
  <span class="hud-dot"></span>

  {#if hudState === "call-may-have-ended"}
    <!--
      Call-end prompt: shown when the backend's call-end detector fires.
      "Stop" ends the meeting; "Keep recording" suppresses further
      prompts for this session. Replaces the label/timer/waveform rather
      than squeezing in beside them — the pill is too narrow for both.
    -->
    <span class="hud-prompt" data-tauri-drag-region="false">
      <span
        class="hud-prompt-label"
        title={callEndConfidence === "high"
          ? "Your call has likely ended"
          : "Your call may be winding down"}
      >
        {callEndConfidence === "high" ? "Call ended?" : "Call winding down?"}
      </span>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--stop"
        disabled={!promptArmed}
        onclick={stopMeeting}
      >Stop</button>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--keep"
        disabled={!promptArmed}
        onclick={() => {
          sessionCallEndSuppressed = true;
          hudState = prevHudState ?? "recording";
          callEndConfidence = null;
          clearStopError();
        }}
      >Keep recording</button>
    </span>
  {:else if hudState === "recording" && confirmingStop}
    <span class="hud-prompt" data-tauri-drag-region="false">
      <span class="hud-prompt-label">Stop recording?</span>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--stop"
        disabled={!promptArmed}
        onclick={stopMeeting}
      >Stop</button>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--keep"
        disabled={!promptArmed}
        onclick={() => { confirmingStop = false; }}
      >Keep recording</button>
    </span>
  {:else}
    <span class="hud-label">{label}</span>
    {#if hudState === "recording" && stopError === null}
      <span class="hud-elapsed" data-testid="hud-elapsed" aria-hidden="true">
        {elapsedLabel}
      </span>
      <!--
        Waveform visualiser (#362). The component owns its own
        audio:level subscription, attack/release smoothing, and ring
        buffer. Only mounted when hudState is explicitly "recording"
        (set by the backend event) so the rAF loop starts in a visible
        window.
      -->
      <AudioWaveform mode="recording" levelScale={480} silenceFloorPct={15} />
    {:else if hudState === "processing" || hudState === "stopping"}
      <!--
        Processing / stopping: a slim shimmer in the waveform's slot so
        the pill doesn't reflow on transition ("Hush is still working
        but isn't capturing audio right now").
      -->
      <div class="hud-shimmer" role="presentation">
        <div class="hud-shimmer-fill"></div>
      </div>
    {:else if hudState === "pending"}
      <!--
        Draining orange bar: fraction of the 3-second countdown left
        before auto-recording starts. `pendingProgress` reads
        `pendingTick`, which forces re-evaluation every rAF frame.
      -->
      {#if pendingEndsAtMs !== null}
        <div class="hud-countdown-track" role="presentation">
          <div
            class="hud-countdown-bar"
            style="--progress: {pendingProgress}"
            data-testid="hud-countdown-bar"
          ></div>
        </div>
      {/if}
      <!-- Announced once on entry; "Don't record" is the action. -->
      <span class="sr-only" aria-live="assertive" aria-atomic="true">
        Recording will start in 3 seconds. Activate Don't record to cancel.
      </span>
    {:else if hudState === "done" || hudState === "stopped"}
      <!--
        Done (#669) / stopped: a brief green check so the user gets a
        clear "finished" signal before the HUD self-dismisses.
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
  {/if}

  {#if hudState === "recording" && !confirmingStop && recordingKind === "meeting"}
    <button
      type="button"
      class="hud-stop"
      aria-label="Stop recording"
      title="Stop recording"
      data-tauri-drag-region="false"
      onclick={beginConfirmStop}
    >
      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
        <rect x="2" y="2" width="8" height="8" fill="currentColor" rx="1.5" />
      </svg>
    </button>
  {/if}
  {#if hudState === "pending"}
    <button
      type="button"
      class="hud-confirm-btn hud-confirm-btn--keep"
      aria-label="Cancel auto-recording"
      title="Cancel auto-recording"
      data-tauri-drag-region="false"
      onclick={async () => {
        try { await invoke("meeting_cancel_pending"); } catch { /* best-effort */ }
        await dismiss();
      }}
    >Don't record</button>
  {/if}
  {#if !promptVisible && hudState !== "pending"}
    <button
      type="button"
      class="hud-dismiss"
      aria-label="Hide recording overlay (recording continues)"
      title="Hide overlay"
      onclick={dismiss}
      data-tauri-drag-region="false"
    >
      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
        <path d="M2 2 L10 10 M10 2 L2 10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
      </svg>
    </button>
  {/if}
</div>

<style>
  /* Transparent window — override the global body background. The HUD
     window is `transparent: true` + `decorations: false`; without this
     rule WebKit paints a white rectangle over the screen. Keep it. */
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: transparent !important;
    overflow: hidden;
    color: #f5efe8;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  /* The window is `shadow: false` and exactly the size in tauri.conf.json
     (mirrored by HUD_LOGICAL_WIDTH in hud/mod.rs). Pre-fix the pill
     filled it edge to edge, so its drop shadow was clipped flat at the
     window bounds. The inset below is the shadow's room — keep it at
     least the shadow's blur + offset. */
  .hud-root {
    --hud-inset: 8px;
    --audio-waveform-height: 20px;
    --audio-waveform-width: 48px;
    position: fixed;
    inset: var(--hud-inset);
    display: flex;
    align-items: center;
    gap: 8px;
    box-sizing: border-box;
    padding: 0 8px 0 12px;
    /* Neutral near-black pill, glassy — matches the app's dark canvas */
    background-color: rgba(24, 22, 21, 0.9);
    border-radius: 999px;
    border: 1px solid rgba(244, 158, 23, 0.26);
    box-shadow:
      0 2px 6px rgba(0, 0, 0, 0.35),
      0 1px 1.5px rgba(0, 0, 0, 0.3),
      inset 0 0.5px 0 rgba(255, 255, 255, 0.08);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
    user-select: none;
    -webkit-user-select: none;
    white-space: nowrap;
    cursor: grab;
  }
  .hud-root:active {
    cursor: grabbing;
  }
  .hud-root.hud-prompting,
  .hud-root.hud-pending {
    padding-left: 12px;
    padding-right: 6px;
  }
  .hud-pending .hud-label {
    font-size: 12px;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  .hud-grip {
    display: inline-flex;
    align-items: center;
    flex-shrink: 0;
    color: rgba(255, 255, 255, 0.25);
    transition: color 0.12s;
  }
  .hud-root:hover .hud-grip {
    color: rgba(255, 255, 255, 0.55);
  }

  .hud-dot {
    width: 10px;
    height: 10px;
    /* Never let the flex row compress the dot's width — once the elapsed
       timer grows to H:MM:SS (calls over an hour) the pill gets tight and
       a shrinkable dot renders as an ellipse (#989). */
    flex-shrink: 0;
    border-radius: 50%;
    background-color: #e85050;
    box-shadow: 0 0 6px rgba(232, 80, 80, 0.6);
    animation: hud-pulse 1.2s ease-in-out infinite;
  }
  @keyframes hud-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }

  .hud-label {
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.01em;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .hud-elapsed {
    flex-shrink: 0;
    font-size: 12px;
    font-weight: 500;
    color: rgba(245, 239, 232, 0.72);
    font-variant-numeric: tabular-nums;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
  }

  /* Trailing controls sit flush right regardless of state. */
  .hud-stop,
  .hud-dismiss,
  .hud-root > .hud-confirm-btn {
    flex-shrink: 0;
  }
  .hud-stop {
    margin-left: auto;
  }
  .hud-stop + .hud-dismiss {
    margin-left: 0;
  }

  /* ■ gets a real 26 px hit target with a visible ring — pre-fix it was a
     faint 22 px glyph on a transparent background, easy to miss, and a
     miss landed on the pill (drag region / double-click → main window). */
  .hud-stop {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    border: none;
    background-color: rgba(248, 113, 113, 0.16);
    color: #f87171;
    cursor: pointer;
    padding: 0;
    transition: background-color 0.12s, color 0.12s;
  }
  .hud-stop:hover {
    background-color: rgba(248, 113, 113, 0.3);
    color: #fca5a5;
  }

  .hud-dismiss {
    margin-left: auto;
    padding: 0;
    width: 20px;
    height: 20px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: none;
    background-color: rgba(255, 255, 255, 0.12);
    color: rgba(255, 255, 255, 0.7);
    border-radius: 50%;
    cursor: pointer;
    transition: background-color 0.12s, color 0.12s;
  }
  .hud-dismiss:hover {
    background-color: rgba(255, 255, 255, 0.26);
    color: #ffffff;
  }

  .hud-stop:focus-visible,
  .hud-dismiss:focus-visible,
  .hud-confirm-btn:focus-visible {
    outline: 2px solid rgba(244, 158, 23, 0.7);
    outline-offset: 1px;
  }

  /* Inline prompts (stop confirmation, call-end) replace the label, timer
     and waveform — the pill can't fit both. */
  .hud-prompt {
    display: flex;
    align-items: center;
    gap: 5px;
    flex: 1;
    min-width: 0;
  }
  .hud-prompt-label {
    font-size: 12px;
    font-weight: 600;
    margin-right: auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .hud-confirm-btn {
    flex-shrink: 0;
    font: inherit;
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    border-radius: 999px;
    border: none;
    cursor: pointer;
    padding: 6px 9px;
    transition: background-color 0.12s, opacity 0.12s;
  }
  .hud-confirm-btn:disabled {
    cursor: default;
    opacity: 0.55;
  }
  .hud-confirm-btn--stop {
    background: #f87171;
    color: #1a1a1a;
  }
  .hud-confirm-btn--stop:hover:not(:disabled) {
    background: #fb8f8f;
  }
  .hud-confirm-btn--keep {
    background: rgba(255, 255, 255, 0.14);
    color: #f5efe8;
  }
  .hud-confirm-btn--keep:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.24);
  }
  .hud-root > .hud-confirm-btn {
    margin-left: auto;
  }

  /* Processing / stopping: dot turns orange (accent), shimmer replaces waveform */
  .hud-processing .hud-dot {
    animation: none;
    background-color: #f49e17;
    box-shadow: 0 0 6px rgba(244, 158, 23, 0.6);
  }

  .hud-shimmer {
    flex-shrink: 0;
    width: var(--audio-waveform-width);
    height: 6px;
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
      rgba(244, 158, 23, 0.7) 50%,
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

  /* Pending: nothing is being recorded yet, so the dot is orange, not
     the red "recording" dot. Draining bar counts down to auto-start. */
  .hud-pending .hud-dot {
    background-color: #f49e17;
    box-shadow: 0 0 6px rgba(244, 158, 23, 0.6);
  }
  .hud-countdown-track {
    flex-shrink: 0;
    width: 32px;
    height: 4px;
    border-radius: 2px;
    background-color: rgba(255, 255, 255, 0.12);
    overflow: hidden;
  }
  .hud-countdown-bar {
    width: calc(var(--progress, 1) * 100%);
    height: 100%;
    background: #f49e17;
    border-radius: 2px;
    transition: none; /* rAF-driven — no CSS transition needed */
  }

  /* Done / stopped: green dot + check. HUD-local green, tuned for the dark
     pill — the light-theme --success-text (#2f7a35) would be too dark here. */
  .hud-done .hud-dot {
    animation: none;
    background-color: #74b06c;
    box-shadow: 0 0 6px rgba(116, 176, 108, 0.55);
  }
  .hud-done-check {
    flex-shrink: 0;
    color: #74b06c;
    width: 16px;
    height: 16px;
  }

  @media (prefers-reduced-motion: reduce) {
    .hud-dot { animation: none; }
    .hud-shimmer-fill { animation: none; background-position: 50% 0; }
    /* Show a static half-width bar instead of animating */
    .hud-countdown-bar { width: 50%; }
  }
</style>
