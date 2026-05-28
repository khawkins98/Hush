# VAD gate to stop Whisper hallucinations on silence — design proposal

**Status:** Accepted (implemented) · **Date:** 2026-05-28 · **Author:** Ken + Claude · **Issue:** #974

## Problem

Whisper hallucinates on silence and low-information non-speech audio. Real-world
meeting transcripts show signature artefacts:

- `.com` / `.org` standalone utterances on silent windows
- "Thanks for watching!" / "We're going to use the web." / "I'm going to show you how to do it." — high-prior YouTube/tutorial endings from training data
- Looping degenerate decodes ("We're going to use the web." × 6) on low-amplitude
  non-speech (keyboard, breath, room hum)
- Foreign-language token bursts (`ら`, mixed CJK) on silence
- Phantom diarizer clusters (e.g. "Speaker 1") that only ever produce
  hallucinated text — the diarizer separates the garbage audio acoustically and
  parks it in its own cluster

The contrast is diagnostic: where Whisper correctly identifies silence it emits
`[BLANK_AUDIO]`; where it fails, it emits these training-data ghosts. Both occur
in the same transcript.

## Root cause

The streaming pump (`WhisperStreamingSession::drain()`) feeds every window of audio
to `WhisperInferer::infer()` regardless of whether that audio contains speech.
When the input is uninformative, Whisper's decoder gravitates to high-prior
phrases from its training set.

Whisper.cpp itself has knobs (`no_speech_thold`, `logprob_thold`, built-in VAD)
that would partially mitigate this, but **whisper-rs 0.14 does not expose them**
(verified: `params.rs` setter list omits both, and there's no `vad` module).
Bumping whisper-rs is out of scope for v1 (separate decision; #974 doesn't gate
on it). The fix has to be upstream of `infer()`, in our own code.

## Goals (v1)

1. **Eliminate hallucinated utterances on silent / non-speech windows.** Concretely:
   the `.com` / "Thanks for watching" / repeating-phrase patterns Ken pasted from
   a real meeting should not appear in future transcripts on the same audio.
2. **Preserve real speech, including soft onsets and trailing-off words.** A
   hangover after the last detected speech keeps inference live long enough to
   capture conversational tails.
3. **Apply the VAD gate to meeting; tune `FullParams` for both paths.** The
   gate sits in front of the streaming `infer()` call, so only the meeting
   path receives the structural protection. Dictation is one-shot
   (`Transcribe::transcribe_chunks` on the full PTT buffer) — it benefits
   from the Task 5 `FullParams` tuning (`set_temperature(0.0)` +
   `set_suppress_nst(true)`) as defense-in-depth, but doesn't get
   speech-presence gating. No reported dictation hallucinations in the
   issue corpus; if dogfood surfaces them, a small input-trim pass before
   `run_inference()` is a ~30-line follow-up (the VAD model is already
   loaded by Task 4 and the slot is wired through `InferenceState`).

## Non-goals (v1)

- Bumping whisper-rs to expose more native knobs.
- User-facing UI settings for thresholds (env vars only).
- Per-app or per-meeting-source threshold profiles.
- Replacing the existing diarizer or transcription pipeline.

## Approved decisions

- **Algorithm:** **Silero VAD** via `tract-onnx`. Best accuracy of the realistic
  options; tract is already in the tree for the wespeaker diarizer; ~1.28 MB ONNX
  model bundled at compile time via `include_bytes!`.
- **Gate point:** in `WhisperStreamingSession::drain()`, just before
  `state.tick(inferer)`. The window/timestamps stay aligned (audio is still fed);
  only the *inference call* is gated.
- **Defense-in-depth:** also tune the FullParams whisper-rs 0.14 does expose —
  `set_temperature(0.0)` and `set_suppress_nst(true)` — as a free belt-and-braces.

## Architecture

### A. Trait seam — split heavy model from per-session state

Silero VAD is a **recurrent** model: each frame's prediction depends on the
previous frame's hidden state. So per-stream state is mandatory — unlike the
diarizer split (which was deferred), VAD has no single-instance shortcut. The
trait pair is:

```rust
/// Heavy, immutable, shared. Loads the ONNX model once.
pub trait VadModel: Send + Sync {
    /// Mint a fresh per-stream session with its own LSTM hidden state.
    fn new_session(&self) -> Box<dyn VadSession>;
}

/// Per-stream. Carries the recurrent state across `score_frame` calls.
pub trait VadSession: Send {
    /// Speech probability ∈ [0,1] for one Silero-sized frame at 16kHz mono
    /// (Silero v5 expects 512 samples / 32ms; the impl exposes the required
    /// frame size as a constant so the caller can chunk correctly).
    /// Updates internal state; calls must be in temporal order on the same session.
    fn score_frame(&mut self, frame_16k_mono: &[f32]) -> f32;
}
```

Concrete impls:

- **`SileroVad`** (production): wraps a `tract_onnx::TypedRunnableModel`. Loaded
  once at startup into a shared `Arc<dyn VadModel>`. `new_session()` returns a
  `SileroVadSession` with fresh zero-initialised LSTM state tensors.
- **`NoopVad`** / **`NoopVadSession`**: `score_frame` always returns `1.0`
  (always speech → no gating). Used when the model file is absent and as the
  baseline for tests that aren't exercising the gate.

Production wires the live `VadModel` through `AppState` and into `PumpContext`
the same way `Diarize` flows today. The `StreamingTranscribeSession` holds a
`Box<dyn VadSession>` minted at session start (dictation is one-shot, not a
streaming session, and doesn't use a `VadSession`).

### B. Gate logic in `WhisperStreamingSession::drain()`

Per-drain state on the session:

```text
FRAME_LEN: u32   = SileroVad::FRAME_LEN_SAMPLES;   // Silero-mandated (512 @ 16kHz)
VAD_THRESHOLD    = 0.5                              // env: HUSH_VAD_THRESHOLD
HANGOVER_MS      = 1500                             // env: HUSH_VAD_HANGOVER_MS

on each drain, before calling state.tick(inferer):
  for each FRAME_LEN-sample frame f in the *newly-fed* samples since last drain:
      prob = vad_session.score_frame(f)
      if prob >= VAD_THRESHOLD:
          last_speech_ms = wall_clock_ms()
  if (wall_clock_ms() - last_speech_ms) > HANGOVER_MS:
      return Ok(vec![])      // skip infer — no segments → no partials → no hallucinations
  else:
      return state.tick(inferer)
```

- The window itself isn't truncated; audio still accumulates. When speech
  resumes after a silent stretch, the next allowed infer sees the full
  in-window history (Whisper has its onset context).
- Skipping `infer()` returns `Vec<StreamSegment>` empty — the streaming policy
  handles "no segments" as "nothing to commit this drain" today, so no
  downstream changes are needed.
- **Per-source independence**: each `StreamingTranscribeSession` has its own
  `VadSession` and its own `last_speech_ms`. Meeting mic and system-audio gate
  independently — mic silence doesn't gate system-audio inference.

### C. Defense-in-depth — tune the FullParams we have

In `WhisperInferer::infer()` (and the one-shot dictation path), add to the
existing `FullParams` setup:

```rust
params.set_temperature(0.0);     // no sampling fallback (greedy already, but pin it)
params.set_suppress_nst(true);   // suppress non-speech tokens ([Music], [Applause], ...)
```

These are one-line additions that target the specific token classes Whisper
hallucinates. Safe and additive — they don't affect real-speech decoding.

### D. Model bundling — `include_bytes!` of a 16kHz-specialized derived artifact

The bundled Silero v5 ONNX is committed to `src-tauri/assets/silero_vad.onnx`
(~1.28 MB) and loaded at compile time via `include_bytes!`. There is no
first-run download — the model is always present.

The committed artifact is a **derived** model, not the upstream v5.1.2
release. tract-onnx 0.22.1 strict-analyses both branches of ONNX `If` ops,
and Silero v5 wraps its inference in `If(sr == 16000) { ... } else { ... }`
whose dead 8kHz branch has shape-incompatible ops against the inputs we
pin for the 16kHz path. `scripts/build-silero-vad-onnx.py` downloads the
upstream release, substitutes `sr` with a constant initializer, then
iteratively splices nested `If` branches (six of them inside the LSTM
dispatch + decoder) by probing each `If`'s condition via onnxruntime.
The resulting model has the `If`s constant-folded away and loads cleanly
in tract. See `src-tauri/assets/README.md` for the reproduction recipe.

A SHA256 self-check at startup verifies the in-memory bytes match a pinned
constant — catches asset corruption (or "I forgot to bump the SHA after
re-deriving") loud and early.

**Fallback if the bundled model fails to load**: runtime falls back to
`NoopVad` (`ipc/state.rs::build_vad`); transcription continues to work
just without the gate. Logged at WARN.

### E. Cargo feature flag

Tract-onnx is already a runtime dependency via `diarization-onnx`, so adding
VAD doesn't pull anything new. Keep the surface simple: **no separate `vad`
Cargo feature**. The VAD code compiles unconditionally; if the model file is
absent at runtime, `NoopVad` is used. (This mirrors how the diarizer
gracefully no-ops when its model isn't present.)

## Config & toggles

Env vars only — no user-facing settings in v1. Convention follows
`HUSH_DIARIZER_THRESHOLD`:

| Env var | Default | Effect |
|---|---|---|
| `HUSH_VAD_THRESHOLD` | `0.5` | Silero speech-probability threshold per frame |
| `HUSH_VAD_HANGOVER_MS` | `1500` | ms after last detected speech to keep inferring |
| `HUSH_VAD_DISABLE` | unset | If `=1`, force `NoopVad` everywhere (debug / A/B) |

## Edge cases & failure modes

- **Model missing at startup:** `NoopVad` used; INFO log notes "VAD disabled —
  model not present"; tracked alongside the wespeaker missing-model warning.
- **Tract can't load the ONNX:** same fallback as missing model. WARN log with
  the tract error.
- **Soft-speech clipping risk:** the 1500ms hangover catches the common "I…"
  hesitation pattern. Threshold can be lowered via env var if real speech is
  being missed during tuning.
- **First tick of a new session:** `last_speech_ms` initialised to "now" so the
  first window is always inferred (no false silent-start). Whisper's own
  onset behavior takes over.
- **Long monologues (no pause):** every frame above threshold → `last_speech_ms`
  keeps updating → never gates. Correct behavior.
- **Background music / TV in the call:** Silero may classify as speech →
  hallucinations still possible on those segments. v1 acceptable; revisit if
  prevalent.

## Risks & open uncertainties

1. **Tract-onnx may not support Silero VAD's exact ONNX opset** (Silero v5 uses
   LSTM ops; tract-onnx's coverage is broad but not 100%). Mitigation: verified
   during implementation; if loading fails, fall back to Silero v4 or to a
   simpler conv-only ONNX VAD. Documented in implementation notes.
2. **Latency budget per tick.** Silero on 32ms frames × ~15 frames per 500ms
   tick ≈ ~5-30ms total tract inference per tick. Well within the 500ms tick
   budget on M1; verify on slower hardware via the existing perf logs.
3. **Threshold tuning is empirical.** Default 0.5 is the Silero-recommended
   midpoint; may need adjustment after Ken's first dogfood. Env vars exist
   precisely so tuning doesn't require a release.

## Testing strategy

- **Unit (`vad/onnx.rs`)**: load the model, score a known-silent and a
  known-speech frame, assert probability ordering.
- **Unit (`transcription/streaming.rs`)**: scripted `VadSession` mock returning
  programmed probabilities; assert `drain()` skips `infer()` exactly when the
  gate predicts skip; assert hangover boundary behavior.
- **Integration (`meeting_fixture.rs`)**: feed a fixture containing alternating
  speech + ≥3s silence; assert that hallucinated tokens (`.com`, "Thanks for
  watching", etc.) do **not** appear in the final transcript while real speech
  utterances are preserved.
- **Frontend**: no UI surface changes; no frontend tests.
- **`npm run tauri dev` smoke**: required before merge (touches startup model
  loading + runtime inference paths per CLAUDE.md "Dev-launch smoke").

## Out of scope (v2+ ideas, not in this PR)

- Bumping whisper-rs to a version that exposes `no_speech_thold`/`logprob_thold`
  natively for further defense-in-depth.
- Per-app / per-meeting-source threshold profiles.
- A settings-UI knob for the threshold (env-var-only in v1).
- Pre-roll buffering for sub-30ms onsets (the existing 30s window already
  preserves onset context for any natural speech).
