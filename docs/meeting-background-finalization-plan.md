# Meeting Background Finalization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make meeting "Stop" release the microphone and return in well under a second, then finalize transcription (whisper `finish()` + diarization + speaker-identity + DB close) in the background — so dictation can start immediately during finalization.

**Architecture:** Reorder the meeting pump's cancel path to explicitly `stop()` each audio handle (an already-ack-waited call that returns the drained tail audio) and signal an "audio released" checkpoint *before* the slow tail work; `stop_manual` awaits only that checkpoint, parks the pump's continuation in a single-lane `finalizing` slot, and returns. The work that currently runs *after* the pump join in `stop_manual` (speaker-identity #667, `close_session`, `MeetingSessionEnded`) moves into the background pump continuation. A new *meeting* start awaits any in-flight finalization (they'd share the diarizer + meeting `WhisperContext`); dictation does not (separate slots, no diarizer). Concurrent meetings are explicitly deferred — see the design proposal's "Deferred" section.

**Tech Stack:** Rust (Tauri 2 backend, `tokio`), Svelte 5 / SvelteKit frontend, `cargo test --lib`, `vitest`, Playwright.

**Spec:** `docs/meeting-background-finalization-proposal.md` (read it first; this plan implements the "v1" scope).

**Branch:** `feat/meeting-background-finalization` (already created; the design proposal is committed there).

---

## Pre-flight

- [ ] **Confirm the branch + clean tree**

Run: `git -C /Users/khawkins/Documents/git/Hush rev-parse --abbrev-ref HEAD && git status --porcelain`
Expected: `feat/meeting-background-finalization` and an empty status (the two doc commits are already in).

- [ ] **Confirm the baseline builds + tests pass**

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -5`
Expected: PASS (establishes the green baseline before changes). If the Swift-dylib error from CLAUDE.md appears, prefix with the `DYLD_FALLBACK_LIBRARY_PATH=…` workaround.

---

## File map (what changes, and why)

**Backend (`src-tauri/src/`):**
- `meeting/events.rs` — add `MeetingFinalizing` payload + emit helper + event-name const.
- `meeting/manager.rs` — `SessionState::Stopping` → `Releasing` (narrowed); add `finalizing: Mutex<Option<JoinHandle<()>>>`; update `Drop`, `active_session_id`, the #492/#839 tests.
- `meeting/pump.rs` — reorder `run_pump` cancel path: explicit `handle.stop()` capturing tail audio → signal audio-released checkpoint → tail flush → (moved-in) #667 + `close_session` + emit ended. `PumpContext` gains the deps that move in.
- `meeting/lifecycle.rs` — `start_manual` awaits `finalizing` for a *meeting* start; `stop_manual` awaits only the audio-released checkpoint, then parks the continuation; the post-join block (#667 resolve, `close_session`, emit ended) is removed (moved to the pump).
- `ipc/commands/meeting.rs` — `stop_meeting_and_rebuild_transcriber` no longer needs to wait for full finalization; verify the transcriber-rebuild spawn still composes.

**Frontend (`src/`):**
- `lib/events.ts` — add `MeetingFinalizing`.
- `lib/state/meeting-sessions.svelte.ts` — add `finalizingId`; clear `activeId` + set `finalizingId` on `MeetingFinalizing`; clear `finalizingId` on `MeetingSessionEnded`.
- `lib/state/dictation.svelte.ts` — add a `finalizing` derived flag for the centralized status.
- `lib/AppLifecycle.svelte` — PTT guard already keys off `meeting.activeId` (now cleared at finalize-start), so dictation unblocks automatically; add the `MeetingFinalizing` listener wiring.
- `routes/+page.svelte` / meeting panel component — show a subtle "finishing transcription…" line while `finalizingId` is set.

**Tests:**
- `src-tauri/src/meeting/manager.rs` (tests), `src-tauri/src/ipc/tests.rs`, `tests/e2e/_mock.ts`, a `vitest` spec for the status reducer.

---

## Task 1: Add the `MeetingFinalizing` event (backend + TS)

Smallest, isolated, no behavior change yet — just the new event surface.

**Files:**
- Modify: `src-tauri/src/meeting/events.rs`
- Modify: `src/lib/events.ts`
- Test: `src-tauri/src/meeting/events.rs` (inline `#[cfg(test)]` if present; otherwise assert via the emit call in Task 4's integration test)

- [ ] **Step 1: Read the existing ended-event emitter to copy its shape**

Run: `sed -n '143,160p' src-tauri/src/meeting/events.rs`
Expected: the `MeetingSessionEndedPayload` struct, its `MEETING_SESSION_ENDED_EVENT` const, and `emit_meeting_session_ended(...)`. Mirror this exactly for finalizing.

- [ ] **Step 2: Add the payload, const, and emit helper**

In `src-tauri/src/meeting/events.rs`, alongside the ended-event block, add:

```rust
/// Payload emitted when the meeting pump has released the audio device and
/// begun background finalization (tail flush + diarization + persistence).
/// The frontend uses this to clear `activeId` (unblocking dictation) and show
/// a "finishing transcription…" indicator. Payload: `{ sessionId: number }`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingFinalizingPayload {
    pub session_id: i64,
}

/// Tauri event name for [`MeetingFinalizingPayload`]. Matches
/// `Events.MeetingFinalizing` in `src/lib/events.ts`.
pub(super) const MEETING_FINALIZING_EVENT: &str = "meeting:finalizing";

pub(super) fn emit_meeting_finalizing(
    emitter: &dyn crate::events::EventEmitter,
    session_id: i64,
) {
    emitter.emit(
        MEETING_FINALIZING_EVENT,
        &MeetingFinalizingPayload { session_id },
    );
}
```

> Match the exact `emit` signature used by `emit_meeting_session_ended` (it may take `&dyn EventEmitter` or a typed emitter — copy whatever that function does, including any `serde_json` serialization helper).

- [ ] **Step 3: Add the TS event name**

In `src/lib/events.ts`, inside the `Events` object after `MeetingSessionEnded`, add:

```ts
  /// Backend → frontend (main): the meeting pump released the audio device and
  /// began background finalization. Payload is `{ sessionId: number }`. The
  /// main window clears `meeting.activeId` (unblocking dictation/PTT) and sets
  /// `meeting.finalizingId` to drive a "finishing transcription…" indicator.
  /// `MeetingSessionEnded` later clears `finalizingId` when the tail is done.
  MeetingFinalizing: "meeting:finalizing",
```

- [ ] **Step 4: Compile**

Run: `cd src-tauri && cargo build --lib --features whisper,diarization-onnx 2>&1 | tail -5`
Expected: builds (the new fn is `dead_code` until Task 2 wires it — add `#[allow(dead_code)]` on the fn/const/struct temporarily, removed in Task 2, or accept the warning if `-D warnings` is not set for `build`).
Run: `npm run check 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/meeting/events.rs src/lib/events.ts
git commit -m "feat(meeting): add meeting:finalizing event surface"
```

---

## Task 2: Release-then-finalize — explicit ack-waited audio stop + checkpoint

Reorder `run_pump`'s cancel path so the audio handles are explicitly `stop()`ed (capturing the tail) *before* the slow flush, and signal an "audio released" checkpoint that `stop_manual` (Task 4) will await.

**Key facts (verified):**
- `AudioSession::stop(self: Box<Self>) -> Result<CapturedAudio>` (`audio/mod.rs:359`) round-trips `Cmd::Stop` and **waits for the reply** (`audio/cpal.rs:438-445`), returning the final drained buffer. This is the ack-waited release the spec requires — `Drop` (fire-and-forget, discards audio, `cpal.rs:462`) is what we must avoid relying on.
- `run_pump` currently: tick loop → final drain + inference (`pump.rs:303-323`) → `flush_sessions` (`pump.rs:327`) → clear partials → summary logs (`pump.rs:340-361`). Audio handles live in `ctx.handles: Vec<Option<Box<dyn AudioSession>>>` and are dropped only when `ctx` drops at task end.

**Files:**
- Modify: `src-tauri/src/meeting/pump.rs` (`PumpContext`, `run_pump`)
- Test: `src-tauri/src/meeting/pump.rs` (inline test) or `src-tauri/src/ipc/tests.rs`

- [ ] **Step 1: Read the exact cancel-path region to edit**

Run: `sed -n '300,362p' src-tauri/src/meeting/pump.rs` and `sed -n '104,175p' src-tauri/src/meeting/pump.rs`
Expected: the post-loop tail block and the `PumpContext` struct field list. Confirm the `handles` field name/type and how `tick_drain_sources` reads them.

- [ ] **Step 2: Add the audio-released signal to `PumpContext`**

Add a field to `PumpContext`:

```rust
    /// Fired exactly once, by `run_pump`, the moment all audio handles have
    /// been explicitly stopped (device released) and before the slow tail
    /// flush begins. `stop_manual` awaits this so it can return promptly while
    /// the pump task continues finalizing in the background (#meeting-bg-final).
    pub audio_released_tx: Option<tokio::sync::oneshot::Sender<()>>,
```

(Built in `lifecycle.rs::start_manual` where the rest of `PumpContext` is constructed — Task 4 wires the receiver into `ActiveSession`.)

- [ ] **Step 3: Write the failing test — stop captures tail then signals before flush**

Add to `pump.rs` tests (use the existing pump test scaffolding / mocks; if `run_pump` is hard to drive directly, assert the ordering via a mock `AudioSession` whose `stop()` records it was called, and a `oneshot` receiver that resolves before a slow mock `finish`):

```rust
#[tokio::test]
async fn run_pump_releases_audio_and_signals_before_tail_flush() {
    // A mock AudioSession::stop() that records the call order, and a streaming
    // session whose finish() blocks on a barrier. Assert: audio_released_tx
    // fires (receiver resolves) while finish() is still blocked.
    // (Construct PumpContext via the existing pump test helper.)
    // ... see meeting::test_support for the available builders ...
}
```

> If `run_pump` has no existing unit harness, defer this assertion to the IPC integration test in Task 6 (`meeting_stop_manual` returns before a slow `finish` completes) and make this step a `// covered by ipc::tests::stop_returns_before_finish` note. Do not leave a placeholder test that always passes.

- [ ] **Step 4: Implement the reorder in `run_pump`**

Replace the post-loop sequence so it is: (a) final drain + inference (unchanged, `pump.rs:311-323`); (b) **explicitly stop each handle, capturing tail audio**; (c) **fire `audio_released_tx`**; (d) `flush_sessions` + dispatch (unchanged); (e) the moved-in finalization from Task 3.

For (b)+(c), after the final `tick_inference`:

```rust
    // Release the audio device explicitly (ack-waited) so the cpal/SCK
    // singleton is freed *now*, not when this task eventually drops. Feed the
    // returned tail samples into the matching streaming session so the final
    // `finish()` still sees them (no tail-loss across the drain→stop gap).
    for i in 0..ctx.handles.len() {
        if let Some(handle) = ctx.handles[i].take() {
            match handle.stop() {
                Ok(captured) => {
                    if !captured.samples.is_empty() {
                        if let Some(session) = ctx.streaming_sessions[i].as_mut() {
                            // feed via the same path tick_inference uses; see
                            // how tick_inference calls session.feed/infer and
                            // mirror it for `captured.samples` + captured format.
                        }
                    }
                }
                Err(e) => tracing::warn!(error = ?e, source_index = i, "meeting pump: audio stop on release failed"),
            }
        }
    }
    if let Some(tx) = ctx.audio_released_tx.take() {
        let _ = tx.send(()); // receiver may have dropped if stop_manual gave up
    }
```

> Confirm the exact feed mechanism by reading how `tick_inference` pushes drained samples into `streaming_sessions[i]` (it converts via `CaptureFormat`); reuse that helper rather than re-implementing resampling. If feeding post-stop samples into the streaming window is awkward, the acceptable fallback is to run one extra `tick_inference` on `captured.samples` *before* stopping — but prefer feeding after stop to keep the release as early as possible.

- [ ] **Step 5: Build + run the pump/ipc test**

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx meeting::pump 2>&1 | tail -15`
Expected: PASS (or the deferred-to-Task-6 note holds and the module still compiles + existing pump tests pass).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/meeting/pump.rs
git commit -m "feat(meeting): release audio device explicitly before tail flush"
```

---

## Task 3: Move finalization tail-work (#667 + close + ended) into the pump

The work currently after the pump-join in `stop_manual` must run *after* the tail flush, in the background. Move it into `run_pump` (or a helper `run_pump` calls at the end).

**Verified call site to move (`meeting/lifecycle.rs:622-639`):**
```rust
        // Cross-session speaker identity resolution (#667).
        // ... gated on self.speaker_identity_enabled ...
        let centroids = self.diarize.session_centroids();
        // ...
        resolve_speaker_identities(store, session_id, centroids).await;
```
plus the `repo.close_session(session_id)` call and `emit_meeting_session_ended`.

**Files:**
- Modify: `src-tauri/src/meeting/pump.rs` (`PumpContext` gains `repo` already present; add `speaker_store`, `speaker_identity_enabled`, `diarize` already present; `event_emitter` already present)
- Modify: `src-tauri/src/meeting/lifecycle.rs` (remove the moved block; `resolve_speaker_identities` becomes callable from the pump — make it `pub(super)`)

- [ ] **Step 1: Read the full stop_manual tail + resolve_speaker_identities**

Run: `sed -n '611,720p' src-tauri/src/meeting/lifecycle.rs`
Expected: the #667 block, `close_session`, `emit_meeting_session_ended`, and the `resolve_speaker_identities` fn signature (`lifecycle.rs:718`).

- [ ] **Step 2: Make `resolve_speaker_identities` reachable from the pump**

Change its visibility to `pub(super) async fn resolve_speaker_identities(...)` so `pump.rs` can call it. Add to `PumpContext` the fields it needs (mirror types from `SessionManager`): `speaker_store: Arc<dyn crate::speakers::SpeakerStore>` and `speaker_identity_enabled: Arc<std::sync::atomic::AtomicBool>`.

- [ ] **Step 3: Write the failing test — finalization persists tail, resolves identity, closes session, emits ended (in the background)**

Add to `src-tauri/src/ipc/tests.rs` (or `meeting` tests) using `AppStateBuilder` + a `MemHistory`/repo and a `RecordingEventEmitter`:

```rust
#[tokio::test]
async fn meeting_finalization_closes_session_and_emits_ended_in_background() {
    // Start a meeting via the manager, stop it. Assert:
    //  - close_session was called (row has ended_at) AFTER stop_manual returned
    //  - MeetingSessionEnded emitted with the right id
    //  - tail finals (if any) persisted
    // Use a slow mock finish() to prove the ordering is background, not inline.
}
```

- [ ] **Step 4: Implement the move**

At the end of `run_pump`, after the tail flush + partial clear + summary logs, add (adapting the moved code to read from `ctx` instead of `self`):

```rust
    // Background finalization tail (moved from stop_manual, #meeting-bg-final):
    // speaker-identity resolution reads THIS session's diarizer centroids —
    // safe because a new meeting start awaits this finalization (manager
    // `finalizing` gate), so nothing has reset the diarizer yet.
    if ctx.speaker_identity_enabled.load(std::sync::atomic::Ordering::Acquire) {
        let centroids = ctx.diarize.session_centroids();
        if !centroids.is_empty() {
            crate::meeting::lifecycle::resolve_speaker_identities(
                Arc::clone(&ctx.speaker_store),
                ctx.session_id,
                centroids,
            )
            .await;
        }
    }
    if let Err(e) = ctx.repo.close_session(ctx.session_id).await {
        tracing::warn!(error = ?e, session_id = ctx.session_id,
            "meeting finalization: close_session failed; orphan reconcile will close it next launch");
    }
    crate::meeting::events::emit_meeting_session_ended(ctx.event_emitter.as_ref(), ctx.session_id);
```

> Match the exact `centroids` type and the `resolve_speaker_identities` argument order from Step 1. Match the exact `emit_meeting_session_ended` signature.

- [ ] **Step 5: Remove the now-duplicated block from `stop_manual`**

Delete the #667 resolve + `close_session` + `emit_meeting_session_ended` from `stop_manual` (kept there only for the non-`close_attempted` first-try path). The `close_attempted` retry machinery is removed in Task 4. Leave `stop_manual` compiling; it will be finished in Task 4.

- [ ] **Step 6: Build**

Run: `cd src-tauri && cargo build --lib --features whisper,diarization-onnx 2>&1 | tail -10`
Expected: compiles (there may be `stop_manual` shape churn finished in Task 4 — if `stop_manual` is temporarily inconsistent, complete Task 4 before running tests).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/meeting/pump.rs src-tauri/src/meeting/lifecycle.rs
git commit -m "refactor(meeting): move close + speaker-identity into background finalization"
```

---

## Task 4: State machine — `Releasing`, single `finalizing` lane, and the gates

**Files:**
- Modify: `src-tauri/src/meeting/manager.rs` (`SessionState`, `SessionManager` field, `active_session_id`, `Drop`)
- Modify: `src-tauri/src/meeting/lifecycle.rs` (`start_manual` await-finalizing gate; `stop_manual` rewrite)

- [ ] **Step 1: Rename `Stopping` → `Releasing` and add the field**

In `manager.rs`: rename the `SessionState::Stopping` variant to `Releasing` (update its doc comment to say it covers only the brief foreground audio-release window). Add to `SessionManager`:

```rust
    /// Single in-flight background finalization (whisper tail flush + diarize +
    /// persistence) from the most recent stop. At most one because a new
    /// meeting start awaits this before opening (it would share the diarizer +
    /// meeting WhisperContext). Cleared when the task completes or on Drop.
    /// Concurrent meetings are deferred — see the design proposal.
    pub(super) finalizing: Mutex<Option<tokio::task::JoinHandle<()>>>,
```

Initialize `finalizing: Mutex::new(None)` in `new()`. Update every `match` on `SessionState` (in `active_session_id`, `Drop`, `start_manual`, and the tests) to use `Releasing` instead of `Stopping`.

- [ ] **Step 2: Write the failing test — new meeting awaits finalization; dictation does not**

In `manager.rs` tests:

```rust
#[tokio::test]
async fn start_manual_meeting_awaits_in_flight_finalization() {
    // Start meeting A, stop it (a slow mock finish keeps finalization in flight).
    // Immediately start meeting B: it must block until A's finalization completes,
    // then succeed (not error with "finishing; wait").
}
```

(Dictation-does-not-wait is asserted in the IPC test in Task 6, since dictation start lives in the ipc layer.)

- [ ] **Step 3: Rewrite `stop_manual` to await only the audio-released checkpoint**

Replace the pump-join + post-join block with: signal cancel, **await the `audio_released` oneshot** (with a generous timeout fallback), flip `Releasing → Idle`, and store the pump `JoinHandle` in `finalizing`:

```rust
        active.cancel.store(true, Ordering::Release);
        // Await only until the pump has released the audio device, not until the
        // whole tail flush completes (#meeting-bg-final).
        if let Some(rx) = active.audio_released_rx.lock().unwrap().take() {
            // generous bound; the pump fires this right after handle.stop()
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await;
        }
        // Park the pump continuation as the single background finalization.
        if let Some(handle) = active.pump_handle.lock().unwrap().take() {
            if let Ok(mut slot) = self.finalizing.lock() {
                // If a prior finalization handle is still parked, drop it — it has
                // already completed (the await-finalizing gate guarantees only one
                // meeting at a time). Abort defensively in case of a stuck task.
                if let Some(old) = slot.replace(handle) {
                    old.abort();
                }
            }
        }
        {
            let mut guard = self.state.lock().map_err(|_| anyhow!("session manager mutex poisoned"))?;
            if matches!(&*guard, SessionState::Releasing) {
                *guard = SessionState::Idle;
            }
        }
        return Ok(());
```

> `ActiveSession` gains `audio_released_rx: Mutex<Option<oneshot::Receiver<()>>>` (set in `start_manual` from the same `oneshot` whose `Sender` went into `PumpContext`). Remove `close_attempted` and the `close_session`-failure restore block (close is now in the pump). Update the `#492`/`#839` tests accordingly (Task 6).

- [ ] **Step 4: Add the await-finalizing gate to `start_manual` (meeting only)**

At the top of `start_manual`, before claiming the slot, await any in-flight finalization so the new meeting doesn't share the diarizer/context:

```rust
        // A new meeting must wait for any in-flight background finalization to
        // complete — it would otherwise share the diarizer cluster state and the
        // meeting WhisperContext with the finalizing session (#meeting-bg-final).
        let pending = self.finalizing.lock().ok().and_then(|mut g| g.take());
        if let Some(handle) = pending {
            let _ = handle.await; // normally sub-second; see proposal "Deferred"
        }
```

> This runs in `start_manual` (meeting path) only. The dictation start path (`ipc/commands/dictation/`) must NOT call this — it uses a separate transcribe slot and no diarizer, so it proceeds as soon as `active_session_id()` is `None` (which it is once `Releasing → Idle`).

- [ ] **Step 5: Update `Drop` to abort the finalization too**

In `SessionManager::Drop`, after handling the active/releasing slot, also abort any parked finalization (abort-and-reconcile per the spec — `finish()` in `spawn_blocking` can't be cancelled, so do not block shutdown joining it):

```rust
        if let Ok(mut slot) = self.finalizing.lock() {
            if let Some(handle) = slot.take() {
                handle.abort(); // orphan row, if any, closed by reconcile next launch
            }
        }
```

- [ ] **Step 6: Build + run manager tests**

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx meeting::manager 2>&1 | tail -20`
Expected: the new gate test PASSES; adapt any `Stopping`/`close_attempted` references the compiler flags (finished in Task 6's test pass).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/meeting/manager.rs src-tauri/src/meeting/lifecycle.rs
git commit -m "feat(meeting): single-lane background finalization + meeting-start await gate"
```

---

## Task 5: Frontend — finalizing status + indicator (dictation unblocks automatically)

**Files:**
- Modify: `src/lib/state/meeting-sessions.svelte.ts` (add `finalizingId`; listeners)
- Modify: `src/lib/state/dictation.svelte.ts` (add `finalizing` derived)
- Modify: the meeting panel component (subtle "finishing…" line)

- [ ] **Step 1: Write the failing vitest for the status reducer**

Create `src/lib/state/recording-status.test.ts` (pure-logic; if the state module isn't unit-testable in isolation, test a small extracted reducer). Example shape:

```ts
import { describe, it, expect } from "vitest";
// import or replicate the reducer that maps events → { activeId, finalizingId }
describe("meeting finalize reducer", () => {
  it("clears activeId and sets finalizingId on MeetingFinalizing", () => {
    let s = { activeId: 7, finalizingId: null as number | null };
    s = onFinalizing(s, 7);
    expect(s.activeId).toBeNull();
    expect(s.finalizingId).toBe(7);
  });
  it("clears finalizingId on matching MeetingSessionEnded", () => {
    let s = { activeId: null, finalizingId: 7 };
    s = onEnded(s, 7);
    expect(s.finalizingId).toBeNull();
  });
});
```

Run: `npm run test:unit -- recording-status 2>&1 | tail -10`
Expected: FAIL (reducer not defined).

- [ ] **Step 2: Add `finalizingId` state + the `MeetingFinalizing` listener**

In `meeting-sessions.svelte.ts`: add a `meetingFinalizingId` ($state, default null) with a getter on the `meeting` object. In `initSessionListeners`, add a listener that **clears `activeId` and sets `finalizingId`**:

```ts
    const unlistenFinalizing = await listen<{ sessionId: number }>(
      Events.MeetingFinalizing,
      (e) => {
        // Audio is released; the tail is processing in the background. Clear
        // activeId so PTT/dictation unblocks, and flag finalizing for the UI.
        if (meeting.activeId === e.payload.sessionId) meeting.activeId = null;
        meeting.finalizingId = e.payload.sessionId;
        void meeting.refresh();
      },
    );
```

In the existing `MeetingSessionEnded` listener, also clear finalizing:

```ts
        if (meeting.finalizingId === e.payload.sessionId) meeting.finalizingId = null;
```

Add `unlistenFinalizing()` to the returned cleanup. Extract the `onFinalizing`/`onEnded` pure logic so Step 1's test imports it (DRY).

- [ ] **Step 3: Add the `finalizing` derived to the centralized status**

In `dictation.svelte.ts`, near the existing derivations (`:99-105`):

```ts
let finalizing = $derived(meeting.finalizingId !== null);
```

Expose it on the `dictation` object (getter, like `meetingOnlyActive`). `anyRecordingActive` stays `recording || meetingOnlyActive` — finalizing is NOT "recording" (mic is released), so it must not re-trigger the recording chrome.

- [ ] **Step 4: Run the vitest**

Run: `npm run test:unit -- recording-status 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Render the subtle indicator**

In the meeting panel area (the component that renders the active session / Stop button — find it via `rg "finishing|activeDetail|meeting.activeId" src/lib`), add, gated on `dictation.finalizing`:

```svelte
{#if dictation.finalizing}
  <p class="finishing-note">Finishing transcription…</p>
{/if}
```

with a muted style. Keep it minimal.

- [ ] **Step 6: Verify PTT-unblock + type-check**

Confirm `AppLifecycle.svelte:212` PTT guard (`meeting.activeId !== null`) now permits dictation during finalization (because `activeId` clears on `MeetingFinalizing`). No code change needed there unless the guard also checks `finalizing` — it must NOT.
Run: `npm run check 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/lib/state/meeting-sessions.svelte.ts src/lib/state/dictation.svelte.ts src/lib/state/recording-status.test.ts src/lib/<meeting panel component>
git commit -m "feat(ui): finalizing status + dictation-during-finalize"
```

---

## Task 6: Tests — adapt #492/#839, IPC round-trips, Playwright mock

**Files:**
- Modify: `src-tauri/src/meeting/manager.rs` (tests), `src-tauri/src/ipc/tests.rs`
- Modify: `tests/e2e/_mock.ts`

- [ ] **Step 1: Adapt the #492/#839 tests**

`stop_manual_close_failure_restores_session_for_retry_when_idle` and `stop_manual_close_failure_does_not_clobber_concurrent_start` assert the `close_attempted` retry that no longer exists (close is in the background). Replace them with tests asserting the new contract: a background `close_session` failure logs and leaves the row open (reconcile-next-launch), and `stop_manual` itself returns `Ok` once audio is released. Update the `SessionState::Stopping` arms to `Releasing`.

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx meeting:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 2: IPC integration — stop returns before finish; dictation works during finalize**

In `src-tauri/src/ipc/tests.rs`, add (using `AppStateBuilder` + a transcriber mock whose streaming `finish()` blocks on a barrier you release manually):

```rust
#[tokio::test]
async fn meeting_stop_returns_before_finish_and_allows_dictation() {
    // 1. start a meeting; 2. call the stop path; assert it returns while finish() is still blocked
    // 3. assert a dictation start succeeds while finalization is in flight
    // 4. release the barrier; assert MeetingSessionEnded emitted + session closed
}
```

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx ipc:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3: Playwright mock for the new event**

In `tests/e2e/_mock.ts`, ensure `meeting_stop_manual` resolves promptly and that the harness can emit `meeting:finalizing` then `meeting:session-ended`. Add/adjust a spec asserting the panel shows "Finishing transcription…" then clears.

Run: `npx playwright test tests/e2e/meeting-panel.spec.ts 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 4: Full suites**

Run: `cd src-tauri && cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -5`
Run: `cd src-tauri && cargo clippy --lib --no-default-features -- -D warnings 2>&1 | tail -5` (cross-platform lint per CLAUDE.md)
Run: `npm run check && npm run test:unit 2>&1 | tail -5`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(meeting): cover background finalization + adapt #492/#839"
```

---

## Task 7: Dev-launch smoke + docs

- [ ] **Step 1: Real-runtime smoke (required — touches AppState + meeting state machine)**

Run: `npm run tauri dev` — start a meeting (or trigger auto-detect), press Stop. Verify: Stop resolves within ~1s, the waveform stops, and a push-to-talk dictation works immediately while the meeting transcript's tail is still settling. Then confirm the tail utterances appear and the session shows ended.

> CI cannot catch a boot panic here (CLAUDE.md "Dev-launch smoke"). Do this before opening the PR.

- [ ] **Step 2: Update `ARCHITECTURE.md`**

Update the meeting-pump dataflow section to describe release-then-finalize, the single `finalizing` lane, and the "new meeting awaits finalization" gate.

- [ ] **Step 3: Add a `learnings.md` entry**

Add a dated entry: why meeting finalization was backgrounded; the `AudioSession::stop()` ack-waited-vs-`Drop` distinction; and the deferred-concurrency constraint (shared `Arc<Mutex<WhisperContext>>` at `whisper.rs:460` freezes a concurrent live meeting behind a finalizing one) with a pointer to the proposal's "Deferred" section + issue #974.

- [ ] **Step 4: Code comments for future pickup**

Add comments at: the `finalizing` field + the await-gate in `lifecycle.rs` (pointing at the proposal "Deferred" section), and the shared-`WhisperContext` clone (`whisper.rs:460`) noting it's the load-bearing constraint for concurrent meetings.

- [ ] **Step 5: Mark the proposal accepted**

Edit `docs/meeting-background-finalization-proposal.md` status line to "Accepted; implemented in <PR#>".

- [ ] **Step 6: Commit + open PR**

```bash
git add -A && git commit -m "docs(meeting): architecture + learnings for background finalization"
git push -u origin feat/meeting-background-finalization
gh pr create --fill
```

---

## Self-review notes (author)

- **Spec coverage:** Goal 1 (release + bg finalize) → Tasks 2–4. Goal 2 (dictation during finalize) → Task 4 Step 4 + Task 5. Goal 3 (subtle UX) → Task 5. Goal 4 (centralized frontend status) → Task 5. Must-fix items: ack-waited stop → Task 2; tail-loss → Task 2 Step 4; abort-and-reconcile shutdown → Task 4 Step 5; close/#667 to background → Task 3; #492/#839 change → Task 6 Step 1. Deferred section → Task 7 Steps 3–4.
- **Known soft spots (finalize against the compiler during TDD):** the exact sample-feed mechanism in Task 2 Step 4 (mirror `tick_inference`), and the precise `resolve_speaker_identities` signature/centroid type in Task 3 (read at Step 1). Both are anchored to verified call sites; neither is a placeholder.
- **HUD finalizing visual:** intentionally scoped to the in-app panel indicator (Task 5 Step 5). A HUD shimmer for finalizing would require reworking the HUD visibility contract (driven by `UiRecordingState`, which is false once audio is released) — deferred to avoid touching the transparent-window contract for a "subtle" cue.
