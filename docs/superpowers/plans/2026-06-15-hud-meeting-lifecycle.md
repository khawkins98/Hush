# HUD Meeting Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement three meeting lifecycle UX improvements: pre-recording countdown with cancel, stop button on HUD, and a multi-signal call-end detector that prompts users when their call has ended.

**Architecture:** Feature 2 (stop button) is frontend-only. Feature 1 (countdown) adds a `Pending` HUD state to Rust + cancellation IPC. Feature 3 (call-end detector) adds a new `call_end_detector.rs` module with a pluggable signal trait, a pure state-machine evaluation function, three concrete signal implementations, and a parallel background task. New atomics thread from `RuntimeFlags` → `SessionManager` → `PumpContext` for system audio level and Whisper silence counting.

**Tech Stack:** Rust (Tauri 2), Svelte 5, `tokio::sync::oneshot` for cancellation, `std::sync::atomic` for signal atomics, Playwright for e2e tests.

**Spec:** `docs/superpowers/specs/2026-06-15-hud-meeting-lifecycle-design.md`

---

## File Map

| File | Action | Purpose |
|------|--------|---------|
| `src/routes/hud/+page.svelte` | Modify | Add stop button + inline confirm; pending + call-may-have-ended states |
| `src/routes/+page.svelte` | Modify | Listen for `meeting:call-may-have-ended`, show banner when HUD hidden |
| `src/lib/events.ts` | Modify | Add `CallMayHaveEnded`, `CallEndCancelled` constants |
| `src/lib/types.ts` | Modify | Add `CallMayHaveEndedPayload` |
| `src-tauri/src/hud/mod.rs` | Modify | Add `Pending`, `CallMayHaveEnded` HUD state variants |
| `src-tauri/src/ipc/state.rs` | Modify | Add 4 new fields to `RuntimeFlags` |
| `src-tauri/src/ipc/commands/meeting.rs` | Modify | Add `meeting_cancel_pending` command |
| `src-tauri/src/lib.rs` | Modify | Pending countdown; spawn detector task; write `session_is_auto` |
| `src-tauri/src/meeting/mod.rs` | Modify | `pub mod call_end_detector`; re-export key types |
| `src-tauri/src/meeting/call_end_detector.rs` | Create | Trait, state machine, signals, background task |
| `src-tauri/src/meeting/events.rs` | Modify | Add `emit_call_may_have_ended`, `emit_call_end_cancelled` |
| `src-tauri/src/meeting/manager.rs` | Modify | Add new atomics to `SessionManager` |
| `src-tauri/src/meeting/lifecycle.rs` | Modify | Pass new atomics into `PumpContext` |
| `src-tauri/src/meeting/pump.rs` | Modify | Add atomics to `PumpContext`; write values each tick |
| `tests/e2e/_mock.ts` | Modify | Add handlers for new IPC/events |

---

## Task 0: Create feature branch

- [ ] **Create branch**

```bash
git checkout -b feat/hud-meeting-lifecycle
```

---

## Task 1: Stop button + inline confirmation on HUD (Feature 2)

Frontend-only. No backend changes needed — uses existing `meeting_stop_manual`.

**Files:**
- Modify: `src/routes/hud/+page.svelte`
- Modify: `tests/e2e/_mock.ts`

- [ ] **Step 1: Write e2e test (failing)**

Add to `tests/e2e/hud.spec.ts` (create if not exists):

```typescript
import { test, expect } from "@playwright/test";
import { injectMocks } from "./_mock";

test("HUD stop button shows inline confirmation", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  // Trigger recording state
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", { state: "recording", startedAtMs: Date.now() });
  });
  const stopBtn = page.getByRole("button", { name: "Stop recording" });
  await expect(stopBtn).toBeVisible();
  await stopBtn.click();
  await expect(page.getByText("Stop recording?")).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
});

test("HUD confirmation: Keep recording returns to recording state", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", { state: "recording", startedAtMs: Date.now() });
  });
  await page.getByRole("button", { name: "Stop recording" }).click();
  await page.getByRole("button", { name: "Keep recording" }).click();
  await expect(page.getByText("Stop recording?")).not.toBeVisible();
  await expect(page.getByRole("button", { name: "Stop recording" })).toBeVisible();
});

test("HUD confirmation: Stop calls meeting_stop_manual", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  let stopped = false;
  await page.exposeFunction("__onStopCalled", () => { stopped = true; });
  await page.evaluate(() => {
    (window as any).__hush_e2e_overrides = { meeting_stop_manual: async () => { (window as any).__onStopCalled(); } };
    window.__hush_e2e?.emit("hud:state", { state: "recording", startedAtMs: Date.now() });
  });
  await page.getByRole("button", { name: "Stop recording" }).click();
  await page.getByRole("button", { name: "Stop" }).click();
  expect(stopped).toBe(true);
});
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
npx playwright test tests/e2e/hud.spec.ts -g "stop button" --reporter=line
```

Expected: 3 failures (buttons not found).

- [ ] **Step 3: Add `confirmingStop` state and stop button to HUD**

In `src/routes/hud/+page.svelte`, after the existing `let elapsedLabel` declaration:

```svelte
  // True while the user has clicked Stop but not yet confirmed.
  // Resets to false if recording state changes or HUD is dismissed.
  let confirmingStop = $state(false);
```

In the `dismiss()` function, add a reset at the top:

```svelte
  async function dismiss() {
    confirmingStop = false;
    try {
      await getCurrentWebviewWindow().hide();
    } catch {
      // Hide failure is non-fatal — recording continues regardless.
    }
  }
```

In the hud state listener (inside the `listen` callback), add reset when state changes:

```svelte
      if (payload.state === "recording") {
        confirmingStop = false;  // ← add this line
        // ... existing code
```

- [ ] **Step 4: Add stop button and inline confirmation to recording branch**

In `src/routes/hud/+page.svelte`, replace the existing dismiss button block (the single `<button class="hud-dismiss">` near line 311) with:

```svelte
  {#if hudState === "recording"}
    {#if confirmingStop}
      <span class="hud-confirm-stop" data-tauri-drag-region="false">
        <span class="hud-confirm-label">Stop recording?</span>
        <button
          type="button"
          class="hud-confirm-btn hud-confirm-btn--stop"
          onclick={async () => {
            try { await invoke("meeting_stop_manual"); } catch { /* best-effort */ }
            await dismiss();
          }}
        >Stop</button>
        <button
          type="button"
          class="hud-confirm-btn hud-confirm-btn--keep"
          onclick={() => { confirmingStop = false; }}
        >Keep recording</button>
      </span>
    {:else}
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
```

- [ ] **Step 5: Add CSS for new elements**

In the `<style>` block of `src/routes/hud/+page.svelte`, add after the `.hud-dismiss` rule:

```css
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
```

- [ ] **Step 6: Run tests**

```bash
npx playwright test tests/e2e/hud.spec.ts -g "stop button" --reporter=line
npm run check
```

Expected: 3 passing, no type errors.

- [ ] **Step 7: Commit**

```bash
git add src/routes/hud/+page.svelte tests/e2e/hud.spec.ts
git commit -m "feat(ui): add stop button with inline confirmation to HUD"
```

---

## Task 2: Pending HUD state — Rust backend (Feature 1)

**Files:**
- Modify: `src-tauri/src/hud/mod.rs`
- Modify: `src-tauri/src/ipc/state.rs`
- Modify: `src-tauri/src/ipc/commands/meeting.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add `Pending` and `CallMayHaveEnded` variants to `HudState`**

In `src-tauri/src/hud/mod.rs`, replace the `HudState` enum and its `state_str` impl:

```rust
#[derive(Debug, Clone, Copy)]
pub enum HudState {
    /// Auto-detection has fired; recording will begin in ~3 s unless the
    /// user clicks "Don't record". `ends_at_ms` is the Unix-ms timestamp
    /// when the countdown expires so the frontend can animate a draining
    /// progress bar without a separate tick event.
    Pending { ends_at_ms: u64 },
    /// Audio capture is active. Pulsing dot + level meter. The
    /// `started_at_ms` field is the Unix-ms timestamp the backend
    /// captured at the moment it considered the recording started;
    /// the frontend anchors its elapsed-time counter to this so
    /// back-to-back sessions reset to 0:00 deterministically (#481).
    Recording { started_at_ms: u64 },
    /// Audio capture stopped, transcription + clipboard write in
    /// flight. Static dot, "Processing…" label, no level meter.
    Processing,
    /// Clipboard write succeeded. Green dot, "Copied!" label. The
    /// frontend self-dismisses after a brief confirmation window
    /// (~1.5 s) so the user sees a clear "safe to paste" signal
    /// before the HUD disappears (#669).
    Done,
    /// The call-end detector reached confidence threshold. Prompt
    /// the user to stop recording. `confidence` is "high" (two or
    /// more signals agree) or "medium" (one signal held for 120 s).
    CallMayHaveEnded { confidence: &'static str },
}

impl HudState {
    fn state_str(self) -> &'static str {
        match self {
            HudState::Pending { .. } => "pending",
            HudState::Recording { .. } => "recording",
            HudState::Processing => "processing",
            HudState::Done => "done",
            HudState::CallMayHaveEnded { .. } => "call-may-have-ended",
        }
    }
}
```

- [ ] **Step 2: Update `HudStatePayload` and `set_state` to handle new variants**

In `src-tauri/src/hud/mod.rs`, replace `HudStatePayload` and `set_state`:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct HudStatePayload {
    state: &'static str,
    /// Unix-ms timestamp when the pending countdown ends. Only for Pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    ends_at_ms: Option<u64>,
    /// Unix-ms timestamp when recording started. Only for Recording.
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
    /// Confidence level for call-may-have-ended. "high" or "medium".
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<&'static str>,
}

pub fn set_state<R: Runtime>(app: &AppHandle<R>, state: HudState) -> Result<()> {
    use tauri::Emitter as _;
    let payload = match state {
        HudState::Pending { ends_at_ms } => HudStatePayload {
            state: state.state_str(),
            ends_at_ms: Some(ends_at_ms),
            started_at_ms: None,
            confidence: None,
        },
        HudState::Recording { started_at_ms } => HudStatePayload {
            state: state.state_str(),
            ends_at_ms: None,
            started_at_ms: Some(started_at_ms),
            confidence: None,
        },
        HudState::Processing | HudState::Done => HudStatePayload {
            state: state.state_str(),
            ends_at_ms: None,
            started_at_ms: None,
            confidence: None,
        },
        HudState::CallMayHaveEnded { confidence } => HudStatePayload {
            state: state.state_str(),
            ends_at_ms: None,
            started_at_ms: None,
            confidence: Some(confidence),
        },
    };
    app.emit("hud:state", &payload).context("emit hud:state")?;
    Ok(())
}
```

- [ ] **Step 3: Add new fields to `RuntimeFlags`**

In `src-tauri/src/ipc/state.rs`, add four fields after `autostart_path_stale` (the last field, around line 411):

```rust
    /// Cancellation slot for the auto-start pending countdown.
    /// `meeting_cancel_pending` takes the `Sender` from the Mutex and
    /// calls `send(())` to abort the 3-second countdown in
    /// `run_meeting_detection_task`. The Mutex is std (not tokio) because
    /// the lock is never held across an await point — it is acquired,
    /// used, and released synchronously.
    pub pending_cancel: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    /// f32-as-bits RMS of the system-audio tap, written by the meeting
    /// pump after each drain of the system-audio source. Zero when no
    /// tap session is active. Read by the call-end detector's
    /// SystemAudioRmsSignal. Same encoding as RuntimeFlags::diarizer_threshold.
    ///
    /// macOS: sourced from CoreAudioTapSession::current_level().
    /// Linux/Windows future: a platform adapter feeding the same Arc.
    pub system_audio_level: Arc<std::sync::atomic::AtomicU32>,
    /// Consecutive meeting-pump inference ticks that produced no real
    /// speech (all BLANK_AUDIO or empty finals). Reset to 0 when a real
    /// utterance arrives. Written by pump::tick_inference; read by the
    /// call-end detector's WhisperSilenceSignal. Platform-agnostic.
    pub whisper_consecutive_empty_ticks: Arc<std::sync::atomic::AtomicU32>,
    /// True while the active meeting session was started by auto-detection
    /// (`run_meeting_detection_task`). False for manually-started sessions.
    /// The call-end detector only activates when this is false — auto-started
    /// sessions already have `AutoStop` via evaluate_mic_state.
    pub session_is_auto: Arc<std::sync::atomic::AtomicBool>,
```

- [ ] **Step 4: Initialize new fields in the `RuntimeFlags` constructor**

In `src-tauri/src/ipc/builder.rs` around line 511, after `autostart_path_stale: Arc::new(...),`, add the four new fields:

```rust
pending_cancel: Arc::new(std::sync::Mutex::new(None)),
system_audio_level: Arc::new(std::sync::atomic::AtomicU32::new(0)),
whisper_consecutive_empty_ticks: Arc::new(std::sync::atomic::AtomicU32::new(0)),
session_is_auto: Arc::new(std::sync::atomic::AtomicBool::new(false)),
```

- [ ] **Step 5: Add `meeting_cancel_pending` IPC command**

In `src-tauri/src/ipc/commands/meeting.rs`, add after the existing `meeting_stop_manual` function:

```rust
/// Cancel the 3-second auto-start countdown initiated by the meeting
/// detection task. One-shot: only suppresses the current pending instance.
/// If no countdown is active, this is a no-op.
#[tauri::command]
pub async fn meeting_cancel_pending(
    state: tauri::State<'_, crate::ipc::AppState>,
) -> Result<(), crate::ipc::IpcError> {
    if let Some(tx) = state.runtime_flags.pending_cancel.lock().unwrap().take() {
        let _ = tx.send(());
    }
    Ok(())
}
```

- [ ] **Step 6: Register `meeting_cancel_pending` in `generate_handler!`**

In `src-tauri/src/lib.rs`, find `tauri::generate_handler![` and add:

```rust
ipc::commands::meeting::meeting_cancel_pending,
```

- [ ] **Step 7: Add pending countdown to auto-start path in `lib.rs`**

In `src-tauri/src/lib.rs`, find the `MicStateOutcome::Start { app_name }` branch (around line 1379). Replace the HUD show block (currently near line 1449–1466) with a pending countdown that gates the start:

```rust
            MicStateOutcome::Start { app_name } => {
                session_emitted = true;

                // Show a 3-second pending countdown before auto-starting.
                // The user can click "Don't record" to cancel this instance
                // without disabling auto-start globally.
                let cancelled = if state
                    .runtime_flags
                    .hud_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    let ends_at_ms = crate::hud::now_unix_ms() + 3000;
                    crate::hud::show_async(&app);
                    if let Err(e) = crate::hud::set_state(
                        &app,
                        crate::hud::HudState::Pending { ends_at_ms },
                    ) {
                        tracing::warn!(error = ?e, "emit hud:state(pending) failed");
                    }
                    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                    *state.runtime_flags.pending_cancel.lock().unwrap() = Some(tx);
                    let was_cancelled = tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => false,
                        _ = rx => true,
                    };
                    *state.runtime_flags.pending_cancel.lock().unwrap() = None;
                    if was_cancelled {
                        crate::hud::hide_async(&app);
                        tracing::info!(app_name, "auto-start cancelled by user during countdown");
                    }
                    was_cancelled
                } else {
                    false
                };

                if cancelled {
                    session_emitted = false;
                    // next HAL event may retry; mic still active
                } else {
                    // Mark this as an auto-started session before starting
                    // so the call-end detector ignores it (AutoStop handles it).
                    state
                        .runtime_flags
                        .session_is_auto
                        .store(true, std::sync::atomic::Ordering::Relaxed);

                    // ... existing start_result block unchanged from here ...
```

Make sure `session_is_auto` is set to `false` in:
- `meeting_start_manual` (manual start)
- `stop_meeting_and_rebuild_transcriber` (session ends)

- [ ] **Step 8: Set `session_is_auto = false` in manual start and stop**

In `src-tauri/src/ipc/commands/meeting.rs`, in `meeting_start_manual`, add near the top before the start logic:

```rust
state
    .runtime_flags
    .session_is_auto
    .store(false, std::sync::atomic::Ordering::Relaxed);
```

In `stop_meeting_and_rebuild_transcriber`, add after the stop succeeds:

```rust
state
    .runtime_flags
    .session_is_auto
    .store(false, std::sync::atomic::Ordering::Relaxed);
```

- [ ] **Step 9: Build and check**

```bash
cd src-tauri && cargo build 2>&1 | head -60
```

Expected: no errors. Fix any type issues.

- [ ] **Step 10: Add TS type and event constant**

In `src/lib/types.ts`, add:

```typescript
export interface HudStatePendingPayload {
  state: "pending";
  endsAtMs: number;
}

export interface HudStateCallMayHaveEndedPayload {
  state: "call-may-have-ended";
  confidence: "high" | "medium";
}
```

In `src/lib/events.ts`, these states flow through the existing `hud:state` event — no new event constant needed. Document the new states in the existing `HudState` comment.

- [ ] **Step 11: Add Playwright mock for `meeting_cancel_pending`**

In `tests/e2e/_mock.ts`, add alongside `meeting_stop_manual`:

```typescript
meeting_cancel_pending: () => undefined,
```

- [ ] **Step 12: Commit**

```bash
git add src-tauri/src/hud/mod.rs src-tauri/src/ipc/state.rs \
        src-tauri/src/ipc/commands/meeting.rs src-tauri/src/lib.rs \
        src/lib/types.ts src/lib/events.ts tests/e2e/_mock.ts
git commit -m "feat(tauri): add pending countdown state and meeting_cancel_pending IPC"
```

---

## Task 3: Pending countdown HUD frontend (Feature 1)

**Files:**
- Modify: `src/routes/hud/+page.svelte`

- [ ] **Step 1: Write e2e tests for pending state**

Add to `tests/e2e/hud.spec.ts`:

```typescript
test("HUD pending state shows countdown bar and cancel button", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  const endsAtMs = Date.now() + 3000;
  await page.evaluate((endsAt) => {
    window.__hush_e2e?.emit("hud:state", { state: "pending", endsAtMs: endsAt });
  }, endsAtMs);
  await expect(page.getByText("Meeting detected")).toBeVisible();
  await expect(page.getByRole("button", { name: "Cancel auto-recording" })).toBeVisible();
  await expect(page.locator(".hud-countdown-bar")).toBeVisible();
});

test("HUD pending cancel calls meeting_cancel_pending", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  let cancelled = false;
  await page.exposeFunction("__onCancelCalled", () => { cancelled = true; });
  await page.evaluate((endsAt) => {
    (window as any).__hush_e2e_overrides = {
      meeting_cancel_pending: async () => { (window as any).__onCancelCalled(); },
    };
    window.__hush_e2e?.emit("hud:state", { state: "pending", endsAtMs: endsAt });
  }, Date.now() + 3000);
  await page.getByRole("button", { name: "Cancel auto-recording" }).click();
  expect(cancelled).toBe(true);
});
```

- [ ] **Step 2: Run to confirm failure**

```bash
npx playwright test tests/e2e/hud.spec.ts -g "pending" --reporter=line
```

Expected: failures.

- [ ] **Step 3: Update `HudStatePayload` type and `hudState` in frontend**

In `src/routes/hud/+page.svelte`, update the `HudStatePayload` type:

```typescript
  type HudStatePayload = {
    state: "pending" | "recording" | "processing" | "done" | "call-may-have-ended";
    endsAtMs?: number;
    startedAtMs?: number;
    confidence?: "high" | "medium";
  };
```

Update the `hudState` type:

```typescript
  let hudState = $state<"pending" | "recording" | "processing" | "done" | "call-may-have-ended" | null>(null);
```

Add a new state variable for the countdown end time:

```typescript
  // Unix-ms when the pending countdown expires. Drives the draining
  // progress bar in the `pending` state via rAF.
  let pendingEndsAtMs = $state<number | null>(null);
```

- [ ] **Step 4: Handle `pending` state in the event listener**

In the `listen(Events.HudState, ...)` callback, add a branch for `"pending"`:

```typescript
      if (payload.state === "pending") {
        hudState = "pending";
        pendingEndsAtMs = payload.endsAtMs ?? null;
        confirmingStop = false;
        return;
      }
```

- [ ] **Step 5: Add pending render branch**

In the HUD template, add a `{:else if hudState === "pending"}` branch. Insert it after the `{#if hudState === "recording"}` block's `AudioWaveform` but before the shimmer:

Actually, add it as the first conditional in the dynamic content area — before the waveform/shimmer section. Replace the label logic to handle "pending":

In the `<span class="hud-label">` element, add `pending` to the ternary:

```svelte
    {hudState === "processing"
      ? transcriptionProgress !== null
        ? `Processing… ${transcriptionProgress}%`
        : "Processing…"
      : hudState === "done"
        ? "Copied!"
        : hudState === "pending"
          ? "Meeting detected"
          : "Recording"}
```

After the `{#if hudState === "recording"}` waveform block and `{:else if hudState === "processing"}` shimmer block, add:

```svelte
  {:else if hudState === "pending"}
    <!--
      Countdown bar: drains from full width to zero over the pending
      interval. Uses a CSS custom property updated each rAF tick so the
      animation is smooth without triggering layout. `prefers-reduced-motion`
      shows a static bar instead.
    -->
    <div
      class="hud-countdown-bar"
      role="progressbar"
      aria-label="Recording starts soon"
      style="--progress: {pendingEndsAtMs
        ? Math.max(0, Math.min(1, (pendingEndsAtMs - Date.now()) / 3000))
        : 0}"
    ></div>
    <button
      type="button"
      class="hud-dismiss"
      aria-label="Cancel auto-recording"
      title="Cancel"
      data-tauri-drag-region="false"
      onclick={async () => {
        try { await invoke("meeting_cancel_pending"); } catch { /* best-effort */ }
        await dismiss();
      }}
      ondblclick={(e) => e.stopPropagation()}
    >
      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
        <path d="M2 2 L10 10 M10 2 L2 10" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
      </svg>
    </button>
```

Note: the `--progress` CSS variable is updated by Svelte's reactivity. For the smooth animation, add a `$effect` that ticks `pendingEndsAtMs` references via rAF — actually, the simplest approach is to use a reactive store or a tick counter. Add:

```typescript
  // rAF tick counter to force re-evaluation of the progress bar width.
  // Incremented each animation frame while hudState === "pending".
  let pendingTick = $state(0);
  $effect(() => {
    if (hudState !== "pending") return;
    let id: number;
    const tick = () => {
      pendingTick++;
      id = requestAnimationFrame(tick);
    };
    id = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(id);
  });
```

Then update the `--progress` binding to reference `pendingTick` (to trigger re-render):

```svelte
      style="--progress: {pendingTick >= 0 && pendingEndsAtMs
        ? Math.max(0, Math.min(1, (pendingEndsAtMs - Date.now()) / 3000))
        : 0}"
```

- [ ] **Step 6: Add countdown bar CSS**

In `<style>`:

```css
  .hud-countdown-bar {
    width: calc(var(--progress, 1) * 60px);
    height: 3px;
    background: #f49e17;
    border-radius: 2px;
    transition: width 0.1s linear;
    flex-shrink: 0;
  }

  @media (prefers-reduced-motion: reduce) {
    .hud-countdown-bar {
      transition: none;
      width: 60px; /* static, no animation */
    }
  }
```

- [ ] **Step 7: Run tests and type-check**

```bash
npx playwright test tests/e2e/hud.spec.ts -g "pending" --reporter=line
npm run check
```

Expected: passing.

- [ ] **Step 8: Commit**

```bash
git add src/routes/hud/+page.svelte tests/e2e/hud.spec.ts
git commit -m "feat(ui): add pending countdown HUD state with cancel button"
```

---

## Task 4: Thread new atomics through `SessionManager` and `PumpContext`

**Files:**
- Modify: `src-tauri/src/meeting/manager.rs`
- Modify: `src-tauri/src/meeting/lifecycle.rs`
- Modify: `src-tauri/src/meeting/pump.rs`

- [ ] **Step 1: Add atomics to `SessionManager`**

In `src-tauri/src/meeting/manager.rs`, find the `SessionManager` struct fields (near `pub(super) mic_gain_db`). Add:

```rust
    /// f32-as-bits RMS of the system-audio tap, written each pump tick.
    /// Cloned from RuntimeFlags and passed into PumpContext.
    pub(super) system_audio_level: Arc<std::sync::atomic::AtomicU32>,
    /// Consecutive pump ticks with no real speech. Reset to 0 on any real
    /// utterance. Cloned from RuntimeFlags and passed into PumpContext.
    pub(super) whisper_consecutive_empty_ticks: Arc<std::sync::atomic::AtomicU32>,
```

Find `SessionManager::new()` (near line 241) and add to the parameter list:

```rust
        system_audio_level: Arc<std::sync::atomic::AtomicU32>,
        whisper_consecutive_empty_ticks: Arc<std::sync::atomic::AtomicU32>,
```

And initialize them in the struct literal:

```rust
            system_audio_level,
            whisper_consecutive_empty_ticks,
```

- [ ] **Step 2: Pass new atomics when constructing `SessionManager`**

Search for where `SessionManager::new(` is called (in `AppStateBuilder` or `lib.rs setup`). Add:

```rust
system_audio_level: Arc::clone(&runtime_flags.system_audio_level),
whisper_consecutive_empty_ticks: Arc::clone(&runtime_flags.whisper_consecutive_empty_ticks),
```

- [ ] **Step 3: Add atomics to `PumpContext`**

In `src-tauri/src/meeting/pump.rs`, add to the `PumpContext` struct after `mic_gain_db`:

```rust
    /// f32-as-bits RMS of the system-audio source, written each tick.
    /// Read by the call-end detector's SystemAudioRmsSignal.
    ///
    /// macOS: sourced from CoreAudioTapSession::current_level().
    /// Linux/Windows future: a platform adapter writes the same Arc.
    pub system_audio_level: Arc<std::sync::atomic::AtomicU32>,
    /// Consecutive pump ticks that produced no real speech (all BLANK_AUDIO
    /// or empty finals). Reset to 0 when a real utterance arrives.
    /// Read by the call-end detector's WhisperSilenceSignal.
    /// Platform-agnostic — written by Rust pump logic.
    pub whisper_consecutive_empty_ticks: Arc<std::sync::atomic::AtomicU32>,
```

- [ ] **Step 4: Pass atomics in `lifecycle.rs` when constructing `PumpContext`**

In `src-tauri/src/meeting/lifecycle.rs`, add to the `PumpContext { ... }` constructor (after `mic_gain_db`):

```rust
system_audio_level: Arc::clone(&self.system_audio_level),
whisper_consecutive_empty_ticks: Arc::clone(&self.whisper_consecutive_empty_ticks),
```

- [ ] **Step 5: Build to confirm no errors**

```bash
cd src-tauri && cargo build 2>&1 | head -60
```

Expected: clean build (new unused fields will warn — that's fine, they'll be used in Task 5).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/meeting/manager.rs src-tauri/src/meeting/lifecycle.rs \
        src-tauri/src/meeting/pump.rs
git commit -m "refactor(meeting): thread system_audio_level and whisper_empty_ticks atomics through pump"
```

---

## Task 5: Write atomics in pump per-tick

**Files:**
- Modify: `src-tauri/src/meeting/pump.rs`

- [ ] **Step 1: Write `system_audio_level` after draining system audio source**

In `pump.rs`, find `tick_drain_sources` (the function that iterates over sources and drains each). After the drain for the system audio source, add:

Find the loop that iterates `for i in 0..ctx.sources.len()` (or similar). After the drain/inference for source `i`, add:

```rust
        // Write system-audio RMS for the call-end detector.
        // Only the system-audio source has meaningful level for this signal;
        // mic level is more reliably tracked via the HAL IsRunningSomewhere property.
        if matches!(ctx.sources[i], crate::audio::AudioSource::SystemAudio) {
            if let Some(handle) = &ctx.handles[i] {
                ctx.system_audio_level.store(
                    handle.current_level().to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
```

The right location is in `tick_drain_sources`, after the drain of each source. Find where `ctx.sources[i]` is checked for `AudioSource::Microphone` (around line 491) — add the system audio level write in the same loop, in the `else` branch for non-mic sources.

- [ ] **Step 2: Write `whisper_consecutive_empty_ticks` in inference result processing**

In `pump.rs`, find `tick_inference` (or wherever finals are collected after Whisper inference). Find the `blank_counts` / `final_counts` update (around line 969–972). After that block, add:

```rust
        // Update the consecutive-silence counter for the call-end detector.
        // Only track the mic source (source 0 in standard mic+system layout)
        // because Whisper only runs on mic audio; system audio uses the
        // CoreAudioTap level signal instead.
        if matches!(ctx.sources[i], crate::audio::AudioSource::Microphone(_)) {
            let all_blank = utterances
                .iter()
                .filter(|u| u.is_final)
                .all(|u| u.text == "[BLANK_AUDIO]" || u.text.trim().is_empty());
            let has_finals = utterances.iter().any(|u| u.is_final);
            if has_finals && all_blank {
                ctx.whisper_consecutive_empty_ticks
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            } else if has_finals && !all_blank {
                // Real speech detected — reset silence counter immediately.
                ctx.whisper_consecutive_empty_ticks
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
            // If no finals this tick, leave the counter unchanged.
        }
```

- [ ] **Step 3: Reset both atomics in pump cleanup**

Find where the pump cleans up at the end of `run_pump` (after `release_audio_handles` or in the tail flush). Add:

```rust
    // Reset call-end detector atomics so a stale reading doesn't influence
    // the detector during the gap between sessions.
    ctx.system_audio_level
        .store(0, std::sync::atomic::Ordering::Relaxed);
    ctx.whisper_consecutive_empty_ticks
        .store(0, std::sync::atomic::Ordering::Relaxed);
```

- [ ] **Step 4: Build and run Rust unit tests**

```bash
cd src-tauri && cargo build 2>&1 | head -40
cd src-tauri && cargo test --lib 2>&1 | tail -20
```

Expected: clean build, all existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/meeting/pump.rs
git commit -m "feat(meeting): write system audio level and Whisper silence counter each pump tick"
```

---

## Task 6: `call_end_detector.rs` — trait, state machine, unit tests

**Files:**
- Create: `src-tauri/src/meeting/call_end_detector.rs`
- Modify: `src-tauri/src/meeting/mod.rs`

- [ ] **Step 1: Create `call_end_detector.rs` with the trait, types, and `evaluate_call_end_state`**

Create `src-tauri/src/meeting/call_end_detector.rs`:

```rust
//! Call-end detection for meeting sessions.
//!
//! Detects when a meeting call has ended while the meeting app stays open
//! (the common case: Zoom call ends, Zoom app remains running). Emits a
//! `meeting:call-may-have-ended` event so the HUD can prompt the user to
//! stop recording.
//!
//! # Architecture
//!
//! A `CallEndSignal` trait abstracts each observable signal. Three concrete
//! implementations cover the available macOS signals:
//! - `MicHalSignal` — CoreAudio `kAudioDevicePropertyDeviceIsRunningSomewhere`
//! - `SystemAudioRmsSignal` — RMS of the system-audio tap (rolling window)
//! - `WhisperSilenceSignal` — consecutive empty Whisper inference ticks
//!
//! `evaluate_call_end_state` is a pure function over explicit enum inputs/outputs,
//! following the `evaluate_mic_state` pattern from `mic_camera_monitor.rs`.
//! It is deterministic and fully unit-tested with injected `Instant` values.
//!
//! # Platform notes
//!
//! macOS: `MicHalSignal` uses CoreAudio HAL (no entitlements needed for same-UID
//!   processes). `SystemAudioRmsSignal` reads the CoreAudioTap level atomic.
//!
//! Linux (future): Replace `MicHalSignal` with an ALSA/PulseAudio open-device
//!   watcher behind `#[cfg(target_os = "linux")]`. `SystemAudioRmsSignal` and
//!   `WhisperSilenceSignal` are platform-agnostic; a PulseAudio adapter would
//!   feed the same `Arc<AtomicU32>` as the macOS tap.
//!
//! Windows (future): `IMMDeviceEnumerator + IMMNotificationClient` for mic;
//!   `IAudioMeterInformation::GetPeakValue` for system audio. Same trait, new structs.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Signal abstraction ────────────────────────────────────────────────────────

/// Observation from a single call-end signal source.
pub struct SignalReading {
    /// 0.0 = no evidence of call-end; 1.0 = strong evidence.
    /// Only 0.0 and 1.0 are used currently; the f32 type is kept for
    /// future weighted extensions without a trait change.
    pub strength: f32,
    /// True when this signal actively contradicts call-end — e.g. mic just
    /// became active again, or Whisper transcribed real speech.
    /// A reversal immediately resets the state machine to `Monitoring`.
    pub is_reversal: bool,
}

/// Pluggable call-end signal source.
///
/// Implementors must be `Send + Sync`; the detector task holds them behind
/// `Arc<dyn CallEndSignal>`. `poll()` must not block — it reads atomics or
/// in-memory state only.
pub trait CallEndSignal: Send + Sync {
    fn name(&self) -> &'static str;
    fn poll(&self) -> SignalReading;
}

// ── State machine types ───────────────────────────────────────────────────────

/// Which signals are currently active. Plain struct of bools (not bitflags)
/// to keep the code readable without an extra dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SignalSet {
    pub mic_inactive: bool,
    pub system_audio_quiet: bool,
    pub whisper_silent: bool,
}

impl SignalSet {
    pub fn active_count(self) -> u8 {
        self.mic_inactive as u8 + self.system_audio_quiet as u8 + self.whisper_silent as u8
    }
}

/// State machine node.
#[derive(Debug, Clone)]
pub enum CallEndState {
    /// No suspicion. Default while session signals are normal.
    Monitoring,
    /// One or more signals suggest the call may have ended.
    /// `since`: when suspicion began (used for timing thresholds).
    /// `signals`: which signals were active at transition time.
    Suspected {
        since: Instant,
        signals: SignalSet,
    },
    /// Confidence threshold reached. Caller should emit the IPC event.
    /// Immediately transitions back to `Monitoring` so the detector
    /// re-arms for the next call.
    Confident { since: Instant },
}

impl Default for CallEndState {
    fn default() -> Self {
        Self::Monitoring
    }
}

/// What the state machine should do after one evaluation tick.
#[derive(Debug, PartialEq)]
pub enum CallEndTransition {
    Stay,
    EnterSuspected { signals: SignalSet },
    EnterConfident,
    Reset,
}

/// Inputs to one evaluation tick. All plain data — no IO, no async.
pub struct CallEndInputs {
    pub mic_inactive: bool,
    pub system_audio_quiet: bool,
    pub whisper_silent: bool,
    /// Guard: only activate for manually-started sessions.
    /// Auto-started sessions already have `AutoStop` via `evaluate_mic_state`.
    pub session_is_manual: bool,
    /// True when any signal is a reversal (mic went active, loud audio spike,
    /// Whisper produced real speech). Computed by the caller from `poll()` results.
    pub any_reversal: bool,
}

/// Timing thresholds (all tunable via env vars for dev/testing).
pub struct CallEndThresholds {
    /// How long `Suspected` must hold with ≥ 2 active signals before → Confident.
    pub confirm_two_signal_secs: u64,
    /// How long `Suspected` must hold with 1 active signal before → Confident.
    pub confirm_one_signal_secs: u64,
}

impl Default for CallEndThresholds {
    fn default() -> Self {
        Self {
            confirm_two_signal_secs: std::env::var("HUSH_CE_CONFIRM_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            confirm_one_signal_secs: std::env::var("HUSH_CE_SOLO_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
        }
    }
}

/// Pure state-machine evaluation. No IO, no time-reading — `now` is injected.
///
/// Follows the `evaluate_mic_state()` pattern: pure function over explicit
/// enum inputs → enum output. Testable without any async runtime or wall clock.
pub fn evaluate_call_end_state(
    current: &CallEndState,
    inputs: &CallEndInputs,
    now: Instant,
    thresholds: &CallEndThresholds,
) -> CallEndTransition {
    if !inputs.session_is_manual {
        return CallEndTransition::Stay;
    }

    // A reversal signal (mic active, loud audio, real speech) resets
    // immediately from any state — a resuming call cancels the prompt.
    if inputs.any_reversal {
        return CallEndTransition::Reset;
    }

    let signal_set = SignalSet {
        mic_inactive: inputs.mic_inactive,
        system_audio_quiet: inputs.system_audio_quiet,
        whisper_silent: inputs.whisper_silent,
    };

    match current {
        CallEndState::Monitoring => {
            if signal_set.active_count() >= 1 {
                CallEndTransition::EnterSuspected { signals: signal_set }
            } else {
                CallEndTransition::Stay
            }
        }
        CallEndState::Suspected { since, .. } => {
            let elapsed = now.duration_since(*since).as_secs();
            if signal_set.active_count() >= 2
                && elapsed >= thresholds.confirm_two_signal_secs
            {
                CallEndTransition::EnterConfident
            } else if signal_set.active_count() >= 1
                && elapsed >= thresholds.confirm_one_signal_secs
            {
                CallEndTransition::EnterConfident
            } else {
                CallEndTransition::Stay
            }
        }
        CallEndState::Confident { .. } => {
            // Re-arm immediately after emitting the event.
            CallEndTransition::Reset
        }
    }
}

// ── Concrete signal implementations ──────────────────────────────────────────

/// Tracks how long the mic HAL device has been inactive.
///
/// macOS: wraps `MicCameraMonitor::is_any_device_active()`.
/// Linux future: ALSA device open count via /proc, or PulseAudio state change.
/// Windows future: `IMMDeviceEnumerator + IMMNotificationClient`.
///
/// Reliability: HIGH for standard apps. LOW when virtual audio pipelines
/// (Krisp, SoundSource, BlackHole aggregates) hold the HAL device open
/// past call end — those sources prevent `DeviceIsRunningSomewhere` from
/// clearing even when no call is in progress.
#[cfg(target_os = "macos")]
pub struct MicHalSignal {
    monitor: Arc<crate::meeting::mic_camera_monitor::MicCameraMonitor>,
    inactive_since: Mutex<Option<Instant>>,
    threshold_secs: u64,
}

#[cfg(target_os = "macos")]
impl MicHalSignal {
    pub fn new(
        monitor: Arc<crate::meeting::mic_camera_monitor::MicCameraMonitor>,
    ) -> Self {
        let threshold_secs = std::env::var("HUSH_CE_MIC_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        Self {
            monitor,
            inactive_since: Mutex::new(None),
            threshold_secs,
        }
    }
}

#[cfg(target_os = "macos")]
impl CallEndSignal for MicHalSignal {
    fn name(&self) -> &'static str {
        "mic-hal"
    }

    fn poll(&self) -> SignalReading {
        let is_active = self.monitor.is_any_device_active();
        let mut guard = self.inactive_since.lock().unwrap();
        if is_active {
            *guard = None;
            return SignalReading { strength: 0.0, is_reversal: true };
        }
        let since = guard.get_or_insert_with(Instant::now);
        let elapsed = since.elapsed().as_secs();
        SignalReading {
            strength: if elapsed >= self.threshold_secs { 1.0 } else { 0.0 },
            is_reversal: false,
        }
    }
}

/// Tracks sustained silence on the system-audio tap using a rolling RMS window.
///
/// Platform-agnostic struct; the `system_audio_level` Arc is written by
/// the meeting pump after each drain of the system-audio source.
///
/// macOS source: `CoreAudioTapSession::current_level()`.
/// Linux future: PulseAudio peak detect or pipewire volume event.
/// Windows future: `IAudioMeterInformation::GetPeakValue`.
///
/// Reliability: MEDIUM standalone. Browser tabs, OS notifications, and
/// background music all spike the level. The 60-second window is deliberately
/// long. Most valuable as a confirming signal when `MicHalSignal` is blocked
/// by a virtual audio pipeline.
pub struct SystemAudioRmsSignal {
    /// Written by meeting pump; read here. f32-as-bits (same encoding as
    /// other AtomicU32 level fields in this codebase).
    system_audio_level: Arc<AtomicU32>,
    /// Rolling window of recent RMS readings (one entry per poll interval).
    window: Mutex<VecDeque<f32>>,
    quiet_threshold: f32,
    /// How many poll ticks to keep in the window (window_secs / poll_interval_secs).
    window_capacity: usize,
    /// Level must be below quiet_threshold for this many consecutive window
    /// entries before strength goes to 1.0.
    required_quiet_ticks: usize,
    quiet_since: Mutex<Option<Instant>>,
    required_quiet_secs: u64,
}

impl SystemAudioRmsSignal {
    pub fn new(system_audio_level: Arc<AtomicU32>, poll_interval_secs: u64) -> Self {
        let window_secs: u64 = std::env::var("HUSH_CE_AUDIO_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let quiet_threshold: f32 = std::env::var("HUSH_CE_AUDIO_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.005); // ≈ -46 dBFS
        let capacity = (window_secs / poll_interval_secs).max(1) as usize;
        Self {
            system_audio_level,
            window: Mutex::new(VecDeque::with_capacity(capacity)),
            quiet_threshold,
            window_capacity: capacity,
            required_quiet_ticks: capacity,
            quiet_since: Mutex::new(None),
            required_quiet_secs: window_secs,
        }
    }
}

impl CallEndSignal for SystemAudioRmsSignal {
    fn name(&self) -> &'static str {
        "system-audio-rms"
    }

    fn poll(&self) -> SignalReading {
        let rms = f32::from_bits(self.system_audio_level.load(Ordering::Relaxed));
        let mut window = self.window.lock().unwrap();
        if window.len() >= self.window_capacity {
            window.pop_front();
        }
        window.push_back(rms);
        let mean = if window.is_empty() {
            0.0
        } else {
            window.iter().copied().sum::<f32>() / window.len() as f32
        };

        // A loud spike (3× threshold) is a reversal — call is definitely active.
        if mean > self.quiet_threshold * 3.0 {
            *self.quiet_since.lock().unwrap() = None;
            return SignalReading { strength: 0.0, is_reversal: true };
        }

        let all_quiet = window.len() >= self.required_quiet_ticks
            && window.iter().all(|&s| s < self.quiet_threshold);

        if !all_quiet {
            *self.quiet_since.lock().unwrap() = None;
            return SignalReading { strength: 0.0, is_reversal: false };
        }

        let mut qs = self.quiet_since.lock().unwrap();
        let since = qs.get_or_insert_with(Instant::now);
        SignalReading {
            strength: if since.elapsed().as_secs() >= self.required_quiet_secs {
                1.0
            } else {
                0.0
            },
            is_reversal: false,
        }
    }
}

/// Tracks Whisper inference ticks that produced no real speech.
///
/// Platform-agnostic — reads an `AtomicU32` maintained by the Rust pump layer.
/// Reset to 0 by the pump whenever a real utterance is produced.
///
/// Reliability: HIGH semantic signal. BLANK_AUDIO / empty finals mean VAD
/// gated the inference window — actual audio energy was below Whisper's
/// `no_speech_thold`. False-positive risk: long thinking pauses mid-call can
/// produce 20–30 quiet ticks. Mitigated by requiring `Suspected` state to hold
/// for `confirm_*_secs` before promoting to `Confident`.
pub struct WhisperSilenceSignal {
    consecutive_empty_ticks: Arc<AtomicU32>,
    threshold_ticks: u32,
}

impl WhisperSilenceSignal {
    pub fn new(consecutive_empty_ticks: Arc<AtomicU32>) -> Self {
        let threshold_ticks = std::env::var("HUSH_CE_WHISPER_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20); // ~10 s at 500 ms pump tick
        Self { consecutive_empty_ticks, threshold_ticks }
    }
}

impl CallEndSignal for WhisperSilenceSignal {
    fn name(&self) -> &'static str {
        "whisper-silence"
    }

    fn poll(&self) -> SignalReading {
        let ticks = self.consecutive_empty_ticks.load(Ordering::Relaxed);
        if ticks == 0 {
            // Real speech just arrived — strongest reversal.
            return SignalReading { strength: 0.0, is_reversal: true };
        }
        SignalReading {
            strength: if ticks >= self.threshold_ticks { 1.0 } else { 0.0 },
            is_reversal: false,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> CallEndThresholds {
        CallEndThresholds {
            confirm_two_signal_secs: 30,
            confirm_one_signal_secs: 120,
        }
    }

    fn inputs_manual(mic_inactive: bool, sys_quiet: bool, whisper: bool, reversal: bool) -> CallEndInputs {
        CallEndInputs {
            mic_inactive,
            system_audio_quiet: sys_quiet,
            whisper_silent: whisper,
            session_is_manual: true,
            any_reversal: reversal,
        }
    }

    #[test]
    fn stays_when_session_not_manual() {
        let state = CallEndState::Monitoring;
        let inputs = CallEndInputs {
            mic_inactive: true,
            system_audio_quiet: true,
            whisper_silent: true,
            session_is_manual: false,
            any_reversal: false,
        };
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Stay
        );
    }

    #[test]
    fn monitoring_enters_suspected_on_one_signal() {
        let state = CallEndState::Monitoring;
        let inputs = inputs_manual(true, false, false, false);
        let t = thresholds();
        assert!(matches!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::EnterSuspected { .. }
        ));
    }

    #[test]
    fn reversal_resets_from_monitoring() {
        let state = CallEndState::Monitoring;
        let inputs = inputs_manual(false, false, false, true);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }

    #[test]
    fn suspected_stays_before_threshold() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet { mic_inactive: true, ..Default::default() },
        };
        let inputs = inputs_manual(true, true, false, false);
        let t = thresholds();
        // 29 seconds elapsed — just under two-signal threshold (30s)
        let now = since + std::time::Duration::from_secs(29);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::Stay
        );
    }

    #[test]
    fn suspected_promotes_to_confident_two_signals_at_30s() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet { mic_inactive: true, system_audio_quiet: true, ..Default::default() },
        };
        let inputs = inputs_manual(true, true, false, false);
        let t = thresholds();
        let now = since + std::time::Duration::from_secs(30);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::EnterConfident
        );
    }

    #[test]
    fn suspected_promotes_one_signal_at_120s() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet { system_audio_quiet: true, ..Default::default() },
        };
        let inputs = inputs_manual(false, true, false, false);
        let t = thresholds();
        let now = since + std::time::Duration::from_secs(120);
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, now, &t),
            CallEndTransition::EnterConfident
        );
    }

    #[test]
    fn reversal_resets_from_suspected() {
        let since = Instant::now();
        let state = CallEndState::Suspected {
            since,
            signals: SignalSet { mic_inactive: true, ..Default::default() },
        };
        let inputs = inputs_manual(false, false, false, true);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }

    #[test]
    fn confident_resets_immediately() {
        let state = CallEndState::Confident { since: Instant::now() };
        let inputs = inputs_manual(true, true, true, false);
        let t = thresholds();
        assert_eq!(
            evaluate_call_end_state(&state, &inputs, Instant::now(), &t),
            CallEndTransition::Reset
        );
    }
}
```

- [ ] **Step 2: Add `pub mod call_end_detector` to `meeting/mod.rs`**

In `src-tauri/src/meeting/mod.rs`, add alongside other `pub mod` declarations:

```rust
pub mod call_end_detector;
```

- [ ] **Step 3: Run the unit tests**

```bash
cd src-tauri && cargo test --lib meeting::call_end_detector 2>&1
```

Expected: 7 tests pass.

- [ ] **Step 4: Run cross-platform lint**

```bash
cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings 2>&1 | head -40
```

Fix any warnings.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/meeting/call_end_detector.rs src-tauri/src/meeting/mod.rs
git commit -m "feat(meeting): add call_end_detector — trait, state machine, signals, unit tests"
```

---

## Task 7: `run_call_end_detector_task` + event emission

**Files:**
- Modify: `src-tauri/src/meeting/call_end_detector.rs`
- Modify: `src-tauri/src/meeting/events.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/events.ts`
- Modify: `src/lib/types.ts`
- Modify: `tests/e2e/_mock.ts`

- [ ] **Step 1: Add event types and emitters to `events.rs`**

In `src-tauri/src/meeting/events.rs`, add:

```rust
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CallMayHaveEndedPayload {
    /// "high" when two or more signals agree (promoted after 30 s).
    /// "medium" when a single signal held for 120 s (virtual device path).
    pub confidence: &'static str,
    /// Human-readable summary for the HUD tooltip and log.
    /// Example: "Mic inactive 10s + system audio quiet 65s"
    pub signal_summary: String,
}

pub fn emit_call_may_have_ended<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    payload: CallMayHaveEndedPayload,
) {
    use tauri::Emitter as _;
    if let Err(e) = app.emit("meeting:call-may-have-ended", &payload) {
        tracing::warn!(error = ?e, "emit meeting:call-may-have-ended failed");
    }
}

pub fn emit_call_end_cancelled<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Emitter as _;
    if let Err(e) = app.emit("meeting:call-end-cancelled", ()) {
        tracing::warn!(error = ?e, "emit meeting:call-end-cancelled failed");
    }
}
```

- [ ] **Step 2: Add `run_call_end_detector_task` to `call_end_detector.rs`**

Append to `src-tauri/src/meeting/call_end_detector.rs`:

```rust
// ── Background task ───────────────────────────────────────────────────────────

/// Poll interval in seconds. 5 s gives ±5 s jitter on 30–120 s thresholds,
/// which is irrelevant to UX. Cheap: three atomic reads per tick.
const POLL_INTERVAL_SECS: u64 = 5;

/// Spawn as a parallel task alongside `run_meeting_detection_task` in lib.rs.
///
/// Only activates when `session_is_auto` is false (manual sessions). Auto-
/// started sessions already have `AutoStop` via `evaluate_mic_state`.
///
/// macOS: spawned unconditionally; `MicHalSignal` is compiled only on macOS.
/// Linux/Windows: the two platform-agnostic signals still run; prompting is
/// less reliable without `MicHalSignal` but still useful.
pub async fn run_call_end_detector_task(app: tauri::AppHandle) {
    use tokio::time::{interval, Duration};

    let state = app.state::<crate::ipc::AppState>();

    #[cfg(target_os = "macos")]
    let monitor = {
        // Re-use the monitor the detection task also holds, via a fresh Arc clone
        // from AppState. The monitor is cheap to read; no lock contention.
        Arc::clone(&state.mic_monitor)
    };

    let signals: Vec<Arc<dyn CallEndSignal>> = vec![
        // macOS only: HAL device activity (kAudioDevicePropertyDeviceIsRunningSomewhere).
        // Linux future: ALSA/PulseAudio device open detection behind cfg(target_os = "linux").
        // Windows future: IMMDeviceEnumerator + IMMNotificationClient.
        #[cfg(target_os = "macos")]
        Arc::new(MicHalSignal::new(Arc::clone(&monitor))),

        // Platform-agnostic: reads Arc<AtomicU32> written by pump (macOS CoreAudioTap).
        // Linux/Windows future: a platform adapter writes the same atomic.
        Arc::new(SystemAudioRmsSignal::new(
            Arc::clone(&state.runtime_flags.system_audio_level),
            POLL_INTERVAL_SECS,
        )),

        // Fully platform-agnostic: reads Whisper pump silence counter.
        Arc::new(WhisperSilenceSignal::new(Arc::clone(
            &state.runtime_flags.whisper_consecutive_empty_ticks,
        ))),
    ];

    let thresholds = CallEndThresholds::default();
    let mut detector_state = CallEndState::default();
    let mut ticker = interval(Duration::from_secs(POLL_INTERVAL_SECS));

    loop {
        ticker.tick().await;

        // Only activate for manually-started sessions.
        let session_is_manual = !state
            .runtime_flags
            .session_is_auto
            .load(Ordering::Relaxed);

        // Gather readings from all signals.
        let readings: Vec<SignalReading> = signals.iter().map(|s| s.poll()).collect();
        let any_reversal = readings.iter().any(|r| r.is_reversal);

        // Map signal names to flags for CallEndInputs.
        let mut mic_inactive = false;
        let mut system_audio_quiet = false;
        let mut whisper_silent = false;
        for (signal, reading) in signals.iter().zip(readings.iter()) {
            if reading.strength >= 1.0 {
                match signal.name() {
                    "mic-hal" => mic_inactive = true,
                    "system-audio-rms" => system_audio_quiet = true,
                    "whisper-silence" => whisper_silent = true,
                    _ => {}
                }
            }
        }

        let inputs = CallEndInputs {
            mic_inactive,
            system_audio_quiet,
            whisper_silent,
            session_is_manual,
            any_reversal,
        };

        let transition = evaluate_call_end_state(
            &detector_state,
            &inputs,
            Instant::now(),
            &thresholds,
        );

        match transition {
            CallEndTransition::Stay => {}
            CallEndTransition::EnterSuspected { signals: sigs } => {
                tracing::debug!(
                    mic = sigs.mic_inactive,
                    audio = sigs.system_audio_quiet,
                    whisper = sigs.whisper_silent,
                    "call-end detector: entering Suspected"
                );
                detector_state = CallEndState::Suspected {
                    since: Instant::now(),
                    signals: sigs,
                };
            }
            CallEndTransition::EnterConfident => {
                let active_count = SignalSet {
                    mic_inactive,
                    system_audio_quiet,
                    whisper_silent,
                }.active_count();
                let confidence = if active_count >= 2 { "high" } else { "medium" };

                // Build a human-readable summary for the HUD tooltip.
                let mut parts = Vec::new();
                if mic_inactive { parts.push("mic inactive"); }
                if system_audio_quiet { parts.push("system audio quiet"); }
                if whisper_silent { parts.push("no speech detected"); }
                let signal_summary = parts.join(" + ");

                tracing::info!(confidence, signal_summary, "call-end detector: emitting event");

                // Emit to both the HUD and the main window (both listen).
                crate::meeting::events::emit_call_may_have_ended(
                    &app,
                    crate::meeting::events::CallMayHaveEndedPayload {
                        confidence,
                        signal_summary,
                    },
                );

                // Also update HUD state directly so the overlay shows the prompt.
                if state.runtime_flags.hud_enabled.load(Ordering::Relaxed) {
                    let _ = crate::hud::set_state(
                        &app,
                        crate::hud::HudState::CallMayHaveEnded { confidence },
                    );
                }

                detector_state = CallEndState::Confident { since: Instant::now() };
            }
            CallEndTransition::Reset => {
                if matches!(detector_state, CallEndState::Suspected { .. } | CallEndState::Confident { .. }) {
                    tracing::debug!("call-end detector: reversal — resetting to Monitoring");
                    // If we were in Confident (event already emitted), notify
                    // the frontend to dismiss the prompt.
                    if matches!(detector_state, CallEndState::Confident { .. }) {
                        crate::meeting::events::emit_call_end_cancelled(&app);
                    }
                }
                detector_state = CallEndState::Monitoring;
            }
        }
    }
}
```

- [ ] **Step 3: Create a fresh `MicCameraMonitor` inside the detector task**

`AppState` does not expose the monitor from `run_meeting_detection_task`. Creating a second instance is safe — both instances register HAL listeners for the same `kAudioDevicePropertyDeviceIsRunningSomewhere` property and stay in sync. Update the `run_call_end_detector_task` opening to use a local monitor:

```rust
    #[cfg(target_os = "macos")]
    let monitor = {
        // A dedicated MicCameraMonitor for the call-end detector.
        // Safe to have two instances: both listen to the same HAL property
        // and stay in sync. The detection task uses the other instance.
        use crate::meeting::mic_camera_monitor::MicCameraMonitor;
        Arc::new(MicCameraMonitor::new().unwrap_or_else(|e| {
            tracing::warn!(error = ?e, "call-end detector: MicCameraMonitor init failed");
            MicCameraMonitor::noop()  // graceful degradation if HAL unavailable
        }))
    };
```

Check whether `MicCameraMonitor::new()` returns a `Result` and whether a `noop()` constructor exists. If not, handle the error by logging and returning early from the task body (the two platform-agnostic signals still function without it).

- [ ] **Step 4: Spawn `run_call_end_detector_task` in `lib.rs`**

In `src-tauri/src/lib.rs`, in the `setup` hook alongside where `run_meeting_detection_task` is spawned, add:

```rust
    #[cfg(target_os = "macos")]
    tauri::async_runtime::spawn(
        crate::meeting::call_end_detector::run_call_end_detector_task(app.clone()),
    );
```

- [ ] **Step 5: Add TS event constants and types**

In `src/lib/events.ts`, add to the `Events` object:

```typescript
  /// Backend → HUD + main window: the call-end detector reached confidence
  /// that the meeting call has ended. Payload: `CallMayHaveEndedPayload`.
  CallMayHaveEnded: "meeting:call-may-have-ended",
  /// Backend → HUD + main window: the call-end detector reset (a reversal
  /// signal fired — e.g. mic went active again). Dismiss any prompt.
  CallEndCancelled: "meeting:call-end-cancelled",
```

In `src/lib/types.ts`, add:

```typescript
export interface CallMayHaveEndedPayload {
  confidence: "high" | "medium";
  signalSummary: string;
}
```

- [ ] **Step 6: Add Playwright mock handlers**

In `tests/e2e/_mock.ts`, add default no-op handlers for the new events (they are backend → frontend events, not IPC commands, so no mock needed — but if any test needs to emit them, document the pattern).

- [ ] **Step 7: Build and run tests**

```bash
cd src-tauri && cargo build 2>&1 | head -60
cd src-tauri && cargo test --lib meeting:: 2>&1 | tail -20
npm run check
```

Expected: clean build, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/meeting/call_end_detector.rs src-tauri/src/meeting/events.rs \
        src-tauri/src/lib.rs src/lib/events.ts src/lib/types.ts tests/e2e/_mock.ts
git commit -m "feat(meeting): add run_call_end_detector_task and call-may-have-ended events"
```

---

## Task 8: HUD `call-may-have-ended` prompt + main window banner

**Files:**
- Modify: `src/routes/hud/+page.svelte`
- Modify: `src/routes/+page.svelte`

- [ ] **Step 1: Write e2e tests**

Add to `tests/e2e/hud.spec.ts`:

```typescript
test("HUD shows high-confidence call-ended prompt", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });
  });
  await expect(page.getByText("Your call has likely ended")).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop now" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
});

test("HUD shows medium-confidence call-ended prompt with softer copy", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", {
      state: "call-may-have-ended",
      confidence: "medium",
    });
  });
  await expect(page.getByText("your call may be winding down")).toBeVisible();
});

test("HUD call-ended: Keep recording returns to recording state", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", { state: "recording", startedAtMs: Date.now() });
  });
  await page.evaluate(() => {
    window.__hush_e2e?.emit("hud:state", { state: "call-may-have-ended", confidence: "high" });
  });
  await page.getByRole("button", { name: "Keep recording" }).click();
  // Should return to recording state, not show the prompt
  await expect(page.getByText("Your call has likely ended")).not.toBeVisible();
  await expect(page.getByRole("button", { name: "Stop recording" })).toBeVisible();
});

test("HUD call-ended: Stop now calls meeting_stop_manual", async ({ page }) => {
  await page.goto("/hud");
  await injectMocks(page);
  let stopped = false;
  await page.exposeFunction("__onStopCalled", () => { stopped = true; });
  await page.evaluate(() => {
    (window as any).__hush_e2e_overrides = {
      meeting_stop_manual: async () => { (window as any).__onStopCalled(); },
    };
    window.__hush_e2e?.emit("hud:state", { state: "call-may-have-ended", confidence: "high" });
  });
  await page.getByRole("button", { name: "Stop now" }).click();
  expect(stopped).toBe(true);
});
```

- [ ] **Step 2: Run to confirm failure**

```bash
npx playwright test tests/e2e/hud.spec.ts -g "call-ended" --reporter=line
```

- [ ] **Step 3: Add `call-may-have-ended` render branch to HUD**

In `src/routes/hud/+page.svelte`, after the `{:else if hudState === "pending"}` block, add:

```svelte
  {:else if hudState === "call-may-have-ended"}
    <!--
      Call-end detection prompt. Confidence-adaptive copy:
      - "high" (two+ signals, 30s): "Your call has likely ended"
      - "medium" (one signal, 120s): "your call may be winding down"
      "Keep recording" suppresses prompting for the rest of this session;
      it does NOT disable the detector globally.
    -->
    <span class="hud-call-end-prompt" data-tauri-drag-region="false">
      <span class="hud-call-end-label">
        {callEndConfidence === "high"
          ? "Your call has likely ended"
          : "It looks like your call may be winding down"}
      </span>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--stop"
        onclick={async () => {
          try { await invoke("meeting_stop_manual"); } catch { /* best-effort */ }
          await dismiss();
        }}
      >Stop now</button>
      <button
        type="button"
        class="hud-confirm-btn hud-confirm-btn--keep"
        onclick={() => {
          // Return to recording state. The detector will not re-prompt
          // for this session (sessionCallEndSuppressed flag).
          sessionCallEndSuppressed = true;
          hudState = prevHudState ?? "recording";
        }}
      >Keep recording</button>
    </span>
```

Add state variables near the top of the script:

```typescript
  // Confidence level for the call-may-have-ended prompt.
  let callEndConfidence = $state<"high" | "medium" | null>(null);
  // HUD state before the call-end prompt, so "Keep recording" can restore it.
  let prevHudState = $state<"recording" | null>(null);
  // Suppresses repeated call-end prompts for the current session after
  // the user clicks "Keep recording". Resets when state → recording.
  let sessionCallEndSuppressed = $state(false);
```

In the `hud:state` event listener, handle `call-may-have-ended`:

```typescript
      if (payload.state === "call-may-have-ended") {
        if (sessionCallEndSuppressed) return; // user already dismissed once
        callEndConfidence = payload.confidence ?? "high";
        prevHudState = hudState === "recording" ? "recording" : null;
        hudState = "call-may-have-ended";
        return;
      }
```

When state changes to `recording`, reset suppression:

```typescript
      if (payload.state === "recording") {
        confirmingStop = false;
        sessionCallEndSuppressed = false;  // ← add this line
        // ... existing recording code
```

Also add a listener for `meeting:call-end-cancelled` to auto-dismiss the prompt:

```typescript
  // Listen for call-end detector reversal (mic went active, speech detected).
  // Auto-dismiss the call-may-have-ended prompt without stopping recording.
  const unlistenCallEndCancelled = await listen(Events.CallEndCancelled, () => {
    if (hudState === "call-may-have-ended") {
      hudState = prevHudState ?? "recording";
      callEndConfidence = null;
    }
  });
```

Add `unlistenCallEndCancelled` to the `onDestroy` cleanup.

- [ ] **Step 4: Add CSS for call-end prompt**

In `<style>`:

```css
  .hud-call-end-prompt {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .hud-call-end-label {
    font-size: 11px;
    opacity: 0.85;
    white-space: nowrap;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
```

- [ ] **Step 5: Add main window banner**

In `src/routes/+page.svelte`, add a listener for `meeting:call-may-have-ended`. When received and the HUD is not visible (user dismissed it), show a dismissible banner. This is a lightweight addition — find where other transient notices are shown (e.g. `MeetingSection.svelte` banner pattern) and follow the same pattern:

```typescript
  import { Events } from "$lib/events";
  import type { CallMayHaveEndedPayload } from "$lib/types";

  let callEndBanner = $state<CallMayHaveEndedPayload | null>(null);

  onMount(async () => {
    // ... existing listeners ...
    const unlistenCallEnd = await listen<CallMayHaveEndedPayload>(
      Events.CallMayHaveEnded,
      ({ payload }) => {
        callEndBanner = payload;
      }
    );
    const unlistenCallEndCancelled = await listen(
      Events.CallEndCancelled,
      () => { callEndBanner = null; }
    );
    onDestroy(() => {
      unlistenCallEnd();
      unlistenCallEndCancelled();
    });
  });
```

In the template, add (near the top of the main content area, after other notices):

```svelte
  {#if callEndBanner}
    <div class="call-end-banner" role="alert" transition:fly={{ y: -8, duration: 200 }}>
      <span>
        {callEndBanner.confidence === "high"
          ? "Your call has likely ended."
          : "Your call may be winding down."}
      </span>
      <button onclick={async () => {
        await invoke("meeting_stop_manual");
        callEndBanner = null;
      }}>Stop recording</button>
      <button onclick={() => { callEndBanner = null; }}>Dismiss</button>
    </div>
  {/if}
```

Add minimal CSS:

```css
  .call-end-banner {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: rgba(244, 158, 23, 0.15);
    border: 1px solid rgba(244, 158, 23, 0.3);
    border-radius: 8px;
    font-size: 13px;
  }
```

- [ ] **Step 6: Run all tests**

```bash
npx playwright test tests/e2e/hud.spec.ts --reporter=line
npm run check
cd src-tauri && cargo test --lib 2>&1 | tail -20
```

Expected: all passing.

- [ ] **Step 7: Commit**

```bash
git add src/routes/hud/+page.svelte src/routes/+page.svelte tests/e2e/hud.spec.ts
git commit -m "feat(ui): add call-may-have-ended HUD prompt and main window banner"
```

---

## Task 9: Final checks and PR

- [ ] **Step 1: Full test suite**

```bash
npm run test:unit
npm run test:e2e
cd src-tauri && cargo test --lib
cd src-tauri && cargo test --lib --features whisper
cd src-tauri && cargo test --lib --features diarization-onnx
```

- [ ] **Step 2: Type check and lint**

```bash
npm run check
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings
cd src-tauri && cargo fmt --all
```

- [ ] **Step 3: Dev launch smoke test**

```bash
npm run tauri dev
```

Verify:
- Auto-start shows 3s countdown HUD before recording
- "Don't record" cancels the countdown
- Recording HUD shows stop button
- Stop button shows inline confirmation
- Confirming stop ends recording

- [ ] **Step 4: Open PR**

```bash
git push -u origin feat/hud-meeting-lifecycle
gh pr create \
  --title "feat(hud): meeting lifecycle UX — countdown, stop button, call-end detection" \
  --body "$(cat <<'EOF'
## Summary

- **Pre-recording countdown**: auto-detection shows a 3-second HUD countdown before recording starts, with a one-shot "Don't record" cancel button
- **Stop button on HUD**: stop recording from the overlay with inline 2-button confirmation (no modal)  
- **Call-end detector**: multi-signal Rust background task (`CallEndDetector` trait + state machine) prompts when a meeting call ends while the app stays open

## Call-end detection design

Three pluggable signals (`CallEndSignal` trait) combine in a state machine that follows the existing `evaluate_mic_state` pattern:
- `MicHalSignal` (macOS): CoreAudio HAL device activity — no entitlements needed
- `SystemAudioRmsSignal`: rolling 60s RMS window on system-audio tap level
- `WhisperSilenceSignal`: consecutive empty Whisper inference ticks (platform-agnostic)

State machine: `Monitoring → Suspected → Confident` with immediate reversal on any active signal (mic goes active, audio spikes, speech detected). Emits `meeting:call-may-have-ended` with `confidence: "high" | "medium"`. Only activates for manually-started sessions (auto-started have existing AutoStop).

## Test plan

- [ ] All unit tests pass (`cargo test --lib`)
- [ ] All e2e tests pass (`npm run test:e2e`)
- [ ] `npm run check` clean
- [ ] `cargo clippy --no-default-features` clean
- [ ] Dev launch smoke: countdown, stop button, manual session end detection all verified
EOF
)"
```
