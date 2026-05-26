# Meeting background finalization — design proposal

**Status:** Draft for review · **Date:** 2026-05-26 · **Author:** Ken + Claude

## Problem

When a meeting recording stops, the "Stop" IPC does not return until the meeting
pump has fully wound down — and wind-down includes a final whisper inference pass
("tail flush") that can take **up to 60 s per audio source, run sequentially**
(mic + system-audio → worst case ~120 s).

Root cause, verified against the code:

- `meeting_stop_manual` → `stop_meeting_and_rebuild_transcriber` →
  `MeetingManager::stop_manual()` **awaits the pump task to completion**
  (`meeting/lifecycle.rs:528`, `handle.await`).
- On cancel, `run_pump` does a final drain + inference, then `flush_sessions`
  calls `session.finish()` per source (`meeting/pump.rs:947–957`), each wrapped in
  `tokio::time::timeout(STREAMING_FINISH_TIMEOUT = 60s, spawn_blocking(finish))`
  (`meeting/pump.rs:83`, `953`). The loop is **sequential**.
- `finish()` is a full whisper.cpp inference on the streaming window
  (`transcription/whisper.rs:597`). Under #612 memory pressure (observed RSS
  ~1.9 GB with periodic `WhisperState` recreation, `hush.log.2026-05-26` lines
  201–202), a single inference can balloon from ~1 s to tens of seconds.

Because the pump still **owns the live audio capture handles** the entire time it
is blocked in `finish()`, three symptoms follow from this one cause:

| Symptom | Mechanism |
| --- | --- |
| Stop takes ~a minute to "release" | IPC awaits the pump, blocked in `finish()` |
| Can't start any other recording | cpal worker is a singleton; a new `Cmd::Start` returns `"recording already in progress"` while the meeting's `AudioSession` handle is alive (`audio/cpal.rs:524`). Observed in the log, lines 88–112. |
| Mic waveform still animates | The mic `Stream` is never stopped during `finish()`; the audio callback keeps writing samples and the level meter keeps reading them. |

The two stops captured in the supplied log (sessions 24 and 25) were *fast*
(sub-second tail flush), confirming this is an **intermittent** slow path
(memory-pressure-dependent), not the steady state.

## Goals

1. On stop, **release the audio device immediately** (frees the singleton, stops
   the waveform), then run tail transcription/diarization/persistence in the
   **background**.
2. Allow the user to **start a new recording during finalization** — dictation
   *and* a new meeting (full concurrency), including **multiple concurrent
   background finalizations** (a long call whose tail takes a while must not block
   the next session).
3. **Subtle, non-blocking UX**: a quiet "finishing transcription…" indicator on
   the meeting panel and HUD; new recordings just work, no warning needed.
4. **Centralize** the "what is recording / finalizing" status so the meeting
   panel, HUD, document title, tray, and the push-to-talk guard all read one
   source of truth instead of today's scattered `meeting.activeId !== null`
   checks. (Scoped to what this feature touches — not an unrelated rewrite.)

## Non-goals

- Reducing whisper inference cost itself / fixing #612 memory growth. Out of
  scope; finalization simply no longer blocks the user when it is slow.
- Parallelizing whisper across concurrent sessions (they may share the meeting
  `WhisperContext` and serialize on its mutex — see Risks).
- Changing dictation's internal phase machine beyond what the new `finalizing`
  status requires.

## Approved decisions

- **Concurrency:** full — a new meeting or dictation may start while previous
  meetings finalize; N concurrent finalizations.
- **UX:** subtle "finishing…" indicator, non-blocking.

## Architecture

### A. Per-session diarizer isolation (correctness prerequisite)

The diarizer is a shared singleton with mutable per-instance cluster state
(`diarization/onnx.rs`: `clusters: Mutex<SessionClusterState>`), reset at pump
*start* (#794). Today only one pump is ever live, so this is safe. With background
finalization + full concurrency, two pumps can touch the one diarizer at once:

- a new meeting's `diarize.reset()` would wipe a finalizing meeting's clusters;
- two finalizations' tail-diarization would interleave cluster IDs.

`OnnxDiarizer` already separates the **immutable, shareable model**
(`TypedRunnableModel`, `Send + Sync`, no lock) from the **cheap per-session
`clusters`**. The design gives each session its own cluster state while sharing
the one loaded model.

**Trait change.** Introduce a session-scoped diarizer:

```rust
/// Heavy, shared, immutable — loaded once.
trait DiarizerModel: Send + Sync {
    /// Start a fresh, independent diarization scope for one session.
    fn new_session(&self) -> Box<dyn DiarizeSession>;
}

/// Per-session: owns its own cluster state. No reset() — fresh per session.
trait DiarizeSession: Send {
    fn label_utterances(&mut self, /* … */) -> /* … */;
}
```

- `OnnxDiarizer` becomes the `DiarizerModel`; `new_session()` returns a scope
  holding `Arc<model>` + a fresh `SessionClusterState` + its own `MelExtractor`.
- `NoopDiarizer` and the test mocks get the same shape (trait-seam rule: prod
  impl + mock updated together).
- The pump holds a `Box<dyn DiarizeSession>` instead of `Arc<dyn Diarize>`.
  `reset()`-at-start and `reload-after-stop` of the shared instance go away — each
  session is born clean and dropped when its finalization ends.
- The post-stop diarizer *reload* in `stop_meeting_and_rebuild_transcriber`
  (memory hygiene) is no longer needed for correctness; the model stays loaded
  and shared. (Removing it also deletes the `diarizer reloaded after meeting stop`
  step seen at log line 335.)

### B. Release-then-finalize pump

`run_pump`'s cancel path is reordered so audio is freed before the expensive work:

1. Cancel observed → **one final `tick_drain_sources`** (capture the last ring-buffer
   audio) → run inference on it.
2. **Drop `ctx.handles`** (the `AudioSession` boxes). Their `Drop` sends `Cmd::Stop`
   to the cpal worker / SCK tap, releasing the singleton (`audio/cpal.rs:452`).
   Emit `MeetingFinalizing { session_id }`.
3. **Background phase** (no audio held): `flush_sessions` (`finish()` per source) →
   tail diarization via the session's own `DiarizeSession` → persist tail finals →
   `repo.close_session(id)` → emit `MeetingSessionEnded { session_id }`.

The split point between foreground (steps 1–2) and background (step 3) is the key
change: `stop_manual` only awaits through step 2.

### C. `MeetingManager` state machine + finalization registry

Today: a single `Mutex<SessionState>` slot (`Idle | Opening | Active | Stopping`).
Because only one session may capture audio at a time (one audio device), the
**live** slot stays single — but finalizations move out of the slot into a set:

```rust
struct MeetingManager {
    live: Mutex<SessionState>,  // Idle | Opening | Active | Releasing
    finalizing: Mutex<HashMap<i64, JoinHandle<()>>>, // N background finalizations
    // …
}
```

- `stop_manual`: swap `Active → Releasing`, signal cancel, await the pump **only
  to the audio-release point** (step 2 above), then swap `Releasing → Idle` and
  register the pump's continuation handle in `finalizing`. Returns immediately after.
- `Releasing` replaces the old `Stopping`, but its meaning narrows: it covers only
  the sub-second foreground window where audio is still held but being released
  (final drain + handle drop). It still blocks a concurrent `start_manual` — which
  is correct and harmless, because the device isn't free yet. The **long** part
  (whisper `finish()` etc.) now happens *after* `Releasing → Idle`, in the
  `finalizing` set, and blocks nothing.
- Mutual exclusion of *live capture* is enforced by the audio singleton + the
  centralized "can a capture start?" gate (D); a finalizing session is not a live
  owner and does not block a new `start_manual`.
- App shutdown joins all `finalizing` handles (best-effort, bounded) so tails are
  flushed where possible.
- `reconcile_orphan_sessions` (existing, #249) already closes any session left
  `ended_at IS NULL` on next launch, so a quit mid-finalization loses only the
  un-flushed tail — same guarantee as a crash today.

> **Implementation note:** the foreground/background split inside one spawned task
> is achieved by having the pump signal an internal "audio released" checkpoint
> (a `oneshot` or notify) that `stop_manual` awaits, while the task itself keeps
> running into step 3. `stop_manual` never holds the pump `JoinHandle` to
> completion — it moves it into `finalizing`.

### D. Centralized session/recording status

**Backend.** A single `recording_gate` (owned by `AppState`) answers "may a new
capture start?" and names the current live owner (`None | Dictation | Meeting(id)`).
Both the dictation start path and meeting `start_manual`/auto-start consult it
instead of independently inspecting `meeting.active_session_id()`. Finalizing
sessions are *not* live owners, so they don't block the gate.

**Frontend.** Introduce one derived status in a single place (extend
`state/dictation.svelte.ts` or a small new `state/recording-status.svelte.ts`):

```ts
type RecordingStatus =
  | { kind: "idle" }
  | { kind: "recording"; mode: "dictation" | "meeting"; meetingId: number | null }
  | { kind: "finalizing"; meetingIds: number[] };   // ≥1 background finalization
```

- `anyRecordingActive`, the document title, the tray `UiRecordingState`, the
  sidebar dot, the HUD driver, and the PTT guard all read this one status.
- New events: `MeetingFinalizing { sessionId }` adds to the finalizing set;
  `MeetingSessionEnded { sessionId }` removes it. `recording` is cleared the moment
  audio is released (on `MeetingFinalizing`), so the panel leaves the "recording"
  look immediately and shows "finishing…".
- PTT guard (`AppLifecycle.svelte:212`) no longer blocks on `meeting.activeId`
  alone; it blocks only when `status.kind === "recording"` (a live capture).
  `finalizing` does **not** block PTT — new dictation is allowed and safe.

### E. HUD / panel UX

- Reuse the existing HUD `processing` visual (shimmer bar, `hud/+page.svelte`)
  for `finalizing`, driven by the centralized status rather than the dictation
  `transcribing` flag.
- Meeting panel shows a quiet "finishing transcription…" line while the session is
  in the finalizing set; clears on `MeetingSessionEnded`.
- The existing `MeetingTailDropped` notice path is unchanged (the 60 s per-source
  timeout still exists as a backstop; it now fires in the background without
  blocking anything).

## Data flow on stop (target)

```
User presses Stop
  → meeting_stop_manual
    → MeetingManager::stop_manual
        signal cancel
        await pump → [final drain + inference] → [drop audio handles] → emit MeetingFinalizing
        live = Idle; register continuation in `finalizing`
      RETURNS  (sub-second; Stop button resolves, mic free, waveform stops)
  … pump continuation (background) …
        flush_sessions: finish() per source  (whisper; may be slow)
        tail diarization via session's DiarizeSession
        persist tail finals
        repo.close_session(id)
        emit MeetingSessionEnded; remove from `finalizing`
```

A new dictation or meeting started any time after RETURNS acquires the freed audio
device and its own (dictation or per-session-meeting) inference/diarizer state.

## Concurrency safety analysis

- **Diarizer:** per-session `DiarizeSession` → no shared mutable cluster state.
  Safe under N concurrent finalizations + a new live session. ✔
- **Audio device:** exactly one live capture at a time, enforced by the centralized
  gate + the cpal/SCK singleton. Finalizations hold no audio. ✔
- **Dictation vs meeting transcribe contexts:** separate slots (#248); a new
  dictation never contends with meeting finalization. ✔
- **Meeting `WhisperContext`:** concurrent meeting finalizations (and a new live
  meeting) may share one `Arc<Mutex<WhisperContext>>` and **serialize** on its
  mutex — correct but not parallel. See Risks. ✔ (correctness) / ⚠ (latency)
- **Post-stop rebuild (#612/#636):** finalizations hold their own `Arc` snapshots
  of the meeting context, so the slot swap is safe; memory is reclaimed when each
  finalization drops its snapshot. ✔

## Error handling

- `finish()` per-source timeout / panic / error: unchanged behavior
  (`emit_meeting_tail_dropped`, continue) — now in the background.
- `close_session` failure in the background: log + leave row open; orphan-reconcile
  closes it next launch (cannot surface a retry to a Stop button that already
  returned — acceptable, matches crash semantics).
- A finalization task panicking must not poison the `finalizing` map mutex — use
  catch/`JoinHandle` error logging as today (`lifecycle.rs:529`).

## Testing

- **Rust unit (trait-seam):** mock `DiarizerModel`/`DiarizeSession`; assert two
  concurrent finalizations produce independent cluster IDs (the corruption this
  prevents). Assert `stop_manual` returns before `finish()` completes (inject a
  slow mock `finish`), and that audio handles are dropped before it returns.
- **Meeting manager:** start B while A is finalizing → both succeed; `finalizing`
  holds the expected ids; shutdown joins them.
- **IPC integration** (`ipc/tests.rs` + `AppStateBuilder`): `meeting_stop_manual`
  returns promptly with a slow finish mock; `MeetingFinalizing` then
  `MeetingSessionEnded` emitted in order; tail finals land in `MemHistory`/repo.
- **Frontend unit (vitest):** centralized `RecordingStatus` transitions
  recording → finalizing → idle on the new events; PTT guard allows start during
  finalizing, blocks during recording.
- **Playwright (Path A):** mock the new events; assert panel shows "finishing…",
  HUD shimmer, and that a dictation can be started during finalization.
- Four-place IPC sync rule applies to any new/changed command + the new events.
- `npm run tauri dev` smoke required (touches `lib.rs` setup / `AppState`,
  meeting state machine — startup-touching per CLAUDE.md).

## Risks / open tradeoffs

1. **Shared meeting `WhisperContext` serialization.** If a new *live* meeting runs
   tick-inference while an old finalization runs `finish()` on the same
   `Arc<Mutex<WhisperContext>>`, the live meeting's ticks wait behind the
   finalization inference. Mitigation options, deferred unless observed to hurt:
   (a) accept it (finish() is usually seconds); (b) give each meeting session its
   own `WhisperContext` (more compute-buffer memory). **Decision for v1: accept;
   note for follow-up.**
2. **State-machine refactor blast radius.** Narrowing `Stopping` → `Releasing` and
   adding the `finalizing` registry touches `meeting/manager.rs` + `lifecycle.rs` +
   the centralized gate + frontend status. The existing `#492` (race-aware restore)
   and `#839` (Stopping → Idle) tests must be adapted, since the DB `close_session`
   moves out of `stop_manual` into the background finalization — the restore-on-
   close-failure recovery now lives in the finalization task, not the stop path.
3. **Event ordering.** Frontend must tolerate `MeetingSessionEnded` for a session
   it never saw `MeetingFinalizing` for (fast finalize, coalesced) — the status
   reducer treats both as set add/remove and is order-tolerant.

## Rollout

Single feature branch; one squash-merge PR. Suggested internal commit order:
(1) per-session diarizer trait + impls + mocks; (2) release-then-finalize pump +
manager `finalizing` registry + centralized backend gate; (3) events + frontend
centralized status + HUD/panel/PTT; (4) tests; (5) docs (`ARCHITECTURE.md` meeting
dataflow, `learnings.md` entry, this proposal → "accepted").
