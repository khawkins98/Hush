# HUD Meeting Lifecycle UX — Design Spec

**Date:** 2026-06-15  
**Branch:** `feat/hud-meeting-lifecycle`  
**Status:** Approved for implementation

---

## Overview

Three related improvements to the meeting recording lifecycle, shipped together because they share the HUD state machine and meeting session context:

1. **Pre-recording countdown** — when auto-detection fires, show a 3-second HUD countdown before recording starts, with a cancel option
2. **Stop button on HUD** — let users stop recording directly from the overlay (currently only dismiss exists)
3. **Call-end detector** — detect when a meeting call has ended (app still open) and prompt the user to stop recording

All three touch `hud:state` event shape and the meeting session state. Shipping together avoids awkward half-states.

---

## Feature 1: Pre-Recording Countdown HUD

### Behaviour

When `run_meeting_detection_task` determines it should auto-start a meeting (currently fires `meeting_start_manual` immediately), it instead:

1. Shows the HUD in a new `pending` state with a 3-second countdown
2. If the user clicks "Don't record", cancels that auto-start instance (one-shot suppression — does not disable auto-start globally; next mic activation can trigger again)
3. If the countdown completes without cancellation, proceeds to start the meeting normally

### HUD State Change

New `hud:state` payload variant:

```typescript
// New state added to HudStatePayload in hud/+page.svelte
{ state: "pending"; endsAtMs: number }
// existing states unchanged:
{ state: "recording"; startedAtMs: number }
{ state: "processing" }
{ state: "done" }
```

### Frontend (`hud/+page.svelte`)

New render branch for `hudState === "pending"`:
- Text: "Meeting detected"
- Progress bar that drains left-to-right over 3 seconds, driven by `endsAtMs - Date.now()` on a `requestAnimationFrame` loop
- "Don't record" button (same circular style as dismiss button, with X icon and `aria-label="Cancel auto-recording"`)
- CSS animation respects `prefers-reduced-motion` (static bar, no animation)

### Backend Changes

**`src-tauri/src/hud/mod.rs`:** Add `Pending { ends_at_ms: u64 }` variant to `HudState` enum. Add `set_state_pending(app, ends_at_ms)` helper (mirrors existing `set_state`).

**`src-tauri/src/lib.rs` — `run_meeting_detection_task`:**  
In the `MicStateOutcome::Start` branch, before calling `meeting_start_manual`:
1. Emit `hud:state` with `{ state: "pending", endsAtMs: now + 3000 }`
2. Show the HUD window
3. `tokio::time::sleep(Duration::from_secs(3))`, but race it against a `CancellationToken`
4. If cancelled: hide HUD, return to monitoring (do not mark this meeting instance as permanently suppressed)
5. If not cancelled: proceed with `meeting_start_manual` as today

**`src-tauri/src/ipc/commands/meeting.rs`:** New command `meeting_cancel_pending`:
- Signals the `CancellationToken` held by the pending countdown
- Returns `Ok(())` immediately
- Registers in `tauri::generate_handler![]` as `ipc::commands::meeting::meeting_cancel_pending`

**`src/lib/types.ts`:** No new type needed — `meeting_cancel_pending` takes no args and returns `void`.

**`tests/e2e/_mock.ts`:** Add default no-op handler for `meeting_cancel_pending`.

**`CancellationToken` lifetime:** Stored in `AppState.runtime_flags` as `pending_cancel: Arc<Notify>`. Cleared (new `Notify` created) on each new pending start so stale cancels from a previous countdown are harmless.

### IPC Sync Checklist (four-place rule)

| Place | Item |
|-------|------|
| Rust handler | `meeting_cancel_pending` in `commands/meeting.rs` |
| Registration | `ipc::commands::meeting::meeting_cancel_pending` in `generate_handler!` |
| TS type | `invoke("meeting_cancel_pending")` — no payload needed |
| Playwright mock | no-op handler in `_mock.ts` |

---

## Feature 2: Stop Button on HUD

### Behaviour

In the `recording` state, the HUD gains a Stop button alongside the existing Dismiss button. Clicking Stop shows an **inline confirmation** (replaces the controls area within the HUD pill — no separate modal window). The user can confirm stop or return to recording.

### Frontend (`hud/+page.svelte`)

New local state: `let confirmingStop = $state(false)`

**Normal recording controls (unchanged):** dismiss button (X), elapsed timer, audio waveform.

**When `confirmingStop === true`:** Replace the bottom controls area with:
- Text: "Stop recording?"
- Two buttons side by side:
  - "Stop" — `onclick={() => { invoke("meeting_stop_manual"); dismiss(); }}`
  - "Keep recording" — `onclick={() => confirmingStop = false}`
- Both buttons use the existing circular button style; "Stop" uses the red/danger variant consistent with the recording dot colour

**Stop button** (visible when `confirmingStop === false`):
- Icon: square stop glyph (■), same size as dismiss X
- `aria-label="Stop recording"`
- Positioned symmetrically opposite the dismiss button (dismiss on right, stop on left — or both on right with stop first)
- `onclick={() => confirmingStop = true}`

### No Backend Changes

Uses the existing `meeting_stop_manual` IPC command. No new Rust code needed.

### Confirmation UX Note

No countdown on the confirmation — it stays open until the user acts. The risk of accidentally leaving the confirmation open and forgetting is low (the HUD is small and prominent). If the user dismisses the HUD while `confirmingStop === true`, the dismiss clears `confirmingStop` and hides the window (recording continues, same as today).

---

## Feature 3: Call-End Detector

### Problem

Users forget to stop recording after a meeting call ends. The call ends within a running app (Zoom call ends, Zoom stays open), so no process-exit signal fires. The existing HAL-based `AutoStop` handles auto-started sessions (mic releases when leaving a Zoom call). The gap is **manually-started sessions** and **auto-started sessions where a virtual audio pipeline holds the HAL device open** past call end.

### Approach

A new `CallEndDetector` module that:
- Runs as a parallel 5-second-poll background task (not merged into the interrupt-driven HAL task)
- Combines three observable signals into a state machine
- Emits a single `meeting:call-may-have-ended` event when confidence threshold is reached
- Only active for manually-started sessions (auto-started have `AutoStop`)

### Architecture

#### New file: `src-tauri/src/meeting/call_end_detector.rs`

**Signal trait:**

```rust
/// A pluggable call-end signal source.
///
/// # Platform notes
/// macOS: HAL property reads and atomic loads (no entitlements needed).
/// Linux (future): ALSA/PulseAudio device open detection via inotify on
///   /proc/<pid>/fd, or pipewire volume events — implement as a new struct
///   behind #[cfg(target_os = "linux")], no trait change needed.
/// Windows (future): IMMDeviceEnumerator + IMMNotificationClient for mic;
///   IAudioMeterInformation::GetPeakValue for system audio level.
pub trait CallEndSignal: Send + Sync {
    fn name(&self) -> &'static str;
    /// Returns current reading. Must not block — reads atomics or in-memory state only.
    fn poll(&self) -> SignalReading;
}

pub struct SignalReading {
    /// 0.0 = no evidence of call-end; 1.0 = strong evidence.
    pub strength: f32,
    /// True when this signal actively contradicts call-end (e.g. mic just became
    /// active). A reversal immediately resets the state machine to Monitoring.
    pub is_reversal: bool,
}
```

**State machine types:**

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct SignalSet {
    pub mic_inactive: bool,
    pub system_audio_quiet: bool,
    pub whisper_silent: bool,
}
impl SignalSet {
    pub fn active_count(&self) -> u8 { ... }
}

pub enum CallEndState {
    /// No suspicion. Default while session is live.
    Monitoring,
    /// One or more signals suggest call may have ended.
    /// `since`: when suspicion began (for timing thresholds).
    Suspected { since: Instant, signals: SignalSet },
    /// Suspicion confirmed — event emitted. Immediately resets to Monitoring
    /// so the detector re-arms for the next call.
    Confident { since: Instant },
}

pub enum CallEndTransition {
    Stay,
    EnterSuspected { signals: SignalSet },
    EnterConfident,
    Reset,
}
```

**Pure evaluation function:**

```rust
/// Pure function — no IO, no async, no wall clock reads.
/// `now` is injected so tests can control time deterministically.
/// Follows the evaluate_mic_state() pattern from mic_camera_monitor.rs.
pub fn evaluate_call_end_state(
    current: &CallEndState,
    inputs: &CallEndInputs,
    now: Instant,
) -> CallEndTransition
```

`CallEndInputs` carries:
- `mic_inactive: bool` — from HAL signal
- `system_audio_rms: f32` — windowed mean from system audio atomic
- `consecutive_empty_whisper_ticks: u32` — from pump atomic
- `session_is_manual: bool` — guard; returns `Stay` if false

#### Three Concrete Signals

**`MicHalSignal`** — `#[cfg(target_os = "macos")]`
- Wraps `MicCameraMonitor::is_any_device_active()`
- Returns `strength = 1.0` when mic has been inactive for ≥ `MIC_INACTIVE_SUSPECT_SECS` (default 10, env `HUSH_CE_MIC_SECS`)
- Returns `is_reversal = true` when mic becomes active (clears `inactive_since`)
- **Reliability:** High for standard apps. Low when virtual audio pipelines (Krisp, SoundSource, BlackHole aggregates) hold the HAL device open past call end. This is why a second signal or extended time is required before emitting.
- *Linux future: ALSA device open count via /proc, or PulseAudio state change notification*
- *Windows future: IMMNotificationClient::OnDeviceStateChanged*

**`SystemAudioRmsSignal`** — platform-agnostic struct, macOS source
- Reads `system_audio_level: Arc<AtomicU32>` (f32-as-bits) written by meeting pump
- Maintains a rolling window of the last `SYSTEM_AUDIO_QUIET_SECS` seconds of readings
- Returns `strength = 1.0` when windowed mean RMS < `SYSTEM_AUDIO_QUIET_THRESHOLD` (default 0.005 ≈ -46 dBFS) for ≥ `SYSTEM_AUDIO_QUIET_SECS` (default 60s, env `HUSH_CE_AUDIO_SECS`)
- Returns `is_reversal = true` when mean RMS > 3× threshold (loud audio spike = call definitely active)
- **Reliability:** Medium standalone. Long window (60s) is deliberate — browser tabs, OS notifications, and background music all produce sub-threshold bursts. Most valuable as a confirming signal when `MicHalSignal` is blocked by a virtual device.
- *Linux future: PulseAudio peak detect or pipewire volume event feeding the same atomic*
- *Windows future: IAudioMeterInformation::GetPeakValue feeding the same atomic*

**`WhisperSilenceSignal`** — fully platform-agnostic
- Reads `whisper_consecutive_empty_ticks: Arc<AtomicU32>` written by meeting pump; reset to 0 on any real utterance
- Returns `strength = 1.0` when ticks ≥ `EMPTY_WHISPER_TICKS_SUSPECT` (default 20 = ~10s at 500ms pump tick, env `HUSH_CE_WHISPER_TICKS`)
- Returns `is_reversal = true` when ticks == 0 (Whisper just transcribed real speech — strongest possible evidence call is live)
- **Reliability:** High semantic signal. VAD already gates Whisper inference, so an empty tick means actual audio energy was below `no_speech_thold`. False-positive risk: long thinking pauses mid-call. Mitigated by requiring Suspected state to hold before promoting.

#### State Machine Transitions and Timing

```
Monitoring → Suspected
  Trigger: any signal reports strength = 1.0
  Earliest real-world trigger: mic inactive for 10s (MicHalSignal threshold)

Suspected → Confident (two-signal path, high confidence)
  Condition: elapsed ≥ 30s AND active_count ≥ 2
  Example: mic inactive 10s → Suspected; 30s later audio also quiet → Confident (40s total)
  Event payload confidence: "high"

Suspected → Confident (single-signal path, medium confidence)
  Condition: elapsed ≥ 120s AND active_count ≥ 1
  Handles virtual-device scenarios where MicHalSignal never fires.
  Event payload confidence: "medium"

Suspected → Monitoring (reversal)
  Trigger: any signal reports is_reversal = true (immediate, no grace period)
  Examples: Zoom unmutes mic, YouTube notification fires, Whisper transcribes a word
  Rationale: a resuming call must cancel the prompt instantly

Confident → Monitoring
  Immediate — re-arms detector for next call after emitting event
```

Poll interval: 5 seconds. Timing is approximate to ±5s, which is irrelevant at 30-120s thresholds.

#### Integration

Spawned as a parallel task in `lib.rs` setup, alongside the existing HAL detection task:

```rust
#[cfg(target_os = "macos")]
tauri::async_runtime::spawn(run_call_end_detector_task(app.clone()));
```

The task sleeps on a `tokio::time::interval(Duration::from_secs(5))` and exits its inner loop when `session_active == false OR session_is_auto == true`. It does not replace or modify `run_meeting_detection_task`.

**Why parallel rather than merged:** `run_meeting_detection_task` is interrupt-driven (HAL `Notify`). Merging a poll loop requires `tokio::select!` and complicates control flow. Parallel keeps the two concerns (auto-start/stop vs. call-end prompt) independent and separately cancellable.

### Infrastructure Changes

#### `AppState` / `RuntimeFlags` additions

```rust
// Three new fields in RuntimeFlags (or equivalent AppState location):
pub system_audio_level: Arc<AtomicU32>,          // f32-as-bits; 0 when no tap active
pub whisper_consecutive_empty_ticks: Arc<AtomicU32>, // reset to 0 on any real utterance
pub session_is_auto: Arc<AtomicBool>,            // true when auto-started
```

#### `pump.rs` changes (per-tick)

After draining mic source and collecting Whisper output:
```rust
// Write system audio level:
ctx.system_audio_level.store(tap_session.current_level().to_bits(), Relaxed);

// Update whisper silence counter:
let has_real_speech = final_utterances.iter()
    .any(|u| !u.text.trim().is_empty() && u.text != "[BLANK_AUDIO]");
if has_real_speech {
    ctx.whisper_consecutive_empty_ticks.store(0, Relaxed);
} else {
    ctx.whisper_consecutive_empty_ticks.fetch_add(1, Relaxed);
}
```

Reset both atomics to 0 in pump cleanup path (session end).

#### `lib.rs` changes

Write `session_is_auto` flag:
- Set `true` in `MicStateOutcome::Start` branch (auto-detected start)
- Set `false` in `meeting_start_manual` IPC command
- Set `false` (clear) in `stop_meeting_and_rebuild_transcriber`

### IPC Event: `meeting:call-may-have-ended`

**Rust (`src-tauri/src/meeting/events.rs`):**

```rust
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CallMayHaveEndedPayload {
    pub confidence: &'static str,   // "high" | "medium"
    pub signal_summary: String,     // e.g. "Mic inactive 10s + system audio quiet 65s"
}
```

**TypeScript (`src/lib/types.ts`):**

```typescript
export interface CallMayHaveEndedPayload {
  confidence: "high" | "medium";
  signalSummary: string;
}
```

**Playwright mock (`tests/e2e/_mock.ts`):** Default no-op handler (same as other informational events).

### HUD Prompt (new state)

When `meeting:call-may-have-ended` fires, the HUD (if visible) transitions to a new `call-may-have-ended` render branch. If the HUD is hidden, the main window (`routes/+page.svelte`) listens for the same event and surfaces a dismissible banner.

**HUD prompt copy (adaptive by confidence):**
- `"high"`: "Your call has likely ended — stop recording?"
- `"medium"`: "It looks like your call may be winding down — stop recording?"

**Controls:** "Stop now" (calls `meeting_stop_manual`, dismisses HUD) and "Keep recording" (dismisses prompt, returns to `recording` state, does NOT re-arm detector for this session to avoid repeated prompting).

**Auto-dismiss:** If the detector resets to Monitoring (reversal signal fires — e.g. mic goes active again mid-prompt), emit a `meeting:call-end-cancelled` event. The HUD listens and returns to the `recording` state silently.

### New `hud:state` variant

```typescript
{ state: "call-may-have-ended"; confidence: "high" | "medium" }
```

### No New Cargo Dependencies

All required types (`Arc`, `AtomicU32`, `AtomicBool`, `Instant`, `VecDeque`, `Mutex`) are in `std`. No new crates needed.

---

## Platform Cfg Boundaries

```rust
// run_call_end_detector_task signal construction:
let signals: Vec<Arc<dyn CallEndSignal>> = vec![
    // macOS only: HAL device activity via CoreAudio property listener.
    // Linux future: ALSA/PulseAudio device open detection.
    // Windows future: IMMDeviceEnumerator + IMMNotificationClient.
    #[cfg(target_os = "macos")]
    Arc::new(MicHalSignal::new(Arc::clone(&monitor))),

    // Platform-agnostic: reads atomic written by CoreAudioTap (macOS) or
    // future PulseAudio/WASAPI adapter feeding the same Arc<AtomicU32>.
    Arc::new(SystemAudioRmsSignal::new(Arc::clone(&system_audio_level))),

    // Fully platform-agnostic: reads Whisper pump output counter.
    Arc::new(WhisperSilenceSignal::new(Arc::clone(&whisper_empty_ticks))),
];
// On Linux/Windows without MicHalSignal, the detector still functions with
// two signals — it just uses the slower SUSPECTED_SOLO_CONFIRM_SECS path.
```

---

## Test Surface

### Rust unit tests (`call_end_detector.rs`)

Following the `evaluate_mic_state` pattern — pure-function tests with injected `Instant` values, no `sleep`, no wall clock:

- `Monitoring → Suspected` on mic inactive for 10s
- `Suspected → Reset` on mic becoming active (reversal)
- `Suspected → Confident` after 30s with two signals active (high confidence)
- `Suspected → Confident` after 120s with one signal (virtual device / medium confidence path)
- `Confident → Monitoring` immediately (re-arm)
- `Stay` when `session_is_manual = false`
- Whisper reversal (real utterance at tick 0) resets from `Suspected`

### Frontend e2e (`tests/e2e/`)

- Pending countdown cancels and does not start meeting
- Stop button shows inline confirmation
- Confirming stop calls `meeting_stop_manual`
- "Keep recording" dismisses confirmation, recording state resumes
- `meeting:call-may-have-ended` event shows HUD prompt with correct copy per confidence level
- "Keep recording" on call-end prompt returns to recording state
- `meeting:call-end-cancelled` dismisses call-end prompt silently

---

## File Change Summary

| File | Change |
|------|--------|
| `src-tauri/src/meeting/call_end_detector.rs` | New — ~400 LOC |
| `src-tauri/src/meeting/mod.rs` | `pub mod call_end_detector`; re-export key types |
| `src-tauri/src/ipc/state.rs` | Add 3 new atomics + `pending_cancel: Arc<Notify>` to `RuntimeFlags` |
| `src-tauri/src/meeting/pump.rs` | Write system audio level + whisper silence counter per tick; reset on cleanup |
| `src-tauri/src/lib.rs` | Spawn detector task; write `session_is_auto`; pending countdown with `CancellationToken` |
| `src-tauri/src/hud/mod.rs` | Add `Pending` + `CallMayHaveEnded` HUD state variants |
| `src-tauri/src/meeting/events.rs` | Add `emit_call_may_have_ended`, `emit_call_end_cancelled` |
| `src-tauri/src/ipc/commands/meeting.rs` | New `meeting_cancel_pending` command |
| `src/lib/types.ts` | Add `CallMayHaveEndedPayload` |
| `src/lib/events.ts` | Add `CallMayHaveEnded`, `CallEndCancelled` event constants |
| `src/routes/hud/+page.svelte` | Add `pending`, `call-may-have-ended` render branches; stop button + inline confirmation |
| `src/routes/+page.svelte` | Listen for `call-may-have-ended`; show banner if HUD not visible |
| `tests/e2e/_mock.ts` | Add handlers for new IPC command and events |
