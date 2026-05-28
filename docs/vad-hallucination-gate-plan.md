# VAD Hallucination-Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop Whisper from hallucinating "`.com`" / "Thanks for watching!" / repeating-phrase artefacts on silent or non-speech windows, by gating `infer()` behind a Silero VAD speech-presence check.

**Architecture:** A new `Vad` trait pair (`VadModel` shared + `VadSession` per-stream) wraps a bundled Silero VAD ONNX model via `tract-onnx` (already in the tree for wespeaker diarization). The gate lives in `WhisperStreamingSession::tick()` — before delegating to `SlidingWindowState`, check whether speech was detected in the recently-fed audio (with hangover); if not, skip the inference call and return no segments. Plus two one-line defense-in-depth tweaks to `FullParams`: `set_temperature(0.0)` and `set_suppress_nst(true)`.

**Tech Stack:** Rust (Tauri 2 backend, `tract-onnx`, `whisper-rs 0.14`), `cargo test --lib`, `cargo clippy`.

**Spec:** `docs/vad-hallucination-gate-proposal.md` on this branch. **Issue:** [#974](https://github.com/khawkins98/Hush/issues/974).

**Branch:** `feat/vad-hallucination-gate` (already created; the proposal is committed at `e721cd7`).

---

## Pre-flight

- [ ] **Confirm branch + clean tree + baseline green**

```bash
cd /Users/khawkins/Documents/git/Hush
git rev-parse --abbrev-ref HEAD   # expect: feat/vad-hallucination-gate
git status --porcelain            # expect: empty (the proposal commit is the only delta)
cd src-tauri && cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -3
```
Expected: 489 passed (baseline). If the swift-dylib error from `CLAUDE.md` appears, prefix with `DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx`.

---

## File map (what changes, why)

**New (this branch):**
- `src-tauri/src/vad/mod.rs` — module root: `VadModel` + `VadSession` trait pair, `NoopVad`/`NoopVadSession`, constants (`FRAME_LEN_SAMPLES`, `SAMPLE_RATE`), public re-exports.
- `src-tauri/src/vad/onnx.rs` — `SileroVad` (the `VadModel`) and `SileroVadSession`. Loads the bundled Silero v5 ONNX via `tract-onnx`. Frame size 512 at 16kHz.
- `src-tauri/src/vad/tests.rs` (inline `#[cfg(test)]` in `mod.rs`) — Noop behavior tests.
- `src-tauri/assets/silero_vad.onnx` — bundled Silero v5.1.2 model (1.7MB). Sourced from `https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx` — MIT licensed. Committed to repo; loaded via `include_bytes!`.

**Modified:**
- `src-tauri/src/lib.rs` — `mod vad;` registration; load `SileroVad` at startup into the `AppState`-built `Arc<dyn VadModel>` slot.
- `src-tauri/src/ipc/state.rs` — `InferenceState` gains a `vad: Arc<dyn vad::VadModel>` field next to `diarize`; `AppStateBuilder` gains a `with_vad` setter (defaults to `NoopVad` for tests).
- `src-tauri/src/transcription/whisper.rs` — `WhisperStreamingSession` gains `vad_session: Box<dyn VadSession>`, `vad_residual: Vec<f32>`, `last_speech_at: Option<Instant>`, plus the gate threshold/hangover constants from env. Constructor signature gains a `Box<dyn VadSession>` parameter. `feed()` drains the residual into the VAD; `tick()` checks the gate before delegating. Also adds `set_temperature(0.0)` + `set_suppress_nst(true)` to both `WhisperInferer::infer` and the one-shot transcription path's `FullParams` setup.
- `src-tauri/src/transcription/mod.rs` — the `Transcribe::start_stream` trait method signature gains a `Box<dyn VadSession>` parameter so the meeting and dictation paths can pass the session minted from the `VadModel` slot.
- `src-tauri/src/meeting/lifecycle.rs` — at `start_stream` call sites, mint a `VadSession` from the `vad` slot and pass it.
- `src-tauri/src/ipc/commands/dictation/pipeline.rs` (or wherever dictation start lives) — same: mint a VAD session at start, pass through.
- `src-tauri/src/meeting/test_support.rs` and `src-tauri/src/ipc/tests.rs` — update the builders/mocks to thread `NoopVad` through where they construct `AppState` / `WhisperStreamingSession`.

**Docs:**
- `learnings.md` 2026-05-28 entry.
- `ARCHITECTURE.md` — short note on the VAD gate in the meeting-pump dataflow.
- `docs/vad-hallucination-gate-proposal.md` — flip status to "Accepted; implemented in PR #N" before merge.

---

## Task 1: VAD trait pair + Noop impls (skeleton)

Smallest isolated unit — no behavior change yet. Lays down the seam.

**Files:**
- Create: `src-tauri/src/vad/mod.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod vad;`)
- Test: inline `#[cfg(test)]` in `vad/mod.rs`

- [ ] **Step 1: Create `vad/mod.rs` with trait pair + Noop + constants**

```rust
//! Voice Activity Detection — gates whisper inference behind a speech-presence
//! check so silent / non-speech windows don't trigger hallucinations
//! (".com", "Thanks for watching!", repeating-phrase loops). See
//! `docs/vad-hallucination-gate-proposal.md` for the design rationale and
//! `learnings.md` 2026-05-28 for context.
//!
//! Production wires [`onnx::SileroVad`] into the [`crate::ipc::state::InferenceState`]
//! `vad` slot at startup. Each streaming transcription session mints a fresh
//! [`VadSession`] at start (`new_session`) and feeds frames through it as
//! audio arrives.

pub mod onnx;

use anyhow::Result;

/// Sample rate the VAD operates at — matches the streaming inferer's mono-16kHz contract.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Silero VAD v5 expects 512-sample frames at 16kHz (~32ms). Exposing it as a
/// constant lets the caller chunk newly-fed audio correctly without hard-coding.
pub const FRAME_LEN_SAMPLES: usize = 512;

/// Heavy, immutable, shared across the app. Loads the ONNX model once.
/// Hands out per-stream [`VadSession`]s, each with its own recurrent state.
pub trait VadModel: Send + Sync {
    /// Mint a fresh per-stream session with zero-initialised recurrent state.
    fn new_session(&self) -> Box<dyn VadSession>;
}

/// Per-stream state for one ongoing audio source. Mutable because Silero's
/// LSTM hidden state evolves across calls. Calls MUST be in temporal order
/// on the same session — feeding frame N requires the prior call was for
/// frame N-1.
pub trait VadSession: Send {
    /// Speech probability ∈ [0,1] for one [`FRAME_LEN_SAMPLES`]-sample frame at
    /// [`SAMPLE_RATE_HZ`]. Updates internal state. Returns an error only if
    /// inference itself fails; never panics on slice length.
    fn score_frame(&mut self, frame: &[f32]) -> Result<f32>;
}

/// No-op fallback: always reports speech, so the gate never fires.
/// Used when the production Silero model fails to load (degrade gracefully —
/// transcription works as today, just without the gate) and by tests that
/// aren't exercising the gate.
pub struct NoopVad;

impl VadModel for NoopVad {
    fn new_session(&self) -> Box<dyn VadSession> {
        Box::new(NoopVadSession)
    }
}

pub struct NoopVadSession;

impl VadSession for NoopVadSession {
    fn score_frame(&mut self, _frame: &[f32]) -> Result<f32> {
        Ok(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_session_always_reports_full_speech() {
        let model = NoopVad;
        let mut session = model.new_session();
        let frame = vec![0.0f32; FRAME_LEN_SAMPLES];
        assert_eq!(session.score_frame(&frame).unwrap(), 1.0);
        // Repeated calls still return 1.0; no state to corrupt.
        assert_eq!(session.score_frame(&frame).unwrap(), 1.0);
    }

    #[test]
    fn constants_match_silero_v5_contract() {
        // Silero v5 mandates 512-sample frames at 16kHz; both are load-bearing
        // for downstream chunking and ONNX I/O. Pinned so a careless edit
        // doesn't silently regress.
        assert_eq!(FRAME_LEN_SAMPLES, 512);
        assert_eq!(SAMPLE_RATE_HZ, 16_000);
    }
}
```

- [ ] **Step 2: Create `vad/onnx.rs` stub (real impl comes in Task 3)**

```rust
//! Production [`crate::vad::VadModel`] backed by the bundled Silero VAD v5
//! ONNX model via `tract-onnx`. Implemented in Task 3 of the plan —
//! this stub exists so `vad/mod.rs` compiles after Task 1.

// Stub kept intentionally empty; Task 3 fills it in.
```

- [ ] **Step 3: Register the module in `lib.rs`**

Add `pub mod vad;` near the other `pub mod` declarations in `src-tauri/src/lib.rs` (find a clean spot near `pub mod diarization;`).

- [ ] **Step 4: Verify**

```bash
cd src-tauri && cargo test --lib --features whisper,diarization-onnx vad:: 2>&1 | tail -8
cargo clippy --lib --no-default-features -- -D warnings 2>&1 | tail -3
```
Expected: both new tests pass; clippy clean. Prefix with `DYLD_FALLBACK_LIBRARY_PATH=...` if the swift-dylib error appears.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/vad/ src-tauri/src/lib.rs
git commit -m "feat(vad): add VadModel/VadSession trait pair + Noop impls (#974)"
```

---

## Task 2: Gate logic in `WhisperStreamingSession` (TDD with mock VAD)

The functional change. Implements the gate behavior using a mock `VadSession` — no real Silero yet.

**Files:**
- Modify: `src-tauri/src/transcription/whisper.rs` (constructor, `feed`, `tick`, new fields)
- Modify: `src-tauri/src/transcription/mod.rs` (`Transcribe::start_stream` signature gains a `Box<dyn VadSession>` parameter)
- Modify: `src-tauri/src/transcription/streaming.rs` if test mocks live there
- Modify: callers (`meeting/lifecycle.rs`, dictation start path, `meeting/test_support.rs`, IPC tests) to pass `NoopVadSession` for now — Task 4 wires real VAD through `AppState`.

### Step 1: Read the current `WhisperStreamingSession` shape

```bash
grep -n "pub struct WhisperStreamingSession\|impl WhisperStreamingSession\|impl StreamingTranscribeSession for WhisperStreamingSession\|fn feed\|fn tick\|fn finish" src-tauri/src/transcription/whisper.rs | head -20
```
Note the struct fields, the `new(...)` signature, and the `feed`/`tick`/`finish` impls. The plan's edits below extend these — match the existing style (doc comments, locking, etc.).

### Step 2: Read `Transcribe::start_stream` and all its call sites

```bash
grep -rn "fn start_stream\|\.start_stream(" src-tauri/src/ | head -20
```
You'll be adding a `vad_session: Box<dyn VadSession>` parameter to the trait method, so every caller needs to thread one through. In this task, callers pass `Box::new(crate::vad::NoopVadSession)`; Task 4 replaces that with `app_state.inference.vad.new_session()`.

### Step 3: Write the failing gate tests

Add to the `#[cfg(test)] mod tests` block in `src-tauri/src/transcription/whisper.rs` (where the existing `WhisperStreamingSession` tests live — find them with `grep -n "mod tests" src-tauri/src/transcription/whisper.rs`):

```rust
// ---- VAD gate tests (#974) ----

/// A scripted VadSession: returns probabilities from a queue in order.
/// When the queue empties, returns 0.0 (silence) forever. Used to drive
/// gate-behavior tests deterministically without loading the real model.
struct ScriptedVad {
    probs: std::collections::VecDeque<f32>,
}

impl crate::vad::VadSession for ScriptedVad {
    fn score_frame(&mut self, _frame: &[f32]) -> anyhow::Result<f32> {
        Ok(self.probs.pop_front().unwrap_or(0.0))
    }
}

/// All-speech VAD: probability 1.0 for every frame. Equivalent to NoopVad
/// but spelled out here so tests can swap it for ScriptedVad without
/// touching the rest of the harness.
struct AlwaysSpeechVad;
impl crate::vad::VadSession for AlwaysSpeechVad {
    fn score_frame(&mut self, _frame: &[f32]) -> anyhow::Result<f32> { Ok(1.0) }
}

/// All-silence VAD: probability 0.0 for every frame.
struct AlwaysSilenceVad;
impl crate::vad::VadSession for AlwaysSilenceVad {
    fn score_frame(&mut self, _frame: &[f32]) -> anyhow::Result<f32> { Ok(0.0) }
}

#[test]
fn vad_all_speech_does_not_gate_inference() {
    // With AlwaysSpeechVad, tick() must call into the inferer just as it
    // does today — i.e. behaviour is identical to the pre-gate world.
    // Build a WhisperStreamingSession (or its policy equivalent) with a
    // mock inferer that records its calls; assert at least one inference
    // was attempted after feeding a window of audio.
    //
    // The exact construction mirrors the existing `tick()` tests further
    // down in this file; copy that scaffolding and pass `Box::new(AlwaysSpeechVad)`
    // as the VAD session.
    todo!("port from existing tick_* test scaffolding once you've located it");
}

#[test]
fn vad_all_silence_after_hangover_skips_inference() {
    // With AlwaysSilenceVad and the default 1500ms hangover already elapsed
    // (set `last_speech_at` to `Some(Instant::now() - 2 * HANGOVER)` via a
    // #[cfg(test)] setter on the session), tick() must return Ok(empty)
    // WITHOUT calling the inferer. Assert the mock inferer recorded zero
    // calls.
    todo!("port from existing tick_* scaffolding");
}

#[test]
fn vad_speech_then_silence_inside_hangover_still_infers() {
    // Speech detected at t=0, gate checked at t=hangover-500ms (still inside
    // the hangover window). Inference must run. After t=hangover+1ms, the
    // next check skips.
    todo!("port from existing tick_* scaffolding");
}

#[test]
fn feed_chunks_in_frame_len_groups_and_handles_residual() {
    // Feed (FRAME_LEN_SAMPLES * 1.5) samples in one call. The first 512
    // should be VAD-scored (1 call); the remaining 256 should sit in the
    // residual buffer. A second feed of 256 samples should complete that
    // residual and yield a second VAD call. Use a ScriptedVad with two
    // entries [0.0, 1.0] and verify both were consumed.
    todo!("counter on ScriptedVad");
}
```

> The `todo!` placeholders are intentional **for this Step's listing only** — write the actual test bodies before running Step 4 by copying the existing `tick_*` test scaffolding from the same `mod tests` block in `whisper.rs`. Each `todo!` has a one-sentence directive for what to assert; the existing tests in that module show how to mock the inferer.

### Step 4: Run tests to see them fail

```bash
DYLD_FALLBACK_LIBRARY_PATH=/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift-5.5/macosx \
  cargo test --lib --features whisper,diarization-onnx \
  transcription::whisper::tests::vad_ 2>&1 | tail -15
```
Expected: each test fails because the constructor doesn't accept a VAD session yet and/or the gate logic doesn't exist.

### Step 5: Add gate state + plumbing to `WhisperStreamingSession`

Add fields:

```rust
pub struct WhisperStreamingSession {
    // ... existing fields ...

    // ---- VAD gate state (#974) ----
    /// Per-stream VAD session. Each `feed()` drains accumulated audio in
    /// FRAME_LEN_SAMPLES-sized chunks through this and updates `last_speech_at`.
    vad_session: Box<dyn crate::vad::VadSession>,
    /// Partial frame buffer carried between `feed()` calls (since audio
    /// arrives in arbitrary chunk sizes, but Silero needs exact 512-sample
    /// frames). Drained into the VAD as full frames become available.
    vad_residual: Vec<f32>,
    /// Wall-clock instant of the most recent frame whose VAD probability
    /// crossed the threshold. `None` until the first speech-positive frame.
    /// `tick()` compares `Instant::now().duration_since(last_speech_at)`
    /// against the hangover before allowing inference.
    last_speech_at: Option<std::time::Instant>,
    /// Cached env-var configuration; read once at construction so we don't
    /// re-parse on every frame.
    vad_threshold: f32,
    vad_hangover: std::time::Duration,
    vad_disabled: bool,
}
```

Add a helper to read the env-var configuration (place near the top of the file, alongside any other config helpers):

```rust
/// Read VAD configuration from env vars at session construction.
/// Matches the `HUSH_DIARIZER_THRESHOLD` convention.
///   * `HUSH_VAD_THRESHOLD` → probability threshold (default 0.5)
///   * `HUSH_VAD_HANGOVER_MS` → ms after last speech before gating (default 1500)
///   * `HUSH_VAD_DISABLE=1` → force the gate off (debug / A-B)
fn vad_config_from_env() -> (f32, std::time::Duration, bool) {
    let threshold = std::env::var("HUSH_VAD_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    let hangover_ms = std::env::var("HUSH_VAD_HANGOVER_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1500);
    let disabled = matches!(std::env::var("HUSH_VAD_DISABLE").as_deref(), Ok("1"));
    (threshold, std::time::Duration::from_millis(hangover_ms), disabled)
}
```

Update the constructor signature to take a `Box<dyn VadSession>`:

```rust
impl WhisperStreamingSession {
    pub fn new(
        // ... existing parameters ...
        vad_session: Box<dyn crate::vad::VadSession>,
    ) -> Self {
        let (vad_threshold, vad_hangover, vad_disabled) = vad_config_from_env();
        Self {
            // ... existing field inits ...
            vad_session,
            vad_residual: Vec::with_capacity(crate::vad::FRAME_LEN_SAMPLES),
            last_speech_at: None,
            vad_threshold,
            vad_hangover,
            vad_disabled,
        }
    }
}
```

### Step 6: Implement `feed()` VAD draining

Modify the existing `feed()` (or equivalent — check the actual method name) so it drains accumulated samples through the VAD in 512-sample frames, updating `last_speech_at` whenever a frame scores above threshold. Then forward to the existing `state.feed_mono(samples)` (or whatever the policy module call is).

```rust
fn feed(&mut self, samples: &[f32]) -> Result<()> {
    // Forward to the streaming policy unconditionally — the window/timestamps
    // must stay aligned regardless of the gate.
    self.state.feed_mono(samples);

    // VAD short-circuit: skip the (mildly non-trivial) framing + ONNX call
    // when the gate is disabled.
    if self.vad_disabled {
        // Pretend every frame is speech so the hangover check in tick()
        // never gates. Cheaper than running the model.
        self.last_speech_at = Some(std::time::Instant::now());
        return Ok(());
    }

    // Drain (residual ++ new) in FRAME_LEN_SAMPLES chunks.
    let frame_len = crate::vad::FRAME_LEN_SAMPLES;
    self.vad_residual.extend_from_slice(samples);
    let mut offset = 0usize;
    while self.vad_residual.len() - offset >= frame_len {
        let frame = &self.vad_residual[offset..offset + frame_len];
        match self.vad_session.score_frame(frame) {
            Ok(prob) if prob >= self.vad_threshold => {
                self.last_speech_at = Some(std::time::Instant::now());
            }
            Ok(_) => {}
            Err(e) => {
                // VAD failure shouldn't break transcription. Log once,
                // degrade to "treat as speech" so the gate never blocks
                // real audio, and continue.
                tracing::warn!(error = ?e, "VAD frame scoring failed; falling back to ungated");
                self.last_speech_at = Some(std::time::Instant::now());
            }
        }
        offset += frame_len;
    }
    // Keep only the leftover residual (< frame_len samples).
    self.vad_residual.drain(..offset);
    Ok(())
}
```

### Step 7: Implement the gate in `tick()`

Modify `tick()` to short-circuit when the hangover has elapsed:

```rust
fn tick(&mut self) -> Result<Vec<Utterance>> {
    // VAD gate (#974): skip inference if no recent speech.
    if !self.vad_disabled {
        let should_gate = match self.last_speech_at {
            None => true,
            Some(when) => when.elapsed() > self.vad_hangover,
        };
        if should_gate {
            return Ok(Vec::new());
        }
    }

    // ... existing tick body — build the inferer + call self.state.tick(&mut inferer) ...
}
```

> Note: if the existing `tick()` builds a `WhisperInferer` and calls `self.state.tick(&mut inferer)`, leave that flow intact — just guard it with the early-return above. The gate is purely additive.

### Step 8: Update all call sites of the constructor + `start_stream`

The `Transcribe::start_stream` signature gains a parameter:

```rust
// src-tauri/src/transcription/mod.rs (and the impl in whisper.rs)
fn start_stream(
    &self,
    format: CaptureFormat,
    prompt: &str,
    vad_session: Box<dyn crate::vad::VadSession>,
) -> Result<Box<dyn StreamingTranscribeSession>>;
```

Update every implementation and call site. For this task, callers pass `Box::new(crate::vad::NoopVadSession)` as a placeholder — Task 4 swaps in the real session from the `AppState` VAD slot.

```bash
grep -rn "\.start_stream(" src-tauri/src/ | head
# Update each call site to pass Box::new(crate::vad::NoopVadSession).
```

Common sites: `meeting/lifecycle.rs`, dictation start (search for `transcribe.start_stream`), `meeting/test_support.rs` mocks (the `MockTranscribe` impl), the streaming tests in `transcription/streaming.rs`.

### Step 9: Run the tests and the full suite

```bash
DYLD_FALLBACK_LIBRARY_PATH=... cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -8
cargo clippy --lib --no-default-features -- -D warnings 2>&1 | tail -3
cargo clippy --lib --tests --features whisper,diarization-onnx -- -D warnings 2>&1 | tail -3
cargo fmt --all
```
Expected: all tests pass (incl. the new VAD-gate tests); both clippy gates clean.

### Step 10: Commit

```bash
git add -A
git commit -m "feat(vad): gate whisper inference behind VAD speech-presence check (#974)"
```

---

## Task 3: SileroVad real impl + bundled ONNX model

Loads the Silero v5 ONNX into tract, manages the recurrent LSTM state per session, scores 512-sample frames.

**Files:**
- Create: `src-tauri/assets/silero_vad.onnx` (1.7MB, sourced from Silero v5.1.2 release)
- Modify: `src-tauri/src/vad/onnx.rs` (the real impl, replacing Task 1's stub)
- Test: inline `#[cfg(test)]` in `vad/onnx.rs`

### Step 1: Download and commit the model

```bash
cd /Users/khawkins/Documents/git/Hush
mkdir -p src-tauri/assets
curl -fsSL -o src-tauri/assets/silero_vad.onnx \
  https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx
ls -la src-tauri/assets/silero_vad.onnx   # expect ~1.7MB
shasum -a 256 src-tauri/assets/silero_vad.onnx
```
Record the SHA256 in a constant in `vad/onnx.rs` (used as a self-check at startup — if the bundled bytes don't match, we know the file got corrupted during a git operation).

### Step 2: Inspect the Silero v5 ONNX I/O signature

The ONNX has these inputs/outputs (verify with `python3 -c "import onnx; m = onnx.load('src-tauri/assets/silero_vad.onnx'); print([(i.name, [d.dim_value for d in i.type.tensor_type.shape.dim]) for i in m.graph.input]); print([(o.name, [d.dim_value for d in o.type.tensor_type.shape.dim]) for o in m.graph.output])` if Python+onnx are installed, otherwise check the model card in the Silero repo):

- **Inputs:**
  - `input`: `[batch=1, num_samples=512]` float32 audio.
  - `state`: `[2, batch=1, 128]` float32 — recurrent LSTM state.
  - `sr`: `[]` int64 — sample rate scalar.
- **Outputs:**
  - `output`: `[batch=1, 1]` float32 — speech probability.
  - `stateN`: `[2, batch=1, 128]` float32 — new recurrent state to feed back on next call.

### Step 3: Implement `SileroVad` + `SileroVadSession`

Replace the stub `vad/onnx.rs`:

```rust
//! Silero VAD v5 (`tract-onnx` impl). Loads the bundled ONNX once at
//! startup into [`SileroVad`]; each [`crate::vad::VadSession`] minted from
//! it owns its own LSTM hidden state. Frame size and sample rate are
//! pinned in [`crate::vad`].

use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use tract_onnx::prelude::*;

use crate::vad::{VadModel, VadSession, FRAME_LEN_SAMPLES, SAMPLE_RATE_HZ};

/// Bundled model bytes. Avoids any first-run download dance — the model
/// is always available, and the 1.7MB cost is acceptable for a project
/// that distributes via signed installers.
const SILERO_VAD_ONNX: &[u8] = include_bytes!("../../assets/silero_vad.onnx");

/// SHA256 of the bundled file. Recomputed at build time on every CI run
/// (a tiny startup self-check verifies the in-memory bytes match this so
/// a corrupted asset under git is caught loud).
const SILERO_VAD_SHA256: &str =
    "<paste the SHA256 from step 1's shasum output>";

/// LSTM hidden-state shape: `[2, 1, 128]` (per Silero v5 ONNX signature).
const STATE_SHAPE: [usize; 3] = [2, 1, 128];

pub struct SileroVad {
    /// The compiled tract model. `TypedRunnableModel` is `Send + Sync`,
    /// so this Arc is shared across all sessions without locking.
    model: Arc<TypedRunnableModel<TypedModel>>,
}

impl SileroVad {
    /// Load the bundled model. Returns an error if tract can't parse the
    /// ONNX (means the bundled asset is broken — should fail loud at
    /// startup so we never silently degrade).
    pub fn load() -> Result<Self> {
        // Self-check the bundled bytes haven't been corrupted under git.
        use sha2::{Digest, Sha256};
        let actual = format!("{:x}", Sha256::digest(SILERO_VAD_ONNX));
        if actual != SILERO_VAD_SHA256 {
            return Err(anyhow!(
                "bundled silero_vad.onnx SHA mismatch — expected {}, got {} \
                 (asset may be corrupted in checkout)",
                SILERO_VAD_SHA256,
                actual,
            ));
        }

        let mut cursor = std::io::Cursor::new(SILERO_VAD_ONNX);
        let model = tract_onnx::onnx()
            .model_for_read(&mut cursor)
            .context("parse silero_vad.onnx")?
            // Pin all input dimensions so tract can pre-plan a fast typed graph.
            .with_input_fact(
                0,
                f32::fact([1, FRAME_LEN_SAMPLES]).into(),
            )
            .context("pin Silero `input` shape")?
            .with_input_fact(
                1,
                f32::fact(STATE_SHAPE).into(),
            )
            .context("pin Silero `state` shape")?
            .with_input_fact(
                2,
                i64::fact([]).into(),
            )
            .context("pin Silero `sr` shape")?
            .into_optimized()
            .context("optimise Silero graph")?
            .into_runnable()
            .context("compile Silero runnable")?;

        Ok(SileroVad {
            model: Arc::new(model),
        })
    }
}

impl VadModel for SileroVad {
    fn new_session(&self) -> Box<dyn VadSession> {
        Box::new(SileroVadSession {
            model: Arc::clone(&self.model),
            state: tract_ndarray::Array3::<f32>::zeros(STATE_SHAPE).into_dyn(),
        })
    }
}

pub struct SileroVadSession {
    model: Arc<TypedRunnableModel<TypedModel>>,
    /// LSTM hidden state, shape `[2, 1, 128]`. Re-written on every
    /// `score_frame` call from the model's `stateN` output.
    state: tract_ndarray::ArrayD<f32>,
}

impl VadSession for SileroVadSession {
    fn score_frame(&mut self, frame: &[f32]) -> Result<f32> {
        if frame.len() != FRAME_LEN_SAMPLES {
            return Err(anyhow!(
                "Silero VAD expects frames of exactly {} samples; got {}",
                FRAME_LEN_SAMPLES,
                frame.len(),
            ));
        }

        // Inputs: [audio f32 [1,512]], [state f32 [2,1,128]], [sr i64 []]
        let audio_t: Tensor = tract_ndarray::Array2::from_shape_vec(
            (1, FRAME_LEN_SAMPLES),
            frame.to_vec(),
        )
        .context("build Silero audio tensor")?
        .into();

        let state_t: Tensor = self.state.clone().into();

        let sr_t: Tensor = tract_ndarray::arr0(SAMPLE_RATE_HZ as i64).into_dyn().into();

        let outputs = self
            .model
            .run(tvec!(audio_t.into(), state_t.into(), sr_t.into()))
            .context("Silero VAD inference")?;

        // Outputs: [prob f32 [1,1], stateN f32 [2,1,128]]
        let prob = outputs[0]
            .to_array_view::<f32>()
            .context("read Silero prob")?
            .iter()
            .next()
            .copied()
            .ok_or_else(|| anyhow!("Silero prob output empty"))?;

        // Persist the new hidden state for the next call.
        self.state = outputs[1]
            .to_array_view::<f32>()
            .context("read Silero stateN")?
            .to_owned()
            .into_dyn();

        Ok(prob.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silero_loads_and_scores_silent_frame_low() {
        // Smoke: model loads from the bundled bytes, a frame of zeros
        // gets a low probability (well below the 0.5 default threshold).
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let silent = vec![0.0f32; FRAME_LEN_SAMPLES];
        let p = session.score_frame(&silent).expect("score");
        assert!(
            p < 0.3,
            "silent frame should score low; got {p}"
        );
    }

    #[test]
    fn silero_loads_and_scores_noise_frame_higher_than_silent() {
        // We can't assert "noise = speech" because Silero is trained on
        // speech, not arbitrary noise. But we CAN assert ordering — a
        // frame of structured noise (a sine wave) should score *at least*
        // as high as a flat silent frame after a few frames of warmup
        // (because the LSTM has to settle).
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let silent = vec![0.0f32; FRAME_LEN_SAMPLES];

        // Warm up the LSTM with a few silent frames so the state is settled.
        for _ in 0..4 {
            let _ = session.score_frame(&silent).unwrap();
        }
        let p_silent = session.score_frame(&silent).unwrap();

        // Sine wave at ~200Hz (vaguely speech-band, structured).
        let sr = SAMPLE_RATE_HZ as f32;
        let sine: Vec<f32> = (0..FRAME_LEN_SAMPLES)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * (i as f32) / sr).sin() * 0.5)
            .collect();
        for _ in 0..4 {
            let _ = session.score_frame(&sine).unwrap();
        }
        let p_noise = session.score_frame(&sine).unwrap();

        assert!(
            p_noise >= p_silent,
            "structured-noise prob ({p_noise}) should be >= silent prob ({p_silent})"
        );
    }

    #[test]
    fn silero_rejects_wrong_frame_size() {
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let wrong = vec![0.0f32; FRAME_LEN_SAMPLES + 1];
        assert!(session.score_frame(&wrong).is_err());
    }
}
```

### Step 4: Verify `sha2` is in `Cargo.toml`

```bash
grep -n "^sha2\b" src-tauri/Cargo.toml
```
If absent, add `sha2 = "0.10"` to `[dependencies]`. (It may already be present via a transitive dep — `cargo tree | grep sha2` will tell you.)

### Step 5: Run

```bash
DYLD_FALLBACK_LIBRARY_PATH=... cargo test --lib --features whisper,diarization-onnx vad::onnx 2>&1 | tail -10
cargo clippy --lib --tests --features whisper,diarization-onnx -- -D warnings 2>&1 | tail -3
```
Expected: 3 new tests pass; clippy clean.

> If `into_optimized()` fails because tract-onnx doesn't support some op Silero v5 uses, the spec calls out the fallback: try Silero v4 (`https://github.com/snakers4/silero-vad/raw/v4.0/files/silero_vad.onnx` — a simpler conv-only model with no LSTM ops). If even v4 fails, escalate (STATUS: BLOCKED) — don't silently swap algorithms.

### Step 6: Commit

```bash
git add src-tauri/assets/silero_vad.onnx src-tauri/src/vad/onnx.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(vad): SileroVad ONNX impl with bundled v5 model (#974)"
```

---

## Task 4: Wire `SileroVad` through `AppState` into both pipelines

Replaces the `NoopVadSession` placeholder from Task 2's call sites with real sessions minted from a shared `Arc<dyn VadModel>` in `InferenceState`.

**Files:**
- Modify: `src-tauri/src/ipc/state.rs` — `InferenceState` gains `vad: Arc<dyn vad::VadModel>`; `AppStateBuilder` gets `with_vad`.
- Modify: `src-tauri/src/lib.rs` (or wherever `AppState::build_default` lives) — load `SileroVad` once at startup, fall back to `NoopVad` on failure.
- Modify: `src-tauri/src/meeting/lifecycle.rs` (where `start_stream` is called per source) — mint from the slot.
- Modify: dictation start path (find with `grep -rn "transcribe.start_stream\|\.start_stream(" src-tauri/src/ipc/commands/dictation/`).
- Modify: `src-tauri/src/meeting/test_support.rs` and `src-tauri/src/ipc/tests.rs` — keep tests passing `NoopVad` through the builder.

### Step 1: Read how `diarize` flows through `InferenceState` and `AppStateBuilder`

```bash
grep -n "pub diarize\|diarize:\|with_diarizer\|fn build_diarizer" src-tauri/src/ipc/state.rs src-tauri/src/ipc/builder.rs 2>/dev/null | head -20
```
Mirror this exactly — that's the established seam pattern. The `vad` slot is structurally identical (no hot-swap for v1; if Silero load fails at startup we use `NoopVad` and don't reload).

### Step 2: Add `vad` field to `InferenceState`

In `src-tauri/src/ipc/state.rs`, alongside `pub diarize: Arc<dyn crate::diarization::Diarize>`:

```rust
    /// Speech-presence VAD used to gate whisper inference (#974). Mints a
    /// per-stream [`crate::vad::VadSession`] at each meeting / dictation
    /// session start. Production is [`crate::vad::onnx::SileroVad`];
    /// tests use [`crate::vad::NoopVad`]. If the bundled ONNX fails to
    /// load at startup (corrupted asset, tract incompatibility), we fall
    /// back to [`crate::vad::NoopVad`] so transcription still works — the
    /// gate is the additive nice-to-have, not load-bearing.
    pub vad: Arc<dyn crate::vad::VadModel>,
```

### Step 3: Add `with_vad` to `AppStateBuilder`

In `src-tauri/src/ipc/builder.rs`:

```rust
pub fn with_vad(mut self, vad: Arc<dyn crate::vad::VadModel>) -> Self {
    self.vad = Some(vad);
    self
}
```

And in `build()` panic if missing (matches the existing "required seam" pattern other slots use).

### Step 4: Load `SileroVad` at startup

In `src-tauri/src/lib.rs` (find `AppState::build_default` or the equivalent setup; look for where `diarize` is loaded):

```rust
let vad: Arc<dyn crate::vad::VadModel> = match crate::vad::onnx::SileroVad::load() {
    Ok(m) => {
        tracing::info!("vad: loaded SileroVad (bundled v5)");
        Arc::new(m)
    }
    Err(e) => {
        tracing::warn!(error = ?e, "vad: SileroVad load failed; falling back to NoopVad (transcription will work but won't be gated against silence-hallucinations — see #974)");
        Arc::new(crate::vad::NoopVad)
    }
};
// ... pass `vad` into AppStateBuilder::with_vad(...) ...
```

### Step 5: Update meeting + dictation start paths to mint sessions

In `src-tauri/src/meeting/lifecycle.rs` at the `start_stream` call (around the loop over sources):

```rust
match transcriber.start_stream(
    format,
    &dict_opts.vocab_prompt,
    self.vad.new_session(),  // NEW: real per-source VAD session
) {
    // ... existing arms ...
}
```

`self.vad` requires `SessionManager` to hold the `Arc<dyn VadModel>`. Add it to `SessionManager` fields + constructor + the construction site in `lib.rs::build_default`.

Same idea in the dictation start path — find the `start_stream` call and pass `state.inference.vad.new_session()`.

### Step 6: Update test builders + mocks

In `src-tauri/src/meeting/test_support.rs` and the `AppStateBuilder` test helpers in `ipc/tests.rs` / `ipc/builder.rs`, default `vad` to `Arc::new(crate::vad::NoopVad)` so existing tests don't have to be touched.

Also update the `MockTranscribe` impl(s) — its `start_stream` now takes a `Box<dyn VadSession>`, which it can just drop.

### Step 7: Run full suite

```bash
DYLD_FALLBACK_LIBRARY_PATH=... cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -8
cargo clippy --lib --no-default-features -- -D warnings 2>&1 | tail -3
cargo clippy --lib --tests --features whisper,diarization-onnx -- -D warnings 2>&1 | tail -3
cargo fmt --all
```
Expected: all pre-existing tests still pass; clippy clean.

### Step 8: Commit

```bash
git add -A
git commit -m "feat(vad): wire SileroVad through AppState into meeting + dictation (#974)"
```

---

## Task 5: Defense-in-depth — tune `FullParams`

One-line additions. Trivial.

**Files:**
- Modify: `src-tauri/src/transcription/whisper.rs` (`WhisperInferer::infer` AND the one-shot dictation transcription path, if separate)

### Step 1: Find the `FullParams::new` call sites

```bash
grep -n "FullParams::new\|set_n_threads\|set_no_context" src-tauri/src/transcription/whisper.rs
```

There are typically two: the streaming `WhisperInferer::infer` (~line 656 per earlier reads) and the one-shot transcription used by dictation (~line 323).

### Step 2: Add the two tuning calls in both places

Immediately after `set_no_context(true)` (or at the equivalent point if no_context isn't set in the one-shot path):

```rust
// #974: defense-in-depth against silence/non-speech hallucinations.
// `set_temperature(0.0)` pins greedy decoding (no fallback to sampling
// on low logprob, which is where `.com` / "Thanks for watching" type
// confabulations emerge).
// `set_suppress_nst(true)` suppresses non-speech tokens like `[Music]`
// / `[Applause]`. Both are additive and safe for real-speech decoding.
params.set_temperature(0.0);
params.set_suppress_nst(true);
```

### Step 3: Run + commit

```bash
DYLD_FALLBACK_LIBRARY_PATH=... cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -3
git add src-tauri/src/transcription/whisper.rs
git commit -m "chore(transcription): pin temperature=0 + suppress non-speech tokens (#974)"
```

---

## Task 6: Integration test + docs + dev-launch smoke

### Step 1: Add an integration test that proves the gate works end-to-end

In `src-tauri/src/ipc/tests.rs` (or a new file under `src-tauri/tests/` if you prefer end-to-end harness), drive a `WhisperStreamingSession` with a scripted `VadSession` and a recording inferer mock, and verify the gate logic at the IPC composition level. The unit tests in Task 2 cover the per-method behaviour; this test covers the composition through `AppStateBuilder + with_vad`.

```rust
#[tokio::test]
async fn streaming_session_gates_inference_when_vad_reports_silence() {
    // Mock VadModel that returns AlwaysSilenceVad sessions, plus a
    // RecordingTranscribe that counts how many times infer was called.
    // Wire both through AppStateBuilder; start a streaming session;
    // feed it many seconds of audio; verify zero infer calls landed.
}
```

The exact harness shape will mirror existing `ipc::tests` patterns. If existing tests use `MemHistory` / `AppStateBuilder`, copy that scaffolding.

### Step 2: Update `learnings.md`

Append a new entry after the latest one (currently 2026-05-28 Homebrew retirement):

```markdown
## 2026-05-28 — VAD gate in front of Whisper inference (#974)

Whisper hallucinates on silence / low-information audio with signature
training-data ghosts: `.com`, `.org`, "Thanks for watching!", repeating
phrases like "We're going to use the web." × 6. Root cause is the
streaming pump feeding every window to the decoder regardless of speech
content; given an uninformative window, Whisper falls back to high-prior
phrases from its training set (YouTube outros, tutorial templates).

Whisper.cpp has knobs that would mitigate this (`no_speech_thold`,
`logprob_thold`, built-in VAD) but **whisper-rs 0.14 doesn't expose
them** — verified from the FullParams setter list. So the gate has to
live upstream in our own code.

**Fix:** Silero VAD v5 (bundled ONNX, ~1.7MB, MIT) loaded once via
`tract-onnx` (already in the tree for wespeaker). Each streaming
transcription session mints its own `VadSession` carrying recurrent
LSTM state. `WhisperStreamingSession::feed` drains samples into the VAD
in 512-sample frames; `tick()` short-circuits inference if no frame
scored above threshold within the hangover window (default 1500ms).
Plus two FullParams tweaks we *do* have access to —
`set_temperature(0.0)` and `set_suppress_nst(true)` — as
defense-in-depth.

**Bundled vs downloaded:** chose to commit the ONNX bytes via
`include_bytes!` rather than auto-download. 1.7MB is acceptable in a
binary distributed via signed installers, and it eliminates the
first-run download UX entirely. A SHA self-check at startup catches
asset corruption under git.

**Trait shape requires the per-session split.** Silero is recurrent —
each frame's prediction depends on the prior frame's hidden state. So
the `VadModel` (shared, immutable) / `VadSession` (per-stream, mutable)
split that was deferred for the diarizer (where it was a "would be
nicer") is mandatory here. No way to make Silero stateless.

**Knobs:** `HUSH_VAD_THRESHOLD` (default 0.5), `HUSH_VAD_HANGOVER_MS`
(default 1500), `HUSH_VAD_DISABLE=1` for A/B and debug. No
user-facing setting in v1 — env vars only.
```

### Step 3: Add a short note to `ARCHITECTURE.md`

In the meeting-pump dataflow section, add a sentence: "Before each `infer()` call, `WhisperStreamingSession::tick()` checks a per-stream Silero VAD gate; windows without recent speech bypass the inferer entirely (#974, see `learnings.md` 2026-05-28)."

### Step 4: Flip the proposal status

```bash
sed -i '' 's/^\*\*Status:\*\* Draft (in implementation)/\*\*Status:\*\* Accepted (implemented)/' \
  docs/vad-hallucination-gate-proposal.md
```

### Step 5: Full suite + cross-platform clippy + smoke

```bash
DYLD_FALLBACK_LIBRARY_PATH=... cargo test --lib --features whisper,diarization-onnx 2>&1 | tail -3
cargo clippy --lib --no-default-features -- -D warnings 2>&1 | tail -3
cargo clippy --lib --tests --features whisper,diarization-onnx -- -D warnings 2>&1 | tail -3
npm run check 2>&1 | tail -3
```

**Dev-launch smoke (required per `CLAUDE.md` "Dev-launch smoke")** — this PR touches `lib.rs`'s `setup` (loading SileroVad at startup) and the `Transcribe::start_stream` signature, both startup-touching:

```bash
npm run tauri dev
```
Verify the app boots without panic and a meeting session runs (auto-detect or manual) and produces transcripts. If you're a subagent without an interactive runtime, surface this as an explicit "MANUAL SMOKE REQUIRED BEFORE MERGE" line in the PR description so Ken handles it.

### Step 6: Commit + push + PR

```bash
git add -A
git commit -m "test+docs(vad): integration coverage + learnings + ARCHITECTURE note (#974)"
git push -u origin feat/vad-hallucination-gate
gh pr create --base main --title "feat(vad): Silero VAD gate to stop Whisper silence-hallucinations (#974)" --body-file - <<EOF
Fixes #974.

## Summary

Whisper was emitting signature hallucinations on silent / non-speech windows
(\`.com\`, "Thanks for watching!", repeating loops). This adds a Silero VAD
gate in front of \`WhisperInferer::infer\` — inference is skipped on windows
that don't contain recent speech (with a 1500ms hangover so trailing words
aren't clipped). Plus two FullParams tweaks (\`set_temperature(0.0)\`,
\`set_suppress_nst(true)\`) as defense-in-depth.

## What's in it

- New \`vad\` module with \`VadModel\`/\`VadSession\` trait pair + \`NoopVad\`.
- \`SileroVad\` impl via \`tract-onnx\` (no new heavy deps).
- Silero v5.1.2 ONNX bundled in \`src-tauri/assets/silero_vad.onnx\` (~1.7MB,
  MIT). No auto-download — \`include_bytes!\` keeps the install path simple.
- Gate logic in \`WhisperStreamingSession::tick()\`.
- Plumbing through \`InferenceState\` / \`AppStateBuilder\` into meeting +
  dictation start paths.
- Env knobs: \`HUSH_VAD_THRESHOLD\`, \`HUSH_VAD_HANGOVER_MS\`, \`HUSH_VAD_DISABLE\`.

## Tested

- [x] \`cargo test --lib --features whisper,diarization-onnx\` — all pass
- [x] \`cargo clippy --lib --no-default-features -- -D warnings\` — clean
- [x] \`cargo clippy --lib --tests --features whisper,diarization-onnx -- -D warnings\` — clean
- [ ] **MANUAL SMOKE REQUIRED**: \`npm run tauri dev\` — start a meeting,
      confirm Stop still works, confirm a transcript with real speech is
      produced, eyeball that the \`.com\` / "Thanks for watching" cluster
      is gone on a stretch of silence (the original reproduction Ken
      pasted in the conversation). Touches startup + the
      \`Transcribe::start_stream\` signature → per CLAUDE.md "Dev-launch
      smoke" this is required before merge.
EOF
```

### Step 7: Watch CI; merge if green

Per Ken's standing PR-merge autonomy on routine CI-green PRs, once the manual smoke is checked off (step 6) and all 8 CI checks pass, squash-merge + delete the branch.

```bash
gh pr checks <PR#> --watch
gh pr merge <PR#> --squash --delete-branch
git checkout main && git pull --ff-only
```

After merge, delete the proposal + plan docs (matches the bg-finalization cleanup pattern — they served their purpose; rationale and resume guides live in \`learnings.md\` per CLAUDE.md convention):

```bash
git checkout -b chore/drop-vad-design-docs
git rm docs/vad-hallucination-gate-proposal.md docs/vad-hallucination-gate-plan.md
git commit -m "docs(vad): drop design docs post-merge (rationale lives in learnings.md)"
git push -u origin chore/drop-vad-design-docs
gh pr create --fill
```

---

## Self-review notes (author)

**Spec coverage:**
- Goal 1 (eliminate hallucinations on silence) → Tasks 2, 3, 4 together (gate + real model + wired through).
- Goal 2 (preserve real speech via hangover) → Task 2 (`HUSH_VAD_HANGOVER_MS` + the gate predicate).
- Goal 3 (meeting + dictation uniformly) → Task 4 (both call sites updated).
- Defense-in-depth FullParams → Task 5.
- Bundled model decision → Task 3.
- Env-var knobs → Task 2.
- Trait split (`VadModel`/`VadSession`) → Task 1.
- Risk: tract opset compatibility → called out in Task 3 Step 5 with explicit fallback to v4 or escalation.

**Placeholders:** the Task 2 Step 3 `todo!()` markers are intentional inline directives for the implementer (each has a one-sentence assertion spec; the harness exists in the surrounding `mod tests`), but they MUST be filled in before Step 4 runs the tests. Flagged in the listing.

**Type consistency:** `Box<dyn VadSession>` everywhere as the per-stream handle; `Arc<dyn VadModel>` everywhere as the slot. `FRAME_LEN_SAMPLES` and `SAMPLE_RATE_HZ` constants used across both `vad/mod.rs` and `vad/onnx.rs` and the gate. `score_frame` returns `Result<f32>` consistently. `vad_config_from_env()` is the single source of truth for the three env-var defaults.

**Known soft spots:**
1. The exact tract API call for typed inference with multiple inputs may need minor adjustment from the listed code — tract's idiomatic version varies between releases. Implementer should verify against the actual `tract-onnx` version in `Cargo.lock` and adjust the `model.run(tvec!(...))` shape if needed. Anchored to verified Silero I/O signature; not a placeholder.
2. The exact `start_stream` call sites in dictation may have slightly different shapes than meeting; implementer reads the actual code rather than copying mine verbatim. Plan calls this out in Task 2 Step 8 + Task 4 Step 5.

**Scope:** focused — single subsystem (VAD gate), single PR, single merge. No coupled refactors.
