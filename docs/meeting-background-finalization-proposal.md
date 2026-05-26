# Meeting background finalization — design proposal

**Status:** Draft for review (v1 scope, revised after adversarial review) ·
**Date:** 2026-05-26 · **Author:** Ken + Claude

> **Revision note.** An initial draft proposed *full concurrency* (a new meeting
> starting while previous meetings finalize, N concurrent finalizations) backed by
> a per-session diarizer trait split + a backend capture gate. Three independent
> adversarial reviews (two internal, plus Copilot as an external model) converged
> on: keep the direction, **cut that scope**. The decisive finding — all meeting
> streaming sessions share one `Arc<Mutex<WhisperContext>>` (`whisper.rs:460`) and
> `infer`/`finish` hold that lock across the whole inference — means a *new live
> meeting* running while an *old meeting finalizes* would freeze the new meeting's
> transcript behind the old one's `finish()`, re-creating the very symptom we are
> removing. Doing meeting-vs-meeting concurrency correctly would require per-session
> whisper contexts (more memory) + a diarizer trait split (more maintenance). The
> decision: ship a lean v1 that removes ~90% of the pain with **no added
> maintenance or memory overhead**, and defer concurrent meetings (see
> [Deferred](#deferred-concurrent-meetings)).

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

## Goals (v1)

1. On stop, **release the audio device immediately** (frees the singleton, stops
   the waveform), then run tail transcription/diarization/persistence in the
   **background**, so the Stop button resolves in well under a second.
2. **Dictation works during finalization.** A push-to-talk dictation can start the
   moment the meeting's audio is released, even while the meeting's tail is still
   processing. (This is the primary felt success criterion. It is inherently safe:
   dictation uses a *separate* transcriber slot (#248, `ipc/state.rs:266`) and never
   touches the diarizer.)
3. **Subtle, non-blocking UX**: a quiet "finishing transcription…" indicator on the
   meeting panel and HUD; tail utterances trickle in via the existing event/poll
   path.
4. **Centralize the *frontend* recording status** (`idle | recording | finalizing`)
   so the panel, HUD, document title, tray, and the push-to-talk guard read one
   source of truth instead of today's scattered `meeting.activeId !== null` checks.

## Non-goals (v1) — see [Deferred](#deferred-concurrent-meetings)

- A **new meeting** starting *while a previous meeting finalizes*. In v1 a new
  meeting waits for the prior finalization (normally sub-second). Deferred because
  doing it correctly needs per-session whisper contexts (memory) + diarizer
  isolation (maintenance).
- **N concurrent finalizations.** Only reachable via the deferred meeting-vs-meeting
  case, so v1 has a **single finalization lane**.
- Per-session whisper contexts, the diarizer trait split, a backend capture gate.
- Fixing #612 whisper memory growth, or trimming silence to speed inference (filed
  separately as **#974**, complementary).

## Approved decisions

- **Scope:** lean v1 (above), no added maintenance/memory overhead. Concurrent
  meetings deferred and documented for future pickup.
- **UX:** subtle "finishing…" indicator, non-blocking.
- **Follow-up:** silence-trimming tracked in **#974**.

## Architecture

### A. Release-then-finalize pump

`run_pump`'s cancel path is reordered so audio is freed *before* the expensive work,
and the audio release is **acknowledged, not fire-and-forget**:

1. Cancel observed → **one final drain** of each source's ring buffer.
2. **Explicit, ack-waited stop** of each `AudioSession`: call a stop that round-trips
   `Cmd::Stop` through the cpal worker (and the SCK tap equivalent) and **returns the
   final drained buffer**, then feed that tail audio into the streaming session.
   - This replaces relying on `CpalMicSessionHandle::Drop`, which sends `Cmd::Stop`
     *without waiting for the reply* and **discards the returned `CapturedAudio`**
     (`audio/cpal.rs:462–474`). Two reasons this matters, both raised in review:
     (a) **"handle dropped ≠ device free"** — a new capture issued immediately after
     could still hit `"recording already in progress"` because the worker hasn't
     processed the Stop yet; (b) **tail-loss** — any samples that arrive between the
     step-1 drain and the stop would be dropped on the floor by the Drop path.
   - Implementation must verify cpal's `Cmd::Stop` returns the drained buffer and
     decrements `active_sessions` before the reply, and add/confirm an equivalent
     ack-waited stop on the SCK tap.
3. Audio is now released. Emit `MeetingFinalizing { session_id }`; signal the
   foreground checkpoint (see C) so `stop_manual` can return.
4. **Background phase** (no audio held): `flush_sessions` (`finish()` per source) →
   tail diarization → persist tail finals → **speaker-identity resolution**
   (`session_centroids()`, #667 — moved here from `stop_manual`, see C) →
   `repo.close_session(id)` → emit `MeetingSessionEnded { session_id }`.

The split point between foreground (steps 1–3) and background (step 4) is the key
change.

### B. State machine + single finalization lane

Today: a single `Mutex<SessionState>` slot (`Idle | Opening | Active | Stopping`).
Changes:

```rust
struct MeetingManager {
    live: Mutex<SessionState>,             // Idle | Opening | Active | Releasing
    finalizing: Mutex<Option<JoinHandle<()>>>, // single lane (see below)
    // …
}
```

- `stop_manual`: swap `Active → Releasing`, signal cancel, **await the pump only to
  the audio-release checkpoint** (A step 3), swap `Releasing → Idle`, store the pump's
  continuation handle in `finalizing`, and return. Sub-second.
- `Releasing` replaces `Stopping`, narrowed to only the brief foreground
  audio-release window. It still blocks a concurrent meeting `start_manual` — correct,
  the device isn't free yet.
- **Single finalization lane is sufficient.** Two concurrent finalizations can only
  arise if a new meeting starts mid-finalize — which v1 forbids — so `Option` not a map.
- **A new meeting `start_manual` (and meeting auto-start) awaits `finalizing`** before
  opening. Rationale: a new meeting would share the diarizer cluster state *and* the
  meeting `WhisperContext` with the finalizing one. Waiting for finalization to fully
  complete (incl. tail diarize + centroid snapshot) keeps the **existing, unmodified**
  shared diarizer correct — no isolation work, no extra memory. Normally sub-second.
- **Dictation start does NOT await `finalizing`.** Once `live = Idle` (audio released),
  `meeting_manager.active_session_id()` returns `None`, so the existing dictation start
  path proceeds, and it shares nothing with the finalizing meeting.

### C. What moves into the background task (and the behavior changes that implies)

Three things currently inside `stop_manual` move into the background finalization,
because they must run *after* the tail `finish()`:

1. **`repo.close_session(id)`** (`lifecycle.rs:550`). **Behavior change, accepted:** the
   `#492`/`#839` close-retry recovery (`lifecycle.rs:561–611`) restored
   `Active(close_attempted=true)` so the user could retry a failed DB close. With close
   in the background, a failed close can no longer surface a retry to an
   already-returned Stop — it is logged and the row is closed by
   `reconcile_orphan_sessions` on next launch (#249). This is the same guarantee as a
   crash today. The `close_attempted` retry path is removed; its tests are updated.
2. **Speaker-identity resolution** (`session_centroids()`, #667,
   `lifecycle.rs:622–637`). Reads the diarizer's cluster centroids for *this* session.
   Safe to move because the "new meeting awaits finalization" rule (B) guarantees
   nothing else has `reset()` the shared diarizer before this runs.
3. **Tail persistence** (the `diarize_and_dispatch_merged` of `tail_buckets`), already
   the last thing the pump does.

### D. Centralized frontend recording status

One derived status in a single place (extend `state/dictation.svelte.ts`, which already
centralizes `meetingOnlyActive`/`anyRecordingActive`):

```ts
type RecordingStatus =
  | { kind: "idle" }
  | { kind: "recording"; mode: "dictation" | "meeting"; meetingId: number | null }
  | { kind: "finalizing"; meetingId: number };
```

- `anyRecordingActive`, document title, tray `UiRecordingState`, sidebar dot, and the
  HUD driver read this one status.
- New events: `MeetingFinalizing { sessionId }` → status `finalizing`;
  `MeetingSessionEnded { sessionId }` → `idle`. `recording` clears the moment audio is
  released (on `MeetingFinalizing`), so the panel leaves the "recording" look
  immediately and shows "finishing…".
- **No new backend capture gate.** Review consensus: the existing `SessionState` slot +
  the cpal/SCK singleton are already the authority for "can a capture start". Adding a
  third source of truth would only invite drift. The "new meeting waits" rule lives in
  the meeting manager (await `finalizing`), and dictation-during-finalize works simply
  because `active_session_id()` is `None` once audio is released.
- **PTT guard** (`AppLifecycle.svelte:212`) blocks only when `status.kind ===
  "recording"` (a live capture). `finalizing` does **not** block PTT.

### E. HUD / panel UX

- Reuse the existing HUD `processing` visual (shimmer bar, `hud/+page.svelte`) for
  `finalizing`, driven by the centralized status.
- Meeting panel shows a quiet "finishing transcription…" line while the session is the
  `finalizing` one; clears on `MeetingSessionEnded`.
- The existing `MeetingTailDropped` notice path is unchanged (the 60 s per-source
  timeout still exists as a backstop; it now fires in the background, blocking nothing).

## Data flow on stop (target)

```
User presses Stop
  → meeting_stop_manual → MeetingManager::stop_manual
        Active → Releasing; signal cancel
        await pump → [final drain] → [ack-waited audio stop, capture tail] → emit MeetingFinalizing
        Releasing → Idle; store continuation in `finalizing`
      RETURNS  (sub-second; Stop resolves, mic free, waveform stops)
  … pump continuation (background, single lane) …
        flush_sessions: finish() per source  (whisper; may be slow)
        tail diarization; persist tail finals
        speaker-identity resolution (session_centroids, #667)
        repo.close_session(id)
        emit MeetingSessionEnded; clear `finalizing`

New DICTATION after RETURNS: starts immediately (separate slot, no diarizer).
New MEETING after RETURNS:    awaits `finalizing` (normally sub-second), then opens.
```

## Concurrency safety analysis

- **Diarizer:** unchanged shared instance. At most one meeting touches its cluster
  state at a time — the "new meeting awaits finalization" rule serializes them. ✔
- **Meeting `WhisperContext`:** never shared between a live meeting and a finalizing
  meeting, because a new meeting can't start until finalization completes. The freeze
  the initial draft risked **cannot occur in v1 by construction**. ✔
- **Audio device:** one live capture at a time (cpal/SCK singleton + `Releasing`);
  finalizations hold no audio; the ack-waited stop guarantees the device is actually
  free before `stop_manual` returns. ✔
- **Dictation vs meeting:** separate slots (#248), no diarizer; fully independent. ✔
- **Post-stop rebuild (#612/#636):** the finalization holds its own `Arc` snapshot of
  the meeting context, so the slot swap stays safe; memory reclaims when the single
  finalization drops its snapshot. ✔

## Error handling

- `finish()` per-source timeout / panic / error: unchanged behavior
  (`emit_meeting_tail_dropped`, continue) — now in the background.
- Background `close_session` failure: log + leave row open; `reconcile_orphan_sessions`
  closes it next launch (cannot surface a retry to a Stop that already returned — see C).
- A finalization task panic must not poison the `finalizing` mutex: the handle is
  cleared by a small supervisor (the task's own completion path *and* a `Drop`/shutdown
  sweep), not left dangling.
- **App shutdown:** adopt **abort-and-reconcile**, not "flush where possible". The
  `finish()` runs in `spawn_blocking` and **cannot be cancelled** (`pump.rs:80–82`), so
  joining could hang quit up to 60 s. On quit, abort/drop the finalization and let
  `reconcile_orphan_sessions` close the row next launch — same tail-loss as a crash
  today. (`SessionManager::Drop`, `manager.rs:288`, must learn about `finalizing`.)

## Testing

- **Rust unit / IPC integration** (`ipc/tests.rs` + `AppStateBuilder`, `Mem*` mocks):
  - `meeting_stop_manual` returns **before** `finish()` completes (inject a slow mock
    `finish`); audio is actually released (ack-waited stop observed) before it returns.
  - `MeetingFinalizing` then `MeetingSessionEnded` emitted in order; tail finals land in
    the repo; `close_session` called in the background.
  - A new **meeting** `start_manual` **awaits** an in-flight finalization; a new
    **dictation** start does **not** wait and succeeds while finalizing.
  - Speaker-identity (#667) still resolves from the moved `session_centroids()` call.
  - No tail-audio loss across the drain→stop boundary (samples arriving during release
    are captured by the ack-waited stop).
  - Adapt `#492`/`#839` tests for close-in-background (retry affordance removed).
- **Frontend unit (vitest):** `RecordingStatus` transitions recording → finalizing →
  idle on the new events; PTT guard allows start during finalizing, blocks during
  recording.
- **Playwright (Path A):** mock the new events; panel shows "finishing…", HUD shimmer,
  and a dictation can start during finalization.
- Four-place IPC sync rule applies to the new events.
- `npm run tauri dev` smoke required (touches `lib.rs`/`AppState`, meeting state machine
  — startup-touching per CLAUDE.md).

## Deferred: concurrent meetings

Out of v1 deliberately, to avoid memory + maintenance overhead. Documented here (and to
be cross-referenced from code comments + a `learnings.md` entry) so it's cheap to resume.

**What's deferred:** a new meeting starting while a previous meeting finalizes, and
therefore N concurrent finalizations.

**Why it's hard (the load-bearing constraint):** all meeting streaming sessions clone
one `Arc<Mutex<WhisperContext>>` (`transcription/whisper.rs:460`); `infer`/`finish` hold
that lock across the entire inference (`whisper.rs:662–690`, no early `drop`). A live
meeting + a finalizing meeting sharing it → the live transcript freezes behind the old
`finish()` for up to 60 s. The shared diarizer (`diarization/onnx.rs`, one
`Mutex<SessionClusterState>`, `reset()` at pump start, #794) has the same problem for
speaker clustering.

**How to pick it up later:**
1. Give each meeting *session* its own `WhisperContext` (or at least each *finalization*
   its own), so a finalize never blocks a live meeting. Costs compute-buffer memory per
   the #612 numbers; **bound the number of concurrent finalizations** and queue beyond it.
2. Isolate diarizer cluster state per session. Lowest-blast-radius option: construct a
   fresh `OnnxDiarizer` per session sharing the heavy model behind an `Arc` (the model is
   `Send+Sync`, `onnx.rs`), rather than a full trait split. **Verify `MelExtractor::extract`
   is `&self`** before sharing the model; if it mutates scratch, give each session its own.
   Preserve the `FlagGatedDiarizer` enable-flag + hot-swap-slot dynamism
   (`diarization/mod.rs:179`) — a session-bound diarizer must still observe the runtime
   toggle and a mid-session model download.
3. Promote `finalizing: Option<JoinHandle>` → a bounded map with a supervisor that
   removes entries on completion *and* panic.
4. Relax the "new meeting awaits finalization" rule (B) once 1–3 hold.

**Complementary:** silence-trimming (**#974**) shrinks the tail `finish()`, reducing both
the v1 "new meeting waits" delay and the eventual contention window.

## Rollout

Single feature branch (`feat/meeting-background-finalization`), one squash-merge PR.
Suggested commit order:
1. Release-then-finalize pump + ack-waited audio stop (A).
2. Manager `Releasing` + single `finalizing` lane + "new meeting awaits finalization"
   gate; move close/centroids to background (B, C).
3. New events + centralized frontend `RecordingStatus` + HUD/panel/PTT (D, E).
4. Tests (Rust + vitest + Playwright).
5. Docs: `ARCHITECTURE.md` meeting dataflow; **`learnings.md` entry** on the descope
   decision + the deferred-concurrency constraint; code comments at the
   `finalizing`/await-gate and the shared-`WhisperContext` site pointing at the
   [Deferred](#deferred-concurrent-meetings) section and #974; this proposal → "accepted".
