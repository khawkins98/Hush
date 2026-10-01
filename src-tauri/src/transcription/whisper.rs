//! `whisper-rs` backed implementation of the [`Transcribe`] trait.
//!
//! Concept inspired by VoiceInk's whisper.cpp Swift bridge. Reimplemented
//! from observed public behaviour; no source code referenced. See §13.8 of
//! the PRD.
//!
//! Gated behind the `whisper` Cargo feature because `whisper-rs` pulls in
//! whisper.cpp via `cmake`. Default builds do not require any C++ toolchain;
//! enabling this module is opt-in (CI installs cmake explicitly, contributors
//! who only touch the Rust side can ignore it).
//!
//! ## Why one shared `WhisperContext` per loaded model
//!
//! Loading a GGUF file is the most expensive thing whisper.cpp does — order
//! of seconds for `base`, tens of seconds for `large-v3`. It is also the
//! biggest memory cost: whisper.cpp `read`s every weight tensor into a
//! private host buffer (it does NOT mmap the file), so each loaded context
//! is a full private copy of the weights. We therefore load the model once
//! and share the context ([`ContextHandle`]) between the dictation and
//! meeting transcribers via [`WhisperTranscription::share_context`].
//!
//! ## Threading
//!
//! whisper-rs loads the context with `whisper_init_*_no_state`, so the
//! context holds only the immutable weights + vocab; every mutable buffer
//! (KV cache, mel, compute scratch, results) lives in a `WhisperState`.
//! `whisper_full_with_state` only reads the context, which is why
//! whisper.cpp's own `whisper_full_parallel` runs one state per thread on a
//! single shared context. So two inferences on separate states may run
//! concurrently against one context. What we still serialise is inference
//! *per transcriber* (the [`ContextHandle`] gate) — a scheduling choice,
//! not a safety one: it keeps a meeting's mic and system-audio sessions
//! taking turns exactly as they did before the context was shared, while
//! dictation (a separate transcriber with its own gate) never waits on the
//! meeting pump (#248). Inference uses the worker pool whisper.cpp manages
//! internally (`set_n_threads`); we default to a conservative value rather
//! than spawning per-core threads, because dictation runs in the foreground
//! and we don't want to starve the UI thread on small machines.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::audio::{apply_mic_gain, downmix_to_mono, CaptureFormat, CapturedAudio};
use crate::transcription::quality::{aggregate_token_scores, TokenScore};
use crate::transcription::resample::resample_to_mono;
use crate::transcription::streaming::{
    SlidingWindowConfig, SlidingWindowState, StreamSegment, StreamingTranscribeSession,
    WhisperLikeInferer,
};
use crate::transcription::{ProgressHookSlot, Transcribe, Utterance, WHISPER_SAMPLE_RATE};

/// Default thread count for whisper.cpp inference.
///
/// Whisper.cpp scales roughly linearly up to ~4 threads on Apple Silicon and
/// modern x86; beyond that the gains are small and the contention with the
/// UI thread starts to bite. 4 is the cross-platform default. Users who want
/// more (or less, for battery life) flip the slider in Settings → General
/// (#255). The atomic field on [`WhisperTranscription`] holds the live
/// value; the IPC layer's `set_inference_threads` writes through it so
/// changes take effect on the next inference call without a model reload.
pub const DEFAULT_INFERENCE_THREADS: i32 = 4;

/// Lower bound on the inference thread count. Whisper requires at least
/// one thread to make progress; the slider in Settings → General is also
/// clamped to this floor.
pub const MIN_INFERENCE_THREADS: i32 = 1;

/// Upper bound on the inference thread count. Beyond this, the gains
/// from extra threads are dwarfed by their contention overhead — even
/// on 16-core machines whisper rarely benefits past ~12 threads. Picked
/// to match what `Settings → General` exposes; the atomic is `clamp`'d
/// here at write-time so a malformed settings row can't push past it.
pub const MAX_INFERENCE_THREADS: i32 = 16;

/// Number of `whisper_full` calls a single `WhisperState` is reused
/// for in streaming mode before it gets dropped and recreated (#612
/// second-pass fix).
///
/// **Why this isn't infinite:** the state-reuse fix from #615 stopped
/// the catastrophic per-state-init leak (53 GB → 3.5 GB on a 5-min
/// meeting), but real-session profiling on 2026-05-07 showed RSS still
/// climbing at ~2 GB/min on a two-source meeting (~38 inferences/min,
/// ~44 MB allocated and not returned per `whisper_full`). whisper.cpp's
/// pure-CPU code path appears to do scratch allocations within
/// `whisper_full` that don't return to the heap even when the state is
/// long-lived. Periodically dropping the state forces those
/// allocations free; the next call's lazy-init pays the ~76 MB
/// recreate cost once. Net: bounded RSS instead of unbounded.
///
/// **Why 30:** at our ~3 s inference cadence, 30 calls ≈ 90 s of
/// speech per source. We pay 76 MB recreate + ~30 × 44 MB pre-recreate
/// ≈ 1.4 GB peak between recreations, then drop back down. With one
/// recreation per 90 s, peak/floor ratio stays small enough that the
/// user experience is "RSS hovers" instead of "RSS climbs forever."
/// The 76 MB recreate cost amortises over 30 calls so the per-call
/// overhead is ~2.5 MB — negligible compared with the ~80 MB working
/// set of the inference itself.
///
/// Tunable via `HUSH_WHISPER_STATE_RECREATE_INTERVAL` env var on
/// startup (read once into the const-lookalike `state_recreate_interval`
/// helper below) so we can A/B without rebuilding.
pub const DEFAULT_STATE_RECREATE_INTERVAL: u64 = 30;

/// VAD speech-probability threshold (#974). Frames at or above this score
/// count as "speech" and update `last_speech_at`. Tunable at runtime via
/// `HUSH_VAD_THRESHOLD`.
const DEFAULT_VAD_THRESHOLD: f32 = 0.5;

/// Hangover after the last detected speech frame before `drain()` starts
/// skipping inference (#974). Catches the "I…" hesitation pattern; tunable
/// via `HUSH_VAD_HANGOVER_MS`.
const DEFAULT_VAD_HANGOVER_MS: u64 = 1500;

/// whisper.cpp's `no_speech_thold` (#974 follow-up). A decoded segment is
/// discarded as silence when its no-speech token probability exceeds this
/// AND its average logprob is below `logprob_thold`. **Lower = more
/// aggressive** silence filtering. whisper.cpp's built-in default is
/// 0.6; we set it explicitly (same value, no behavior change) so it's a
/// single tunable knob via `HUSH_WHISPER_NO_SPEECH_THOLD` — the value to
/// lower once a meeting's `HUSH_VAD_TRACE` evidence shows where the
/// compressed-call-audio hallucinations sit. See learnings.md
/// "2026-06-05 VAD hallucination follow-up".
const DEFAULT_NO_SPEECH_THOLD: f32 = 0.6;

/// whisper.cpp's `logprob_thold` (#1013). whisper.cpp's no-speech drop
/// is an **AND**: a segment is discarded as silence only when its
/// no-speech probability exceeds `no_speech_thold` *and* the decode's
/// average logprob is below this value. A confident hallucination
/// (high avg logprob on a silent window) therefore survives any
/// `no_speech_thold` setting — lowering `HUSH_WHISPER_NO_SPEECH_THOLD`
/// alone can't catch it. Raising this (towards 0) widens the silence
/// drop. whisper.cpp's default is -1.0; we set it explicitly (same
/// value, behaviour-neutral) so it's tunable via
/// `HUSH_WHISPER_LOGPROB_THOLD`. With `temperature_inc = 0` there is no
/// fallback ladder, so this knob affects only the no-speech decision.
///
/// The learnings.md entry from #974 claimed whisper-rs 0.14 doesn't
/// expose `logprob_thold`; that was wrong — `FullParams::set_logprob_thold`
/// and `WhisperState::full_get_token_data` both exist in 0.14.4.
const DEFAULT_LOGPROB_THOLD: f32 = -1.0;

/// Resolve [`DEFAULT_LOGPROB_THOLD`] against `HUSH_WHISPER_LOGPROB_THOLD`,
/// clamped to `[-10.0, 0.0]` (logprobs are never positive).
fn logprob_thold() -> f32 {
    std::env::var("HUSH_WHISPER_LOGPROB_THOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(DEFAULT_LOGPROB_THOLD)
        .clamp(-10.0, 0.0)
}

/// Encoder positions in every stock Whisper model (`n_audio_ctx`): 30 s of
/// audio. whisper.cpp rejects an `audio_ctx` above this with error -5.
const WHISPER_FULL_AUDIO_CTX: i32 = 1500;

/// 16 kHz samples per encoder position: mel hop is 160 samples (10 ms)
/// and the encoder's second conv has stride 2, so one position is 20 ms.
const SAMPLES_PER_AUDIO_CTX_FRAME: usize = 320;

/// Extra encoder positions past the end of the input when
/// `HUSH_WHISPER_AUDIO_CTX=1`. The decoder predicts timestamps against the
/// encoded window, and whisper.cpp's own mel already ends in zero padding,
/// so a little trailing silence keeps the last word away from the edge.
/// 64 positions = 1.28 s. Tunable via `HUSH_WHISPER_AUDIO_CTX_MARGIN`.
const DEFAULT_AUDIO_CTX_MARGIN: i32 = 64;

/// Smallest `audio_ctx` the dynamic sizing will use. Stock models were
/// only trained on 1500-position windows, and tiny windows are where
/// errors appear: with no floor, small-q8_0 turned "Send it to Sarah."
/// into "Sended to Sarah.". 384 positions (7.68 s) was the most
/// conservative value with no one-shot regression in the A/B. Tunable
/// via `HUSH_WHISPER_AUDIO_CTX_FLOOR`. See learnings.md 2026-10-01
/// "Encoder audio_ctx sizing".
const DEFAULT_AUDIO_CTX_FLOOR: i32 = 384;

/// Encoder context for an input of `n_samples` 16 kHz samples: enough
/// positions to cover the whole input, plus `margin`, no smaller than
/// `floor` and never above the model's 1500.
///
/// The covering requirement is load-bearing, not just accuracy:
/// whisper.cpp's decode loop advances `seek` by the last timestamp it
/// saw, and when the encoder window ends before the audio does it will
/// re-encode from the new seek, so a too-small window turns one encode
/// into several and lets a segment be cut at the window edge. Covering
/// the input keeps every call a single encode, as it is today.
///
/// No alignment is needed: whisper.cpp pads the cross-attention KV to a
/// multiple of 256 internally (`GGML_PAD(n_ctx, 256)`), and the conv
/// front end accepts any width.
fn dynamic_audio_ctx(n_samples: usize, margin: i32, floor: i32) -> i32 {
    let covering = n_samples.div_ceil(SAMPLES_PER_AUDIO_CTX_FRAME);
    let covering = i32::try_from(covering).unwrap_or(WHISPER_FULL_AUDIO_CTX);
    covering
        .saturating_add(margin.max(0))
        .max(floor)
        .clamp(1, WHISPER_FULL_AUDIO_CTX)
}

/// `audio_ctx` to hand `FullParams::set_audio_ctx` given the raw env
/// values. `0` is whisper.cpp's "use the model's n_audio_ctx" sentinel,
/// so the off path is byte-for-byte today's behaviour. Only `"1"` turns
/// the sizing on.
///
/// Off by default because the A/B (learnings.md 2026-10-01) found that
/// with a shrunk window large-v3-turbo emits timestamps past the end of
/// the audio, which the streaming slide trusts, so words get dropped. It
/// also loops on trailing silence. The 2–5× CPU saving is real but
/// costs timestamp integrity.
fn resolve_audio_ctx(
    n_samples: usize,
    enabled: Option<&str>,
    margin: Option<&str>,
    floor: Option<&str>,
) -> i32 {
    if enabled.map(str::trim) != Some("1") {
        return 0;
    }
    let parse = |v: Option<&str>, default: i32| {
        v.and_then(|s| s.trim().parse::<i32>().ok())
            .map(|n| n.clamp(0, WHISPER_FULL_AUDIO_CTX))
            .unwrap_or(default)
    };
    dynamic_audio_ctx(
        n_samples,
        parse(margin, DEFAULT_AUDIO_CTX_MARGIN),
        parse(floor, DEFAULT_AUDIO_CTX_FLOOR),
    )
}

/// Env-reading wrapper around [`resolve_audio_ctx`]. Read per inference,
/// like the other `HUSH_WHISPER_*` decode knobs, so an A/B needs no
/// restart. Logged at DEBUG per call (with the input length) so a
/// meeting log shows what the encoder was actually given.
fn audio_ctx_for(n_samples: usize) -> i32 {
    let enabled = std::env::var("HUSH_WHISPER_AUDIO_CTX").ok();
    let margin = std::env::var("HUSH_WHISPER_AUDIO_CTX_MARGIN").ok();
    let floor = std::env::var("HUSH_WHISPER_AUDIO_CTX_FLOOR").ok();
    let ctx = resolve_audio_ctx(
        n_samples,
        enabled.as_deref(),
        margin.as_deref(),
        floor.as_deref(),
    );
    if ctx > 0 {
        // Once at INFO so a default-level meeting log records that the
        // experiment was on; per-call detail stays at DEBUG.
        static ANNOUNCED: std::sync::Once = std::sync::Once::new();
        ANNOUNCED.call_once(|| {
            tracing::info!(
                margin = margin.as_deref().unwrap_or("default"),
                floor = floor.as_deref().unwrap_or("default"),
                "whisper: encoder audio_ctx sizing enabled (HUSH_WHISPER_AUDIO_CTX=1)"
            );
        });
        tracing::debug!(
            audio_ctx = ctx,
            input_ms = n_samples / 16,
            "whisper: dynamic encoder audio_ctx (HUSH_WHISPER_AUDIO_CTX=1)"
        );
    }
    ctx
}

/// Read the text tokens of one decoded segment as [`TokenScore`]s.
/// Special tokens (timestamps, `<|endoftext|>`, language tags) all have
/// ids `>= token_eot` in whisper's vocabulary layout and are skipped:
/// their probabilities describe timing / control decisions, not words.
/// Errors on individual tokens are skipped rather than failing the
/// inference — confidence is diagnostic, the text is what matters.
fn segment_token_scores(state: &WhisperState, segment: i32, eot: i32) -> Vec<TokenScore> {
    let n = state.full_n_tokens(segment).unwrap_or(0);
    let mut out = Vec::with_capacity(n.max(0) as usize);
    for t in 0..n {
        let Ok(data) = state.full_get_token_data(segment, t) else {
            continue;
        };
        if data.id >= eot {
            continue;
        }
        let Ok(text) = state.full_get_token_text_lossy(segment, t) else {
            continue;
        };
        out.push(TokenScore {
            text,
            plog: data.plog,
        });
    }
    out
}

/// Resolve [`DEFAULT_NO_SPEECH_THOLD`] against `HUSH_WHISPER_NO_SPEECH_THOLD`,
/// clamped to `[0.0, 1.0]`. Read per inference (cheap env read; matches the
/// runtime-tunable convention of the VAD knobs).
fn no_speech_thold() -> f32 {
    std::env::var("HUSH_WHISPER_NO_SPEECH_THOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(DEFAULT_NO_SPEECH_THOLD)
        .clamp(0.0, 1.0)
}

/// Whether per-decision VAD-gate tracing is enabled (`HUSH_VAD_TRACE=1`).
/// When on, `drain_vad` logs the max Silero probability per feed and
/// `drain` logs each gate suppress/allow/flush decision at INFO — so a
/// real meeting's decisions land in the default (INFO-level) file log
/// without the operator having to set `RUST_LOG=debug` through launchd.
/// Off by default (no per-tick log spam in normal use).
fn vad_trace_enabled() -> bool {
    matches!(std::env::var("HUSH_VAD_TRACE").as_deref(), Ok("1"))
}

/// VAD-boundary windowing (#1013, opt-in): silence after speech that
/// closes a speech region and triggers a boundary commit. Shorter than
/// the 1.5 s gate hangover so the region commits while the gate is still
/// open. Tunable via `HUSH_VAD_BOUNDARY_SILENCE_MS`.
const DEFAULT_VAD_BOUNDARY_SILENCE_MS: u64 = 600;
/// Audio kept after the last speech frame when committing a region, so
/// trailing consonants aren't clipped.
const VAD_BOUNDARY_TAIL_PAD_MS: u64 = 200;
/// Audio kept before a speech onset when trimming leading silence.
const VAD_BOUNDARY_LEAD_PAD_MS: u64 = 300;
/// A speech region needs at least this many speech frames (~160 ms) to
/// earn a boundary commit; shorter blips (a click, a cough) are left to
/// the normal time-based path.
const VAD_BOUNDARY_MIN_SPEECH_FRAMES: u32 = 5;

/// `HUSH_VAD_BOUNDARY=1` → `Some(silence_frames)`; off otherwise.
/// Read once per session (same freeze-at-construction rule as the other
/// VAD knobs).
/// VAD-boundary windowing is on by default (promoted from opt-in after
/// the #1013 fixture showed finals landing in ~1.8 s vs ~5.5 s with no
/// junk); `HUSH_VAD_BOUNDARY=0` (or `false`/`off`) restores the purely
/// time-based windows for A/B comparison.
fn vad_boundary_enabled(raw: Option<&str>) -> bool {
    !matches!(raw.map(str::trim), Some("0" | "false" | "off"))
}

fn vad_boundary_config_from_env() -> Option<u64> {
    if !vad_boundary_enabled(std::env::var("HUSH_VAD_BOUNDARY").ok().as_deref()) {
        return None;
    }
    let ms = std::env::var("HUSH_VAD_BOUNDARY_SILENCE_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_VAD_BOUNDARY_SILENCE_MS)
        .max(100);
    Some(ms_to_vad_frames(ms))
}

fn ms_to_vad_frames(ms: u64) -> u64 {
    (ms * u64::from(WHISPER_SAMPLE_RATE) / 1000).div_ceil(crate::vad::FRAME_LEN_SAMPLES as u64)
}

fn vad_frames_to_ms(frames: u64) -> u64 {
    frames * crate::vad::FRAME_LEN_SAMPLES as u64 * 1000 / u64::from(WHISPER_SAMPLE_RATE)
}

/// Speech-region tracker for VAD-boundary windowing (#1013). Fed one
/// speech/non-speech decision per VAD frame, in order; emits the
/// absolute ms of a region's end once enough silence follows it, and
/// the ms of each speech onset (for trimming leading silence). Pure
/// bookkeeping so the policy is unit-testable without a model.
#[derive(Debug, Default)]
struct BoundaryTracker {
    /// Silence frames that close a region.
    silence_frames: u64,
    /// Index of the next frame to be observed (frame `i` covers samples
    /// `[i*512, (i+1)*512)` of the session stream).
    next_frame: u64,
    /// First frame of the open speech region, if any.
    region_start: Option<u64>,
    /// Speech frames seen in the open region.
    region_speech_frames: u32,
    /// Last speech frame seen in the open region.
    last_speech: Option<u64>,
    /// Any speech frame seen since the last emitted boundary (or session
    /// start). An onset only licenses a leading-silence trim when this
    /// is false: if a short blip — below the region minimum, so it never
    /// produced a boundary — is still uncommitted in the window, trimming
    /// up to the next onset would silently discard it. (The first
    /// real-audio run lost "ask not" from the JFK clip exactly this way.)
    speech_since_boundary: bool,
}

/// What [`BoundaryTracker::observe`] saw on one frame.
#[derive(Debug, PartialEq, Eq)]
enum BoundaryEvent {
    /// A region opened at this ms (speech onset after a pause).
    Onset(u64),
    /// A region closed; commit through this ms.
    Boundary(u64),
}

impl BoundaryTracker {
    /// Whether any speech arrived after the last emitted boundary — i.e.
    /// whether the window may still hold uncommitted speech.
    fn has_uncommitted_speech(&self) -> bool {
        self.speech_since_boundary
    }

    fn new(silence_frames: u64) -> Self {
        Self {
            silence_frames,
            ..Self::default()
        }
    }

    fn observe(&mut self, is_speech: bool) -> Option<BoundaryEvent> {
        let f = self.next_frame;
        self.next_frame += 1;
        if is_speech {
            let opens_region = self.region_start.is_none();
            if opens_region {
                self.region_start = Some(f);
                self.region_speech_frames = 0;
            }
            self.region_speech_frames += 1;
            self.last_speech = Some(f);
            let trim_ok = opens_region && !self.speech_since_boundary;
            self.speech_since_boundary = true;
            return trim_ok.then(|| BoundaryEvent::Onset(vad_frames_to_ms(f)));
        }
        let (Some(_), Some(last)) = (self.region_start, self.last_speech) else {
            return None;
        };
        if f - last < self.silence_frames {
            return None;
        }
        let long_enough = self.region_speech_frames >= VAD_BOUNDARY_MIN_SPEECH_FRAMES;
        self.region_start = None;
        self.region_speech_frames = 0;
        self.last_speech = None;
        if long_enough {
            self.speech_since_boundary = false;
        }
        long_enough.then(|| {
            let end = vad_frames_to_ms(last + 1) + VAD_BOUNDARY_TAIL_PAD_MS;
            BoundaryEvent::Boundary(end.min(vad_frames_to_ms(f + 1)))
        })
    }
}

/// Resolves [`DEFAULT_STATE_RECREATE_INTERVAL`] against an env-var
/// override read at process start. Returns 0 to mean "never recreate"
/// (legacy pre-#612-followup behavior — keep available for A/B tests
/// against a recurrence of the leak symptom).
fn state_recreate_interval() -> u64 {
    std::env::var("HUSH_WHISPER_STATE_RECREATE_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STATE_RECREATE_INTERVAL)
}

/// Read VAD configuration from env vars at session construction (#974).
/// Matches the `HUSH_DIARIZER_THRESHOLD` convention so operators can A/B
/// the gate without a rebuild.
///
///   * `HUSH_VAD_THRESHOLD` → probability threshold (default 0.5,
///     clamped to `[0.0, 1.0]`).
///   * `HUSH_VAD_HANGOVER_MS` → ms after the last speech-positive frame
///     before `drain` starts gating inference (default 1500).
///   * `HUSH_VAD_DISABLE=1` → force the gate off entirely. `feed` skips
///     VAD work and `drain` is never gated. Useful for the "is the gate
///     responsible for this miss?" debugging path.
///
/// Returned as a tuple captured once into the session at construction so
/// a mid-meeting env-var change cannot perturb gate behavior partway
/// through (matches the `state_recreate_interval` pattern above).
fn vad_config_from_env() -> (f32, std::time::Duration, bool) {
    let threshold = std::env::var("HUSH_VAD_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(DEFAULT_VAD_THRESHOLD)
        .clamp(0.0, 1.0);
    let hangover_ms = std::env::var("HUSH_VAD_HANGOVER_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_VAD_HANGOVER_MS);
    let disabled = matches!(std::env::var("HUSH_VAD_DISABLE").as_deref(), Ok("1"));
    (
        threshold,
        std::time::Duration::from_millis(hangover_ms),
        disabled,
    )
}

/// A loaded model shared across transcribers, plus one transcriber's
/// inference gate.
///
/// `ctx` is the expensive part (a private copy of every weight tensor) and
/// is shared by every handle cloned from the same load. `gate` serialises
/// inference *within* one transcriber — the dictation transcriber and the
/// meeting transcriber each get their own (see
/// [`WhisperTranscription::share_context`]), so they infer concurrently on
/// separate `WhisperState`s while a meeting's per-source streaming sessions
/// still take turns. The gate guards no data: whisper.cpp only reads the
/// context during `whisper_full_with_state`; see the module doc.
#[derive(Clone)]
struct ContextHandle {
    ctx: Arc<WhisperContext>,
    gate: Arc<Mutex<()>>,
}

/// Held for the duration of one inference. Derefs to the shared context so
/// call sites read exactly as they did when the context sat behind its own
/// mutex.
struct ContextGuard<'a> {
    ctx: &'a WhisperContext,
    _gate: std::sync::MutexGuard<'a, ()>,
}

impl std::ops::Deref for ContextGuard<'_> {
    type Target = WhisperContext;

    fn deref(&self) -> &WhisperContext {
        self.ctx
    }
}

impl ContextHandle {
    fn new(ctx: WhisperContext) -> Self {
        Self {
            ctx: Arc::new(ctx),
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Same weights, fresh gate — for a second transcriber that must not
    /// queue behind this one's inferences.
    fn share(&self) -> Self {
        Self {
            ctx: Arc::clone(&self.ctx),
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Acquire this transcriber's inference gate. Poison is ignored: the
    /// gate guards `()`, so a panic mid-inference leaves nothing
    /// inconsistent behind it. Surfacing poison as an error would instead
    /// brick the slot until restart or a model switch, now that no
    /// per-meeting rebuild hands out fresh gates.
    fn lock(&self) -> ContextGuard<'_> {
        ContextGuard {
            ctx: &self.ctx,
            _gate: self.gate.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

/// `whisper-rs` backed implementation of [`Transcribe`].
///
/// Construct with [`WhisperTranscription::new`]; the constructor loads the
/// model (a one-time multi-second cost on cold start) and the resulting
/// handle can transcribe many recordings in succession.
///
/// The context is held in a [`ContextHandle`] so the streaming session
/// ([`WhisperStreamingSession`]) can hold its own clone of the handle and
/// run inferences from a different thread (the meeting pump's blocking
/// pool), and so a second transcriber built by [`Self::share_context`]
/// reuses the same loaded weights instead of loading its own copy.
pub struct WhisperTranscription {
    /// Loaded GGUF model plus this transcriber's inference gate. The
    /// weights are shared with any transcriber made by
    /// [`Self::share_context`]; the gate is not. The handle is cloned into
    /// streaming sessions because they outlive the borrow that produced
    /// them (the meeting pump moves them across `spawn_blocking`
    /// boundaries).
    ctx: ContextHandle,
    /// Where the model was loaded from. Kept for diagnostics — useful in
    /// error messages and the eventual settings panel.
    model_path: PathBuf,
    /// Live inference thread count (#255). Read on every inference
    /// call and forwarded to `params.set_n_threads`. Writes via
    /// `set_inference_threads` IPC update this atomic; the next call
    /// (one-shot or streaming) picks up the new value with no model
    /// reload. Stored as `Arc<AtomicI32>` so the streaming session
    /// (cloned out of the parent) and the AppState IPC writer share
    /// one canonical count.
    inference_threads: Arc<std::sync::atomic::AtomicI32>,
    /// Live microphone gain in dB (#531). Stored as `f32` bits in an
    /// `AtomicU32` (`f32::to_bits` / `f32::from_bits`) — std has no
    /// `AtomicF32`. Applied after `prepare_audio` in the one-shot
    /// path and after `convert_chunk` in the streaming path so every
    /// inference call sees the current slider position without a
    /// model reload. 0.0 bits = unity (no boost).
    mic_gain_db: Arc<std::sync::atomic::AtomicU32>,
    /// Optional callback fired by whisper.cpp during inference with an
    /// integer percentage (0–100). Set by the IPC layer so the HUD can
    /// show "Processing… N%" in real time (#566). Stored behind
    /// `Arc<Mutex<...>>` so `set_progress_hook` can take `&self` while
    /// the trait contract requires `Arc<dyn Transcribe>` usage.
    progress_hook: ProgressHookSlot,
}

impl WhisperTranscription {
    /// Load a GGUF model from `model_path` and return a ready-to-use handle.
    ///
    /// The path must point at a quantised GGUF file compatible with
    /// whisper.cpp (e.g. `ggml-base.q5_0.bin`). Path resolution
    /// (catalog selection, env override, auto-download) happens
    /// upstream in `AppStateBuilder` / the model picker; this
    /// constructor just loads the file at the supplied path.
    ///
    /// # Errors
    ///
    /// Returns an error if the path does not exist, or if `whisper-rs`
    /// rejects the file (corrupted, wrong format, incompatible version).
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        let model_path = model_path.into();
        Self::with_path(&model_path)?.into_owned(model_path)
    }

    /// Internal constructor split out so the public `new` can capture the
    /// path for diagnostics without re-allocating the `PathBuf`. The
    /// intermediate `LoadedContext` keeps the load logic in one place.
    fn with_path(model_path: &Path) -> Result<LoadedContext> {
        // Pre-check existence so the user gets a clean "no such file" error
        // rather than whatever whisper.cpp surfaces from its file open path,
        // which historically has been less helpful.
        if !model_path.exists() {
            return Err(anyhow!(
                "whisper model file does not exist: {}",
                model_path.display()
            ));
        }

        let path_str = model_path.to_str().ok_or_else(|| {
            anyhow!(
                "whisper model path is not valid UTF-8: {}",
                model_path.display()
            )
        })?;

        // Default context parameters: CPU-only inference, no GPU offload.
        // GPU acceleration is explicitly out of scope for M1 (CPU baseline
        // must work everywhere first).
        let params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(path_str, params)
            .with_context(|| format!("failed to load whisper model: {}", model_path.display()))?;

        Ok(LoadedContext { ctx })
    }

    /// Convert `CapturedAudio` to the 16 kHz mono f32 PCM that whisper.cpp
    /// expects. Public-in-crate so the test suite can exercise the format
    /// pipeline without going through inference.
    pub(crate) fn prepare_audio(audio: &CapturedAudio) -> Result<Vec<f32>> {
        let CapturedAudio { samples, format } = audio;

        if format.sample_rate == 0 {
            return Err(anyhow!("captured audio has zero sample rate"));
        }
        if format.channels == 0 {
            return Err(anyhow!("captured audio has zero channels"));
        }

        // Step 1: collapse to mono. The audio module hands us
        // channel-interleaved samples; whisper expects a single channel.
        let mono = downmix_to_mono(samples, format.channels);

        // Step 2: resample to 16 kHz if needed. The fast path inside
        // resample_to_mono returns the input unchanged when rates match.
        let resampled = resample_to_mono(&mono, format.sample_rate, WHISPER_SAMPLE_RATE);

        Ok(resampled)
    }
}

/// Intermediate type so `with_path` can return the loaded context and
/// `new` can attach the path. Avoids holding the original `PathBuf` across
/// the `?` in `new` and re-allocating it.
struct LoadedContext {
    ctx: WhisperContext,
}

impl WhisperTranscription {
    /// Borrow the live thread-count atomic (#255). AppStateBuilder
    /// reads this at boot to share the same atomic with the IPC
    /// `set_inference_threads` writer, so a slider change in
    /// Settings → General is observable on the next inference call
    /// without a model reload.
    pub fn shared_inference_threads(&self) -> Arc<std::sync::atomic::AtomicI32> {
        Arc::clone(&self.inference_threads)
    }

    /// Builder-style setter: swap the inference-threads atomic for a
    /// caller-supplied one. Production wiring uses this so the
    /// loaded transcriber points at AppState's canonical Arc, not
    /// the fresh one `into_owned` initialised. Tests that don't care
    /// about live updates skip the setter entirely.
    pub fn with_inference_threads(mut self, arc: Arc<std::sync::atomic::AtomicI32>) -> Self {
        self.inference_threads = arc;
        self
    }

    /// Set the live thread count. Clamps to
    /// `[MIN_INFERENCE_THREADS, MAX_INFERENCE_THREADS]` so a
    /// malformed settings row can't push past the band whisper.cpp
    /// is happy with. Use [`Self::shared_inference_threads`] for
    /// the canonical handle that other code reads through.
    pub fn set_inference_threads(&self, threads: i32) {
        let clamped = threads.clamp(MIN_INFERENCE_THREADS, MAX_INFERENCE_THREADS);
        self.inference_threads
            .store(clamped, std::sync::atomic::Ordering::Relaxed);
    }

    /// Borrow the live mic-gain atomic (#531). `AppStateBuilder` reads this
    /// at boot to share the same Arc with the IPC `set_mic_gain_db` writer,
    /// so a slider change takes effect on the next inference call without a
    /// model reload.
    pub fn shared_mic_gain_db(&self) -> Arc<std::sync::atomic::AtomicU32> {
        Arc::clone(&self.mic_gain_db)
    }

    /// Builder-style setter: swap the mic-gain atomic for a caller-supplied
    /// one. Production wiring uses this so the loaded transcriber points at
    /// `AppState`'s canonical Arc. Tests that don't care about live updates
    /// skip the setter entirely.
    pub fn with_mic_gain_db(mut self, arc: Arc<std::sync::atomic::AtomicU32>) -> Self {
        self.mic_gain_db = arc;
        self
    }

    /// A second transcriber over the SAME loaded weights, with its own
    /// inference gate and progress hook. This is how the dictation and
    /// meeting slots get independent inference (#248) without paying for a
    /// second private copy of the weights (whisper.cpp copies every tensor
    /// into host memory on load — see the module doc). The live
    /// inference-threads and mic-gain atomics are shared, not copied, so
    /// the Settings sliders keep applying to both.
    pub fn share_context(&self) -> Self {
        Self {
            ctx: self.ctx.share(),
            model_path: self.model_path.clone(),
            inference_threads: Arc::clone(&self.inference_threads),
            mic_gain_db: Arc::clone(&self.mic_gain_db),
            progress_hook: Arc::new(Mutex::new(None)),
        }
    }
}

impl LoadedContext {
    fn into_owned(self, model_path: PathBuf) -> Result<WhisperTranscription> {
        Ok(WhisperTranscription {
            ctx: ContextHandle::new(self.ctx),
            model_path,
            inference_threads: Arc::new(std::sync::atomic::AtomicI32::new(
                DEFAULT_INFERENCE_THREADS,
            )),
            mic_gain_db: Arc::new(std::sync::atomic::AtomicU32::new(0f32.to_bits())),
            progress_hook: Arc::new(Mutex::new(None)),
        })
    }
}

impl WhisperTranscription {
    /// Inner inference path shared by [`Transcribe::transcribe`] and
    /// [`Transcribe::transcribe_with_prompt`]. The two public methods
    /// differ only in whether they hand `set_initial_prompt` an empty
    /// string or a comma-separated vocabulary list; everything else
    /// (greedy sampling, thread count, lossy segment concatenation) is
    /// identical, so it lives here behind one parameter.
    fn run_inference(&self, audio: &CapturedAudio, prompt: &str) -> Result<String> {
        let mut pcm = Self::prepare_audio(audio)?;

        // Apply user-configured mic gain before inference (#531). A 0-bit
        // AtomicU32 maps to 0.0 dB (unity) which is the no-op fast path
        // inside `apply_mic_gain`.
        let gain_db = f32::from_bits(self.mic_gain_db.load(std::sync::atomic::Ordering::Relaxed));
        apply_mic_gain(&mut pcm, gain_db);

        // Configure inference. Greedy with best_of=1 is the cheapest mode
        // and is sufficient for dictation; beam search is a quality/latency
        // trade we can expose later if user testing shows accuracy gains
        // worth the cost.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(
            self.inference_threads
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        // Suppress whisper.cpp's stdout chatter — we own the user-visible
        // logging surface via `tracing`.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // For M1 we always transcribe (never translate). Locale handling is
        // a settings concern that lands with the model picker.
        params.set_translate(false);
        // #974: defense-in-depth against silence/non-speech hallucinations.
        // Pin greedy decoding with no sampling-fallback ladder so confabulation
        // tokens have no high-T escape hatch. (`set_suppress_nst(true)`, used
        // on the streaming meeting path, is intentionally NOT applied here:
        // whisper.cpp's NST set includes routine punctuation like `[`, `(`,
        // `"`, `:`, `;`, `/` which a developer-dictation user types into
        // structured text — the cost is real and dictation hasn't shown the
        // hallucination class that motivated the streaming gate.)
        //
        // `set_temperature(0.0)` + `set_temperature_inc(0.0)` together pin
        // greedy decoding with NO sampling fallback. whisper.cpp's default
        // builds a fallback ladder [T, T+inc, ..., 1.0] and walks up on
        // decode failure (default inc = 0.2). The ".com" / "Thanks for
        // watching" confabulations come specifically from the high-T
        // fallback step, so pinning T=0 alone (without inc=0) leaves the
        // escape hatch open. Setting inc=0 collapses the ladder.
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);

        // Progress hook for "Processing… N%" in the HUD (#566). Clone the
        // Arc under a short lock so the mutex is not held across the full
        // inference call. The callback throttles to every 5 percentage
        // points to keep event-bus traffic low on short clips.
        let progress_hook = self
            .progress_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = progress_hook {
            params.set_progress_callback_safe(move |progress: i32| {
                if progress % 5 == 0 {
                    (hook.as_ref())(progress);
                }
            });
        }

        // Personal-dictionary vocabulary biasing. The empty-prompt path
        // is what `transcribe()` takes; the populated-prompt path is the
        // one called by `transcribe_with_prompt()`. whisper.cpp tokenises
        // the prompt and biases the decoder's language model toward the
        // tokens; ~224 tokens are honoured before silent truncation, so
        // the formatter in `dictionary::format_vocabulary_prompt` caps
        // the output length to keep us well under that.
        if !prompt.is_empty() {
            params.set_initial_prompt(prompt);
        }
        // Opt-in encoder sizing; 0 (the default) keeps the full 30 s window.
        params.set_audio_ctx(audio_ctx_for(pcm.len()));

        // Hold this transcriber's inference gate for the duration of
        // inference.
        let ctx = self.ctx.lock();

        // `create_state` is required per-call: the state holds the decoder
        // KV cache, which must not be shared across concurrent inferences
        // (the context itself is shared and only read during `full`), and
        // a fresh state also avoids cross-utterance leakage of attention
        // state.
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow!("failed to create whisper state: {e}"))?;

        state
            .full(params, &pcm)
            .map_err(|e| anyhow!("whisper inference failed: {e}"))?;

        // Concatenate every segment whisper produced. The lossy variant
        // tolerates rare invalid-UTF-8 bytes from the model output rather
        // than failing the whole transcription on a single bad token.
        let n_segments = state
            .full_n_segments()
            .map_err(|e| anyhow!("failed to read segment count: {e}"))?;

        let mut text = String::new();
        for i in 0..n_segments {
            let segment = state
                .full_get_segment_text_lossy(i)
                .map_err(|e| anyhow!("failed to read segment {i}: {e}"))?;
            text.push_str(&segment);
        }

        Ok(text.trim().to_owned())
    }
}

impl Transcribe for WhisperTranscription {
    fn transcribe(&self, audio: &CapturedAudio) -> Result<String> {
        self.run_inference(audio, "")
    }

    fn transcribe_with_prompt(&self, audio: &CapturedAudio, prompt: &str) -> Result<String> {
        self.run_inference(audio, prompt)
    }

    /// whisper.cpp's `FullParams::set_initial_prompt` is a real signal
    /// into the decoder — it biases token probabilities toward terms
    /// that appear in the prompt. So vocabulary terms produced by
    /// [`crate::dictionary::format_vocabulary_prompt`] actually take
    /// effect on this backend.
    fn supports_prompt_biasing(&self) -> bool {
        true
    }

    fn set_progress_hook(&self, hook: Option<Arc<dyn Fn(i32) + Send + Sync + 'static>>) {
        *self.progress_hook.lock().unwrap_or_else(|e| e.into_inner()) = hook;
    }

    fn model_label(&self) -> String {
        // Strip directory; the basename is what's recognisable to the
        // user (`ggml-base.q5_0.bin` vs `/Users/.../models/...`). Falls
        // back to the full path on the unlikely event that there is no
        // file component (e.g. a directory was passed; Whisper would
        // have already rejected it at construction time).
        self.model_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.model_path.to_string_lossy().into_owned())
    }

    /// Whisper.cpp's streaming-friendly mode is what the meeting pump
    /// (post-#108) uses to surface live partials. We override
    /// [`Self::start_stream`] to construct a [`WhisperStreamingSession`]
    /// that runs sliding-window inference; signalling this capability
    /// here lets the IPC / pump layer fan partials out to the frontend
    /// instead of waiting for one-shot terminal utterances.
    fn supports_streaming(&self) -> bool {
        true
    }

    fn start_stream(
        &self,
        format: CaptureFormat,
        prompt: &str,
        vad_session: Box<dyn crate::vad::VadSession>,
    ) -> Result<Box<dyn StreamingTranscribeSession>> {
        // All meeting streaming sessions share this transcriber's inference
        // gate. `infer` and `finish` hold it across the entire inference with
        // no early drop, so a live meeting and a finalizing meeting sharing it
        // would freeze the live transcript behind the old `finish()` for up to
        // 60 s. The shared gate is therefore the load-bearing constraint that
        // defers concurrent meetings in v1 — `SessionManager::start_manual`
        // awaits any in-flight finalization before opening a new session. To
        // enable concurrency, give each session (or finalization) its own gate
        // via `ContextHandle::share` — no second model load needed. See
        // learnings.md 2026-05-26 "Deferred: concurrent meetings" for the
        // resume guide.
        let session = WhisperStreamingSession::new(
            self.ctx.clone(),
            format,
            prompt.to_owned(),
            SlidingWindowConfig::meeting_from_env(),
            Arc::clone(&self.inference_threads),
            vad_session,
        );
        Ok(Box::new(session))
    }
}

/// Streaming session backed by `whisper-rs` sliding-window inference.
///
/// Holds:
/// - A clone of the parent [`WhisperTranscription`]'s [`ContextHandle`]
///   so this session can run inferences from a different thread (the meeting
///   pump's blocking pool) without coupling to the original `&self`'s
///   lifetime.
/// - A [`SlidingWindowState`] policy machine (the testable, whisper-agnostic
///   part — see `transcription::streaming`).
/// - The capture format the upstream pump is feeding samples in. Resampling
///   to 16 kHz mono happens inside `feed`, not at the policy layer, so the
///   policy state machine sees only the model's native rate.
///
/// `feed` is cheap (downmix + resample + push to the policy buffer);
/// `drain` is the expensive bit (potentially runs whisper inference
/// over the full ~30 s window). The pump runs `drain` on the
/// blocking pool via `tokio::task::spawn_blocking`.
pub struct WhisperStreamingSession {
    /// Loaded whisper.cpp context. `Some` in production (every
    /// real `start_stream` call clones the parent's `Arc`); `None`
    /// only on the `#[cfg(test)]` `new_for_test` path so the
    /// VAD-gate tests can construct a session without loading a real
    /// GGUF model. The `drain_with_inferer` test helper bypasses
    /// `ctx` entirely so this branching never reaches production code
    /// paths.
    ctx: Option<ContextHandle>,
    /// Capture format the pump is feeding samples in. `feed`
    /// downmixes and resamples to 16 kHz mono before pushing into
    /// the policy machine.
    capture_format: CaptureFormat,
    /// Initial prompt for vocabulary biasing. Empty string = no
    /// prompt. Same semantics as `transcribe_with_prompt`.
    prompt: String,
    /// Policy state machine — owns the rolling window + commit logic.
    /// See `transcription::streaming` for the design rationale.
    state: SlidingWindowState,
    /// Shared inference thread count (#255). Cloned out of
    /// [`WhisperTranscription`] at session construction so
    /// settings updates propagate without rebuilding the session.
    inference_threads: Arc<std::sync::atomic::AtomicI32>,
    /// Reused whisper.cpp inference state for the lifetime of this
    /// streaming session (#612). Lazily created on the first
    /// `infer` call and reused for every subsequent call until the
    /// session ends. Pre-#612, `WhisperInferer::infer` called
    /// `ctx.create_state()` per inference cycle (~3 s) — over a
    /// 35-min meeting that's ~700 init/free cycles, and whisper.cpp's
    /// internal allocations from `whisper_init_state` apparently do
    /// not return cleanly to the C heap on `whisper_free_state`.
    /// The math worked out: 700 calls × ~76 MB per state ≈ 53 GB
    /// of unreclaimed C-heap, matching the symptom in the issue.
    /// Reusing the state holds whisper.cpp's per-session allocations
    /// once and frees them once when the session is dropped, which
    /// is the textbook streaming-mode pattern.
    ///
    /// `set_no_context(true)` (still set in `WhisperInferer::infer`)
    /// keeps each inference run independent at the decoder level —
    /// previous-window text is not fed back into the prompt — so
    /// reusing the state has no quality impact on the policy's
    /// converge-on-stable-transcript story.
    whisper_state: Option<WhisperState>,
    /// Number of `whisper_full` calls run on the current
    /// [`Self::whisper_state`] slot. Reset to 0 every time the slot
    /// is dropped and lazy-recreated. Drives the periodic-recreation
    /// loop bounded by [`DEFAULT_STATE_RECREATE_INTERVAL`] — see the
    /// const's doc-comment and `learnings.md` (#612 second-pass) for
    /// the per-`whisper_full` accumulation this works around.
    inferences_on_current_state: u64,
    /// Cached recreation interval for this session, captured from
    /// the env var at session construction so a mid-meeting toggle
    /// can't change behaviour partway through. 0 means "never
    /// recreate" — used for A/B against a recurrence of the leak.
    state_recreate_interval: u64,
    // ---- VAD gate state (#974) -------------------------------------
    /// Per-stream VAD session. `feed()` drains accumulated audio in
    /// [`crate::vad::FRAME_LEN_SAMPLES`]-sized chunks through this and
    /// updates [`Self::last_speech_at`] whenever a frame's speech
    /// probability crosses [`Self::vad_threshold`].
    vad_session: Box<dyn crate::vad::VadSession>,
    /// Partial-frame buffer carried between `feed()` calls. Lifetime is
    /// the session — `finish` consumes `Box<Self>` so no explicit flush
    /// is needed; a future re-use across a logical stream boundary would
    /// need an explicit clear.
    vad_residual: Vec<f32>,
    /// Wall-clock instant of the most recent VAD-positive frame. `None`
    /// until the first speech-positive frame. `should_gate()` reads it
    /// through the hangover predicate. **Note:** when `vad_disabled` is
    /// `true` this field is forced to `Some(Instant::now())` on every
    /// `feed`, so a direct read is meaningless — always check
    /// `vad_disabled` first.
    last_speech_at: Option<std::time::Instant>,
    /// Cached env-var configuration; read once at construction. Holds
    /// the threshold + hangover so a mid-meeting env-var change
    /// can't perturb gate behavior partway through (mirrors
    /// `state_recreate_interval`'s freeze-at-construction rule).
    vad_threshold: f32,
    vad_hangover: std::time::Duration,
    /// `HUSH_VAD_DISABLE=1` short-circuits the gate: `feed()` skips
    /// VAD work and `drain()` always treats the session as speech-
    /// present. Behaviour matches the pre-#974 ungated path so we can
    /// A/B against it without rebuilding.
    vad_disabled: bool,
    /// Set once `vad_session.score_frame` returns an error in this session.
    /// Prevents the WARN log from firing on every 32ms frame if the VAD
    /// is consistently failing — first error tells us everything.
    vad_error_logged: bool,
    /// Whether the previous drain ran inference. Used by the gate-close
    /// flush mechanism (#974 follow-up): when drain transitions from
    /// running → gating, fire one more `state.tick_flush(...)` pass so
    /// utterances mid-flight at the moment of silence get committed
    /// before the streaming policy's head-slide can strand them.
    ///
    /// `true` means "last call to drain ran the inferer"; `false` means
    /// "either never inferred yet, or last drain was gated".
    was_inferring: bool,
    // ---- VAD-boundary windowing (#1013, opt-in) ---------------------
    /// `Some` when `HUSH_VAD_BOUNDARY=1`. Tracks speech regions from
    /// the per-frame VAD decisions `drain_vad` already makes.
    boundary: Option<BoundaryTracker>,
    /// Region end (absolute ms) waiting for the next `drain` to commit.
    pending_boundary_ms: Option<u64>,
    /// Speech onset (absolute ms) to trim leading silence up to, applied
    /// in `feed` once the samples are in the window.
    pending_onset_ms: Option<u64>,
}

impl WhisperStreamingSession {
    fn new(
        ctx: ContextHandle,
        capture_format: CaptureFormat,
        prompt: String,
        config: SlidingWindowConfig,
        inference_threads: Arc<std::sync::atomic::AtomicI32>,
        vad_session: Box<dyn crate::vad::VadSession>,
    ) -> Self {
        let (vad_threshold, vad_hangover, vad_disabled) = vad_config_from_env();
        Self {
            ctx: Some(ctx),
            capture_format,
            prompt,
            state: SlidingWindowState::new(WHISPER_SAMPLE_RATE, config),
            inference_threads,
            whisper_state: None,
            inferences_on_current_state: 0,
            state_recreate_interval: state_recreate_interval(),
            vad_session,
            vad_residual: Vec::with_capacity(crate::vad::FRAME_LEN_SAMPLES),
            last_speech_at: None,
            vad_threshold,
            vad_hangover,
            vad_disabled,
            vad_error_logged: false,
            was_inferring: false,
            // Boundary mode is driven entirely by VAD frames: with the VAD
            // disabled the tracker would never see one, report "no speech
            // left", and `finish` would skip flushing the uncommitted tail.
            boundary: vad_boundary_config_from_env()
                .filter(|_| !vad_disabled)
                .map(BoundaryTracker::new),
            pending_boundary_ms: None,
            pending_onset_ms: None,
        }
    }

    /// Test-only constructor: build a session without a real
    /// `WhisperContext`. The VAD-gate tests in this module need to
    /// exercise `feed`'s framing logic and `drain`'s gate decision
    /// without loading a real GGUF model — `ctx = None` plus the
    /// `drain_with_inferer` helper below let them do that. Production
    /// callers always go through [`Self::new`].
    #[cfg(test)]
    pub(super) fn new_for_test(
        capture_format: CaptureFormat,
        config: SlidingWindowConfig,
        vad_session: Box<dyn crate::vad::VadSession>,
    ) -> Self {
        let (vad_threshold, vad_hangover, vad_disabled) = vad_config_from_env();
        Self {
            ctx: None,
            capture_format,
            prompt: String::new(),
            state: SlidingWindowState::new(WHISPER_SAMPLE_RATE, config),
            inference_threads: Arc::new(std::sync::atomic::AtomicI32::new(
                DEFAULT_INFERENCE_THREADS,
            )),
            whisper_state: None,
            inferences_on_current_state: 0,
            state_recreate_interval: 0,
            vad_session,
            vad_residual: Vec::with_capacity(crate::vad::FRAME_LEN_SAMPLES),
            last_speech_at: None,
            vad_threshold,
            vad_hangover,
            vad_disabled,
            vad_error_logged: false,
            was_inferring: false,
            // Boundary mode is driven entirely by VAD frames: with the VAD
            // disabled the tracker would never see one, report "no speech
            // left", and `finish` would skip flushing the uncommitted tail.
            boundary: vad_boundary_config_from_env()
                .filter(|_| !vad_disabled)
                .map(BoundaryTracker::new),
            pending_boundary_ms: None,
            pending_onset_ms: None,
        }
    }

    /// Test-only setter for the speech-presence clock — lets the
    /// VAD-gate tests place the last speech instant at any offset
    /// without actually feeding speech-positive frames + sleeping.
    #[cfg(test)]
    pub(super) fn set_last_speech_at_for_test(&mut self, when: Option<std::time::Instant>) {
        self.last_speech_at = when;
    }

    /// Test-only accessor for the hangover window. Tests place
    /// `last_speech_at` relative to this value to land on either side
    /// of the gate.
    #[cfg(test)]
    pub(super) fn vad_hangover_for_test(&self) -> std::time::Duration {
        self.vad_hangover
    }

    /// Whether `drain` should skip inference: `true` iff the VAD gate
    /// is enabled AND no recent speech is within the hangover window.
    /// Pulled out so production `drain` and the test-only
    /// `drain_with_inferer` share the same gate decision verbatim.
    fn should_gate(&self) -> bool {
        if self.vad_disabled {
            return false;
        }
        match self.last_speech_at {
            None => true,
            Some(when) => when.elapsed() > self.vad_hangover,
        }
    }

    /// Drain accumulated audio through the VAD in
    /// [`crate::vad::FRAME_LEN_SAMPLES`]-sized frames; update
    /// `last_speech_at` when any frame's probability crosses
    /// [`Self::vad_threshold`]. Carry partial frames in `vad_residual`
    /// for the next call.
    ///
    /// VAD errors are logged at WARN and treated as "speech" — same
    /// graceful-degrade philosophy as `NoopVad`: a broken gate must
    /// never silently swallow real audio.
    fn drain_vad(&mut self, samples: &[f32]) {
        if self.vad_disabled {
            // Disabled gate: pretend every feed contains speech so
            // `drain` never gates. Skip the framing + ONNX work
            // entirely (the whole point of the disable knob).
            self.last_speech_at = Some(std::time::Instant::now());
            return;
        }
        let frame_len = crate::vad::FRAME_LEN_SAMPLES;
        self.vad_residual.extend_from_slice(samples);
        let mut offset = 0usize;
        // Track the loudest frame this feed for the `HUSH_VAD_TRACE`
        // diagnostic (#974 follow-up) — lets a real meeting reveal where
        // compressed call audio sits relative to the threshold.
        let mut max_prob = 0.0f32;
        let mut frames_scored = 0usize;
        while self.vad_residual.len() - offset >= frame_len {
            let frame = &self.vad_residual[offset..offset + frame_len];
            match self.vad_session.score_frame(frame) {
                Ok(prob) => {
                    frames_scored += 1;
                    max_prob = max_prob.max(prob);
                    let is_speech = prob >= self.vad_threshold;
                    if is_speech {
                        self.last_speech_at = Some(std::time::Instant::now());
                    }
                    self.observe_boundary(is_speech);
                }
                Err(e) => {
                    if !self.vad_error_logged {
                        tracing::warn!(
                            error = ?e,
                            "VAD frame scoring failed; falling back to ungated \
                             (further errors in this session will be suppressed)"
                        );
                        self.vad_error_logged = true;
                    }
                    self.last_speech_at = Some(std::time::Instant::now());
                    // Graceful degrade: a failing VAD counts as speech,
                    // so boundary mode never closes a region on it.
                    self.observe_boundary(true);
                }
            }
            offset += frame_len;
        }
        self.vad_residual.drain(..offset);

        if frames_scored > 0 && vad_trace_enabled() {
            tracing::info!(
                max_prob,
                threshold = self.vad_threshold,
                frames = frames_scored,
                speech = max_prob >= self.vad_threshold,
                "VAD trace: feed scored (HUSH_VAD_TRACE)"
            );
        }
    }

    /// Feed one VAD decision to the boundary tracker (no-op unless
    /// boundary mode is on) and queue what it reports.
    fn observe_boundary(&mut self, is_speech: bool) {
        let Some(tracker) = self.boundary.as_mut() else {
            return;
        };
        match tracker.observe(is_speech) {
            Some(BoundaryEvent::Boundary(ms)) => {
                self.pending_boundary_ms = Some(self.pending_boundary_ms.map_or(ms, |p| p.max(ms)));
            }
            // Only trim on an onset when no region is waiting to be
            // committed — otherwise the trim would cut into it.
            Some(BoundaryEvent::Onset(ms)) if self.pending_boundary_ms.is_none() => {
                self.pending_onset_ms = Some(ms.saturating_sub(VAD_BOUNDARY_LEAD_PAD_MS));
            }
            _ => {}
        }
    }

    /// Test-only drain that runs the gate against an arbitrary
    /// inferer. Lets the VAD-gate tests assert "inferer was / was not
    /// invoked" without constructing a real `WhisperContext`. The
    /// production [`StreamingTranscribeSession::drain`] runs the same
    /// gate check (including the gate-close flush) then dispatches
    /// against a real `WhisperInferer` built from `self.ctx`.
    #[cfg(test)]
    pub(super) fn drain_with_inferer(
        &mut self,
        inferer: &mut dyn WhisperLikeInferer,
    ) -> Result<Vec<Utterance>> {
        if let Some(boundary_ms) = self.pending_boundary_ms.take() {
            self.was_inferring = false;
            return self.state.commit_through(inferer, boundary_ms);
        }
        if self.should_gate() {
            if self.was_inferring {
                // First gated drain after a run of inferences: flush
                // anything in flight so the head-slide doesn't strand
                // it. After this, subsequent gated drains skip.
                self.was_inferring = false;
                return self.state.tick_flush(inferer);
            }
            return Ok(Vec::new());
        }
        // Not gating — record so the next gate-close fires a flush.
        self.was_inferring = true;
        self.state.tick(inferer)
    }

    /// Convert one chunk of capture-format samples to mono 16 kHz
    /// before feeding into the policy buffer. The same downmix +
    /// resample chain the one-shot path uses, applied per `feed` chunk.
    fn convert_chunk(&self, samples: &[f32]) -> Result<Vec<f32>> {
        if self.capture_format.sample_rate == 0 {
            return Err(anyhow::anyhow!("captured audio has zero sample rate"));
        }
        if self.capture_format.channels == 0 {
            return Err(anyhow::anyhow!("captured audio has zero channels"));
        }
        let mono = downmix_to_mono(samples, self.capture_format.channels);
        Ok(resample_to_mono(
            &mono,
            self.capture_format.sample_rate,
            WHISPER_SAMPLE_RATE,
        ))
    }
}

impl StreamingTranscribeSession for WhisperStreamingSession {
    fn feed(&mut self, captured: &[f32]) -> Result<()> {
        let mono_16k = self.convert_chunk(captured)?;
        if !mono_16k.is_empty() {
            // Drive the VAD against the mono-16kHz sample stream — the
            // same rate Silero expects, and the rate the policy machine
            // sees. Doing this BEFORE the policy push keeps the window
            // and the VAD's view perfectly aligned regardless of how
            // the capture pump chunks samples (#974).
            self.drain_vad(&mono_16k);
            self.state.feed_mono(&mono_16k);
            if let Some(onset_ms) = self.pending_onset_ms.take() {
                self.state.skip_head_until(onset_ms);
            }
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        // VAD gate (#974): skip inference entirely when no recent
        // speech is within the hangover window. Whisper.cpp is the
        // expensive bit; skipping it on silence is the load-bearing
        // win — preventing hallucinations on non-speech windows like
        // a Zoom hold beep or a typing sound.
        let trace = vad_trace_enabled();
        // VAD-boundary commit (#1013, opt-in): a speech region just
        // closed — decode exactly that region and commit it. Runs ahead
        // of the gate (the boundary fires inside the hangover, while the
        // gate is still open). Clearing `was_inferring` stops the
        // gate-close flush from re-decoding the silence that follows.
        if let Some(boundary_ms) = self.pending_boundary_ms.take() {
            if trace {
                tracing::info!(
                    boundary_ms,
                    "VAD trace: region closed → boundary commit (HUSH_VAD_BOUNDARY)"
                );
            }
            self.was_inferring = false;
            let ctx = self
                .ctx
                .as_ref()
                .expect("WhisperStreamingSession::drain called without a loaded ctx (production paths always supply Some)")
                .clone();
            let mut inferer = WhisperInferer {
                ctx,
                prompt: &self.prompt,
                inference_threads: Arc::clone(&self.inference_threads),
                whisper_state: &mut self.whisper_state,
                inferences_on_current_state: &mut self.inferences_on_current_state,
                state_recreate_interval: self.state_recreate_interval,
            };
            return self.state.commit_through(&mut inferer, boundary_ms);
        }
        if self.should_gate() {
            if self.was_inferring {
                // First gated drain after a run of inferences: flush
                // anything mid-flight before the head-slide can strand
                // it (#974 follow-up). After this fires, subsequent
                // gated drains skip cheaply via the early-return below.
                if trace {
                    tracing::info!(
                        "VAD trace: gate closed → final flush inference (HUSH_VAD_TRACE)"
                    );
                }
                self.was_inferring = false;
                let ctx = self
                    .ctx
                    .as_ref()
                    .expect("WhisperStreamingSession::drain called without a loaded ctx (production paths always supply Some)")
                    .clone();
                let mut inferer = WhisperInferer {
                    ctx,
                    prompt: &self.prompt,
                    inference_threads: Arc::clone(&self.inference_threads),
                    whisper_state: &mut self.whisper_state,
                    inferences_on_current_state: &mut self.inferences_on_current_state,
                    state_recreate_interval: self.state_recreate_interval,
                };
                return self.state.tick_flush(&mut inferer);
            }
            if trace {
                tracing::info!("VAD trace: gate suppressing inference (silence) (HUSH_VAD_TRACE)");
            }
            return Ok(Vec::new());
        }
        if trace {
            tracing::info!("VAD trace: gate open → inference (speech) (HUSH_VAD_TRACE)");
        }
        // Not gating — record so the next gate-close fires a flush.
        self.was_inferring = true;
        let ctx = self
            .ctx
            .as_ref()
            .expect("WhisperStreamingSession::drain called without a loaded ctx (production paths always supply Some)")
            .clone();
        let mut inferer = WhisperInferer {
            ctx,
            prompt: &self.prompt,
            inference_threads: Arc::clone(&self.inference_threads),
            whisper_state: &mut self.whisper_state,
            inferences_on_current_state: &mut self.inferences_on_current_state,
            state_recreate_interval: self.state_recreate_interval,
        };
        self.state.tick(&mut inferer)
    }

    fn finish(mut self: Box<Self>) -> Result<Vec<Utterance>> {
        let ctx = self
            .ctx
            .as_ref()
            .expect("WhisperStreamingSession::finish called without a loaded ctx (production paths always supply Some)")
            .clone();
        let pending_boundary = self.pending_boundary_ms.take();
        let speech_left = self
            .boundary
            .as_ref()
            .map(BoundaryTracker::has_uncommitted_speech);
        let mut inferer = WhisperInferer {
            ctx,
            prompt: &self.prompt,
            inference_threads: Arc::clone(&self.inference_threads),
            whisper_state: &mut self.whisper_state,
            inferences_on_current_state: &mut self.inferences_on_current_state,
            state_recreate_interval: self.state_recreate_interval,
        };
        finish_with_boundary(&mut self.state, &mut inferer, pending_boundary, speech_left)
    }
}

/// Session-end flush, aware of VAD-boundary mode (#1013). Commits any
/// region still waiting for a drain, then runs the normal tail flush —
/// unless boundary mode knows the rest of the window is pure silence
/// (`speech_left == Some(false)`), in which case decoding it would only
/// invite a confabulation (the first real-audio run produced a "Thank
/// you." from 3 s of trailing hiss) and the tail is dropped instead.
/// `speech_left == None` (boundary mode off) is the unchanged path.
fn finish_with_boundary(
    state: &mut SlidingWindowState,
    inferer: &mut dyn WhisperLikeInferer,
    pending_boundary: Option<u64>,
    speech_left: Option<bool>,
) -> Result<Vec<Utterance>> {
    let mut out = match pending_boundary {
        Some(ms) => state.commit_through(inferer, ms)?,
        None => Vec::new(),
    };
    if speech_left == Some(false) {
        tracing::debug!(
            "streaming finish: boundary mode, no speech after last region — skipping tail decode"
        );
        return Ok(out);
    }
    out.extend(state.finish(inferer)?);
    Ok(out)
}

/// Adapter that plugs whisper.cpp inference into the
/// [`WhisperLikeInferer`] trait the policy state machine calls. Lives
/// here (not in `streaming.rs`) so the policy module can be tested
/// without the `whisper` Cargo feature.
struct WhisperInferer<'a> {
    ctx: ContextHandle,
    prompt: &'a str,
    inference_threads: Arc<std::sync::atomic::AtomicI32>,
    /// Persistent reference to the streaming session's reused
    /// `WhisperState` slot (#612). Lazily created on the first
    /// `infer` call so a session that never produces audio never
    /// pays the init cost; reused on every subsequent call so
    /// whisper.cpp's per-init C-heap allocations don't accumulate
    /// across the meeting.
    whisper_state: &'a mut Option<WhisperState>,
    /// Companion counter for the periodic-recreation loop bounded by
    /// `state_recreate_interval`. Incremented after each successful
    /// `whisper_full` call; when it reaches the interval, the state
    /// slot is dropped so the next call lazy-recreates a fresh one.
    inferences_on_current_state: &'a mut u64,
    /// Number of inferences a single state is reused for before
    /// recreation. 0 means "never recreate" (legacy pre-#612-followup
    /// behaviour, available for A/B testing).
    state_recreate_interval: u64,
}

impl<'a> WhisperLikeInferer for WhisperInferer<'a> {
    fn infer(&mut self, mono_16k_pcm: &[f32]) -> Result<Vec<StreamSegment>> {
        // Same FullParams shape as the one-shot path — greedy decode,
        // configurable thread count, no chatter on stdout. The streaming-specific
        // bit is `set_no_context(true)`: we feed whisper a fresh window
        // each call rather than carrying KV-cache across calls.
        // Carrying context would technically reduce per-call cost but
        // also propagate any segment-level mistakes from one inference
        // into the next — the no-context path produces independent
        // re-tokenisations and lets the sliding-window policy converge
        // on a stable transcript.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(
            self.inference_threads
                .load(std::sync::atomic::Ordering::Relaxed),
        );
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_translate(false);
        params.set_no_context(true);
        // #974: defense-in-depth against silence/non-speech hallucinations.
        //
        // `set_temperature(0.0)` + `set_temperature_inc(0.0)` together pin
        // greedy decoding with NO sampling fallback. whisper.cpp's default
        // builds a fallback ladder [T, T+inc, ..., 1.0] and walks up on
        // decode failure (default inc = 0.2). The ".com" / "Thanks for
        // watching" confabulations come specifically from the high-T
        // fallback step, so pinning T=0 alone (without inc=0) leaves the
        // escape hatch open. Setting inc=0 collapses the ladder.
        //
        // `set_suppress_nst(true)` blocks non-speech tokens like `[Music]`
        // / `[Applause]` from being decoded at all.
        params.set_temperature(0.0);
        params.set_temperature_inc(0.0);
        params.set_suppress_nst(true);
        // `no_speech_thold` (#974 follow-up): explicitly set the
        // silence-discard threshold whisper.cpp otherwise leaves at its
        // 0.6 default, so it becomes a single tunable knob
        // (`HUSH_WHISPER_NO_SPEECH_THOLD`). Default value matches the
        // built-in, so this is behavior-neutral until tuned. `suppress_nst`
        // only blocks bracketed non-speech tokens; this is the lever for
        // the English-text confabulations on compressed call audio.
        params.set_no_speech_thold(no_speech_thold());
        // `logprob_thold` (#1013): the other half of whisper.cpp's
        // no-speech AND — see `DEFAULT_LOGPROB_THOLD`.
        params.set_logprob_thold(logprob_thold());
        if !self.prompt.is_empty() {
            params.set_initial_prompt(self.prompt);
        }
        // Opt-in encoder sizing; 0 (the default) keeps the full 30 s window.
        params.set_audio_ctx(audio_ctx_for(mono_16k_pcm.len()));

        let ctx = self.ctx.lock();
        // Reuse a single WhisperState across calls (#612). Pre-#612
        // this branch ran `ctx.create_state()` per call — over a
        // long session that's hundreds of init/free cycles, and
        // whisper.cpp's per-init C-heap allocations apparently do
        // not return cleanly to the C heap on free. Lazy init keeps
        // the no-audio session from paying the init cost; subsequent
        // calls hit the `else` branch and reuse the existing state.
        if self.whisper_state.is_none() {
            *self.whisper_state = Some(
                ctx.create_state()
                    .map_err(|e| anyhow!("failed to create whisper state: {e}"))?,
            );
            tracing::debug!("whisper streaming session: created reusable WhisperState (#612)");
        }
        // Run the inference and capture the result so we can drop
        // the state on error rather than reusing it. whisper.cpp's
        // contract on partial-failure state is undocumented; a state
        // that errored mid-decode could carry KV-cache junk into the
        // next inference. Recreating costs the ~76 MB init again, but
        // only on the rare error path.
        let infer_result = self
            .whisper_state
            .as_mut()
            .expect("whisper_state is Some after the lazy-init branch above")
            .full(params, mono_16k_pcm);
        if let Err(e) = infer_result {
            *self.whisper_state = None;
            *self.inferences_on_current_state = 0;
            // The error path frees the most memory of any exit (the
            // failed inference's scratch PLUS the ~76 MB state we just
            // dropped) — purge it like the success path does so a burst
            // of decode failures can't accumulate dirty pages (#985
            // review follow-up). Release the inference gate first so the
            // purge never extends hold time for the other source's
            // session.
            drop(ctx);
            crate::alloc_tuning::force_collect();
            return Err(anyhow!("whisper streaming inference failed: {e}"));
        }
        // Re-borrow for segment reading on the success path. The slot
        // is still Some(_) because we only clear it in the err branch.
        let state = self
            .whisper_state
            .as_mut()
            .expect("whisper_state is Some on the inference-success path");

        // Inference ran successfully — bump the counter. We do this
        // *before* segment reading because all the `state.full_*`
        // calls below are read-only against the just-completed
        // inference and don't allocate scratch the way `full` does.
        *self.inferences_on_current_state += 1;

        let n_segments = state
            .full_n_segments()
            .map_err(|e| anyhow!("failed to read segment count: {e}"))?;

        tracing::debug!(
            n_segments,
            window_samples = mono_16k_pcm.len(),
            // Cross-check for the streaming layer: if this is 0 but the
            // calling layer reports samples flowing, no_speech_thold (0.6)
            // is suppressing the audio. Compare with raw_segments in
            // streaming.rs to distinguish "whisper ran but filtered" from
            // "whisper never ran".
            "whisper: inference complete"
        );

        let eot = ctx.token_eot();
        let mut out = Vec::with_capacity(n_segments as usize);
        for i in 0..n_segments {
            let text = state
                .full_get_segment_text_lossy(i)
                .map_err(|e| anyhow!("failed to read segment {i}: {e}"))?;
            // whisper.cpp returns t0 / t1 in 10ms units (centiseconds).
            // The policy machine expects ms — multiply by 10 here so
            // the conversion stays in one place.
            let t0 = state
                .full_get_segment_t0(i)
                .map_err(|e| anyhow!("failed to read segment {i} t0: {e}"))?;
            let t1 = state
                .full_get_segment_t1(i)
                .map_err(|e| anyhow!("failed to read segment {i} t1: {e}"))?;
            let start_ms = (t0.max(0) as u64).saturating_mul(10);
            let end_ms = (t1.max(0) as u64).saturating_mul(10);
            // Per-segment confidence (#1013): mean text-token logprob
            // for the final filter + word-level p for shading.
            let confidence = aggregate_token_scores(&segment_token_scores(state, i, eot));
            out.push(StreamSegment {
                start_ms,
                end_ms,
                text,
                avg_logprob: confidence.avg_logprob,
                words: confidence.words,
            });
        }

        // Periodic state recreation (#612 second-pass): after every
        // `state_recreate_interval` calls, drop the state so the next
        // call lazy-recreates a fresh one. Bounds whisper.cpp's
        // per-`whisper_full` C-heap accumulation that the long-lived
        // state from #615 doesn't address. The `state` borrow above
        // is no longer used past the for-loop, so NLL ends it before
        // we reassign `*self.whisper_state` here. interval == 0 means
        // "never recreate" (A/B knob).
        if self.state_recreate_interval > 0
            && *self.inferences_on_current_state >= self.state_recreate_interval
        {
            // Capture RSS before and after the state drop so the log
            // shows whether dropping the state actually reclaims any
            // memory (#612 follow-up). If `delta` is reliably ~0
            // across recreations, the per-`whisper_full` accumulation
            // is owned by something OTHER than the state — most
            // likely `WhisperContext` itself — and a different lever
            // is needed (per-context recreation, model unload on
            // idle, or upstream fix). If `delta` is reliably negative
            // (RSS dropped), the recreation is doing work and the
            // remaining growth is coming from something else (audio
            // buffers, diarizer, etc.). Reading `ps` shells out per
            // recreation event (~once per 90 s of speech) — cost is
            // immaterial relative to the 76 MB recreate cost.
            let rss_before_mb = current_rss_mb();
            *self.whisper_state = None;
            *self.inferences_on_current_state = 0;
            let rss_after_mb = current_rss_mb();
            let delta_mb = match (rss_before_mb, rss_after_mb) {
                (Some(b), Some(a)) => Some(a - b),
                _ => None,
            };
            tracing::info!(
                inferences = self.state_recreate_interval,
                ?rss_before_mb,
                ?rss_after_mb,
                ?delta_mb,
                "whisper streaming session: recreating WhisperState (#612 periodic recreation)"
            );
        }

        // Hand the pages this inference (and the periodic state drop
        // above, when it fired) just freed back to the OS immediately.
        // Without this, mimalloc keeps them committed-and-dirty on
        // thread-local free lists and macOS compresses them — physical
        // footprint grows ~1 GB/min over a meeting even though RSS
        // stays bounded (learnings.md 2026-06-01). No-op unless
        // allocator purge tuning is enabled (`alloc_tuning::init`).
        // Release the inference gate first so the purge never extends
        // hold time for the other source's session.
        drop(ctx);
        crate::alloc_tuning::force_collect();

        Ok(out)
    }
}

/// Read current RSS (resident set size) of this process in MB.
///
/// Shells out to `ps -o rss= -p <pid>` because it's the simplest
/// path that doesn't add a dep — `mach2`'s `mach_task_basic_info`
/// would be the right cross-platform-ish answer but pulling a new
/// crate just to log a number on macOS isn't worth it. `ps`'s
/// `rss` column is in KB on macOS (and Linux); we convert to MB.
///
/// Returns `None` if the shell-out failed (parse error, missing
/// `ps` binary). Callers degrade to "no number logged" in that
/// case rather than blocking the recreation.
fn current_rss_mb() -> Option<f64> {
    let pid = std::process::id();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let kb: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(kb / 1024.0)
}

impl std::fmt::Debug for WhisperTranscription {
    /// Custom Debug because `WhisperContext` is not itself `Debug`. We log
    /// only the model path; the context's internal pointers are not useful
    /// in human-facing diagnostics.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WhisperTranscription")
            .field("model_path", &self.model_path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn dynamic_audio_ctx_covers_input_plus_margin() {
        // 11 s = 176_000 samples = 550 positions; + 64 margin.
        assert_eq!(super::dynamic_audio_ctx(176_000, 64, 384), 614);
        // Partial positions round up: one extra sample needs one more frame.
        assert_eq!(super::dynamic_audio_ctx(176_001, 0, 0), 551);
    }

    #[test]
    fn dynamic_audio_ctx_applies_floor_and_ceiling() {
        // 1 s clip: 50 + 64 = 114, lifted to the floor.
        assert_eq!(super::dynamic_audio_ctx(16_000, 64, 384), 384);
        // 29 s + margin would exceed the model's 1500 positions.
        assert_eq!(super::dynamic_audio_ctx(29 * 16_000, 64, 384), 1500);
        // Longer than 30 s (dictation can be) still clamps rather than
        // tripping whisper.cpp's audio_ctx > n_audio_ctx error.
        assert_eq!(super::dynamic_audio_ctx(10 * 60 * 16_000, 64, 384), 1500);
        // Empty input with no floor or margin still yields a valid ctx.
        assert_eq!(super::dynamic_audio_ctx(0, 0, 0), 1);
        // A negative margin is ignored, not subtracted.
        assert_eq!(super::dynamic_audio_ctx(176_000, -100, 0), 550);
    }

    #[test]
    fn resolve_audio_ctx_is_off_unless_exactly_one() {
        for off in [None, Some("0"), Some(""), Some("true"), Some("2")] {
            assert_eq!(
                super::resolve_audio_ctx(176_000, off, None, None),
                0,
                "{off:?}"
            );
        }
        assert_eq!(
            super::resolve_audio_ctx(176_000, Some(" 1 "), None, None),
            550 + super::DEFAULT_AUDIO_CTX_MARGIN
        );
    }

    #[test]
    fn resolve_audio_ctx_reads_margin_and_floor_overrides() {
        assert_eq!(
            super::resolve_audio_ctx(176_000, Some("1"), Some("0"), Some("0")),
            550
        );
        assert_eq!(
            super::resolve_audio_ctx(16_000, Some("1"), Some("32"), Some("256")),
            256
        );
        // Garbage falls back to the defaults; out-of-range values clamp.
        assert_eq!(
            super::resolve_audio_ctx(16_000, Some("1"), Some("x"), Some("y")),
            super::DEFAULT_AUDIO_CTX_FLOOR
        );
        assert_eq!(
            super::resolve_audio_ctx(16_000, Some("1"), None, Some("99999")),
            1500
        );
    }

    #[test]
    fn vad_boundary_is_on_unless_explicitly_disabled() {
        assert!(super::vad_boundary_enabled(None));
        assert!(super::vad_boundary_enabled(Some("1")));
        for off in ["0", "false", "off", " 0 "] {
            assert!(!super::vad_boundary_enabled(Some(off)), "{off:?}");
        }
    }

    use super::*;
    use crate::audio::CaptureFormat;
    use std::sync::Mutex;

    // Tests that mutate env vars must hold this lock for the full
    // read-mutate-restore cycle; Rust test threads run in parallel and a
    // remove_var in one test can race a set_var in another.
    //
    // Currently guards:
    //   * HUSH_WHISPER_STATE_RECREATE_INTERVAL (state_recreate_interval tests)
    //
    // Future tests that set HUSH_VAD_THRESHOLD, HUSH_VAD_HANGOVER_MS, or
    // HUSH_VAD_DISABLE must also acquire this lock — the VAD config is read
    // at session construction (WhisperStreamingSession::new / new_for_test)
    // and is equally susceptible to concurrent-env-var races.
    static ENV_VAR_LOCK: Mutex<()> = Mutex::new(());

    /// `prepare_audio` is the pure-logic glue between the audio module's
    /// output format and whisper's input format. We can exercise it without
    /// loading a real model, which keeps this test in the fast feature-on
    /// suite.
    #[test]
    fn prepare_audio_downmixes_and_resamples() {
        // Stereo at 48 kHz → mono at 16 kHz. 480 samples * 2 channels at
        // 48 kHz is 5 ms of audio; we expect ~80 mono samples at 16 kHz.
        let samples = vec![0.5_f32; 480 * 2];
        let audio = CapturedAudio {
            samples,
            format: CaptureFormat {
                sample_rate: 48_000,
                channels: 2,
            },
        };
        let pcm = WhisperTranscription::prepare_audio(&audio).unwrap();
        // Length check: ratio is 1/3, ceil applied.
        assert!(
            (160..=161).contains(&pcm.len()),
            "unexpected length {}",
            pcm.len()
        );
        // Constant input survives the pipeline as a near-constant output;
        // a 0.5 stereo signal downmixes to 0.5 mono and the linear
        // resampler preserves constants exactly.
        for (i, &v) in pcm.iter().enumerate() {
            assert!((v - 0.5).abs() < 1e-6, "pcm[{i}] = {v}, want 0.5");
        }
    }

    #[test]
    fn prepare_audio_rejects_zero_sample_rate() {
        // A zero-rate format should never come from the audio module, but
        // surfacing a clear error is cheaper than crashing inside the
        // resampler. Defence-in-depth at the IPC boundary.
        let audio = CapturedAudio {
            samples: vec![0.0],
            format: CaptureFormat {
                sample_rate: 0,
                channels: 1,
            },
        };
        assert!(WhisperTranscription::prepare_audio(&audio).is_err());
    }

    #[test]
    fn prepare_audio_rejects_zero_channels() {
        // Defence-in-depth: downmix_to_mono with channels==0 produces a
        // degenerate empty mono buffer; catch it at the format-validation
        // boundary instead (#922).
        let audio = CapturedAudio {
            samples: vec![0.0],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 0,
            },
        };
        assert!(WhisperTranscription::prepare_audio(&audio).is_err());
    }

    /// The constructor must reject a non-existent path with a clear error.
    /// We do not load a real model in this test (no GGUF in the fixture
    /// tree); the happy-path constructor is exercised by the
    /// `tests/audio_fixture.rs` integration test when
    /// `HUSH_TEST_MODEL` points at a real GGUF.
    #[test]
    fn constructor_rejects_missing_model_file() {
        let err = WhisperTranscription::new("/nonexistent/path/to/model.bin").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not exist"),
            "expected 'does not exist' in error, got: {msg}"
        );
    }

    #[test]
    fn state_recreate_interval_defaults_to_const_without_env_var() {
        // Pin the default so a typo in the env-var name doesn't
        // silently disable the periodic recreation that #612's
        // second pass relies on.
        let _guard = ENV_VAR_LOCK.lock().unwrap();
        let saved = std::env::var("HUSH_WHISPER_STATE_RECREATE_INTERVAL").ok();
        unsafe { std::env::remove_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL") };
        assert_eq!(state_recreate_interval(), DEFAULT_STATE_RECREATE_INTERVAL);
        if let Some(prev) = saved {
            unsafe { std::env::set_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL", prev) };
        }
    }

    #[test]
    fn state_recreate_interval_env_var_override_parses() {
        let _guard = ENV_VAR_LOCK.lock().unwrap();
        let saved = std::env::var("HUSH_WHISPER_STATE_RECREATE_INTERVAL").ok();

        unsafe { std::env::set_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL", "5") };
        assert_eq!(state_recreate_interval(), 5);

        unsafe { std::env::set_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL", "0") };
        assert_eq!(
            state_recreate_interval(),
            0,
            "0 must be honoured as the explicit 'never recreate' A/B knob"
        );

        // Garbage should fall back to the default rather than panic.
        unsafe { std::env::set_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL", "not-a-number") };
        assert_eq!(state_recreate_interval(), DEFAULT_STATE_RECREATE_INTERVAL);

        match saved {
            Some(prev) => unsafe {
                std::env::set_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL", prev)
            },
            None => unsafe { std::env::remove_var("HUSH_WHISPER_STATE_RECREATE_INTERVAL") },
        }
    }

    // -- VAD gate (#974) -----------------------------------------------
    //
    // The gate sits inside `WhisperStreamingSession::drain`. Exercising
    // it without a real GGUF model relies on two test-only seams:
    //   * `WhisperStreamingSession::new_for_test` builds a session with
    //     `ctx = None` so `drain_with_inferer` can run the gate decision
    //     without ever touching whisper.cpp.
    //   * `set_last_speech_at_for_test` directly places the speech clock
    //     wherever the test wants it, so we don't need to feed real
    //     speech-positive frames + sleep.
    //
    // The four tests below pin the load-bearing properties: gate-on
    // when silent past hangover; gate-off when speech is present;
    // gate-off when silence is inside the hangover; and the framing
    // contract (residual carry-over + exactly one VAD call per full
    // frame).

    use crate::vad::test_mocks::{AlwaysSilenceVad, AlwaysSpeechVad, ScriptedVad};

    /// Counts how many times `infer` was called so the gate tests can
    /// assert "inference ran" vs "inference skipped" without depending
    /// on the segment payload.
    struct CountingInferer {
        calls: usize,
        segments_per_call: Vec<StreamSegment>,
    }
    impl WhisperLikeInferer for CountingInferer {
        fn infer(&mut self, _mono_16k_pcm: &[f32]) -> Result<Vec<StreamSegment>> {
            self.calls += 1;
            Ok(self.segments_per_call.clone())
        }
    }

    fn meeting_capture_format() -> CaptureFormat {
        // 16 kHz mono — matches the policy's internal rate so `feed`
        // does no resampling. Lets the test's sample count map 1:1 to
        // milliseconds and to VAD frames.
        CaptureFormat {
            sample_rate: WHISPER_SAMPLE_RATE,
            channels: 1,
        }
    }

    /// Build a streaming-policy config that lets `tick` infer over a
    /// short window. Mirrors `streaming::tests::config_for_test`'s
    /// shape so the gate tests don't fight the policy's min-window /
    /// commit-tail thresholds.
    fn vad_gate_streaming_config() -> SlidingWindowConfig {
        // 500 ms min-first + 1 s infer interval + 2 s commit-tail is
        // sufficient for the gate tests; they feed 1 s of audio.
        SlidingWindowConfig {
            window_max_ms: 6_000,
            infer_interval_ms: 1_000,
            commit_tail_ms: 2_000,
            min_first_inference_ms: 500,
            ..SlidingWindowConfig::meeting_defaults()
        }
    }

    fn one_second_of_speech() -> Vec<f32> {
        vec![0.1_f32; 16_000]
    }

    #[test]
    fn vad_all_speech_does_not_gate_inference() {
        // With every VAD frame reporting speech, `feed` updates the
        // speech clock on every call; `drain` finds the clock fresh
        // and dispatches to the inferer. Pins the must-not-gate path
        // so a future refactor that flips the default direction is
        // caught immediately.
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSpeechVad),
        );
        session.feed(&one_second_of_speech()).unwrap();
        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
                ..Default::default()
            }],
        };
        let _ = session.drain_with_inferer(&mut inferer).unwrap();
        assert!(
            inferer.calls >= 1,
            "AlwaysSpeechVad must not gate inference; calls = {}",
            inferer.calls
        );
    }

    #[test]
    fn vad_all_silence_after_hangover_skips_inference() {
        // Place the speech clock well past the hangover. `drain` must
        // short-circuit before the inferer is touched. Pins the
        // load-bearing win of the gate — the whole point of #974.
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSilenceVad),
        );
        session.feed(&one_second_of_speech()).unwrap();
        let past = std::time::Instant::now()
            .checked_sub(2 * session.vad_hangover_for_test())
            .expect("Instant arithmetic doesn't underflow on any sane system");
        session.set_last_speech_at_for_test(Some(past));

        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![],
        };
        let out = session.drain_with_inferer(&mut inferer).unwrap();
        assert!(out.is_empty(), "gated drain must return no utterances");
        assert_eq!(
            inferer.calls, 0,
            "inferer must not be invoked when the gate fires"
        );
    }

    #[test]
    fn vad_speech_then_silence_inside_hangover_still_infers() {
        // Speech clock is recent but not stale; `drain` is inside the
        // hangover window and must still dispatch. Pins the "don't
        // chop off the trailing audio after the last speech ends"
        // property — the hangover exists to let whisper run on the
        // final utterance's tail before silence settles in.
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSilenceVad),
        );
        session.feed(&one_second_of_speech()).unwrap();
        let inside = std::time::Instant::now()
            .checked_sub(
                session
                    .vad_hangover_for_test()
                    .saturating_sub(std::time::Duration::from_millis(500)),
            )
            .expect("inside-hangover Instant arithmetic is well-defined");
        session.set_last_speech_at_for_test(Some(inside));

        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "hello".into(),
                ..Default::default()
            }],
        };
        let _ = session.drain_with_inferer(&mut inferer).unwrap();
        assert!(
            inferer.calls >= 1,
            "drain inside the hangover must still infer; calls = {}",
            inferer.calls
        );
    }

    #[test]
    fn vad_feed_chunks_in_frame_len_groups_and_handles_residual() {
        // Feed a non-multiple of FRAME_LEN_SAMPLES in two calls; the
        // VAD should be invoked exactly once per *complete* frame and
        // the leftover samples should carry over to the next feed.
        // Pins the framing contract — Silero requires exact 512-sample
        // frames at 16 kHz and any drift between feed chunks and frame
        // boundaries would silently corrupt its hidden state in Task 3.
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let scripted = ScriptedVad {
            probs: std::collections::VecDeque::new(),
            calls: std::sync::Arc::clone(&calls),
        };
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(scripted),
        );

        let frame_len = crate::vad::FRAME_LEN_SAMPLES;
        // 1.5 frames in the first feed — one full frame consumed, half
        // a frame carried as residual.
        let first = vec![0.0_f32; frame_len + frame_len / 2];
        session.feed(&first).unwrap();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "first feed: only one full frame should have been scored"
        );

        // 0.5 frame in the second feed — combines with the residual to
        // form exactly one more full frame.
        let second = vec![0.0_f32; frame_len / 2];
        session.feed(&second).unwrap();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "second feed: residual + new samples should yield one more frame"
        );
    }

    #[test]
    fn vad_gate_close_flushes_in_flight_segments() {
        // Pin the load-bearing flush behaviour (#974 follow-up): when
        // the gate transitions from "running inference" to "gating",
        // the first gated drain fires `state.tick_flush` to commit any
        // mid-flight utterance that hasn't crossed `commit_tail_ms`
        // yet. Without this the 30 s `window_max_ms` head-slide would
        // strand the user's last sentence after a long silence.
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSilenceVad),
        );
        // 1 s of audio with the VAD reporting silence. We override
        // `last_speech_at` for the first drain so the gate is open,
        // then for the second drain so the gate has just closed.
        session.feed(&one_second_of_speech()).unwrap();
        session.set_last_speech_at_for_test(Some(std::time::Instant::now()));

        // Drain 1: gate open, inferer returns a segment that's "young"
        // (1 s end with commit_tail_ms = 2 s). Normal `tick` emits it
        // as a partial.
        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "stranded sentence".into(),
                ..Default::default()
            }],
        };
        let first = session.drain_with_inferer(&mut inferer).unwrap();
        assert_eq!(inferer.calls, 1, "first drain runs inference");
        let partials_first: Vec<_> = first.iter().filter(|u| !u.is_final).collect();
        assert_eq!(
            partials_first.len(),
            1,
            "young segment surfaces as partial under normal tick"
        );

        // Feed another second so the interval gate opens on drain 2.
        session.feed(&one_second_of_speech()).unwrap();
        // Now place the speech clock past the hangover — gate closes.
        let past = std::time::Instant::now()
            .checked_sub(2 * session.vad_hangover_for_test())
            .expect("Instant arithmetic doesn't underflow on any sane system");
        session.set_last_speech_at_for_test(Some(past));

        // Drain 2: gate just closed AND was_inferring=true from drain 1
        // — flush must run, committing the in-flight segment as final.
        let flushed = session.drain_with_inferer(&mut inferer).unwrap();
        assert_eq!(
            inferer.calls, 2,
            "gate-close flush must invoke the inferer exactly once"
        );
        let finals: Vec<_> = flushed.iter().filter(|u| u.is_final).collect();
        assert_eq!(
            finals.len(),
            1,
            "flush must commit the in-flight segment as final, not partial"
        );
        assert_eq!(finals[0].text, "stranded sentence");
    }

    #[test]
    fn vad_gate_close_flush_fires_exactly_once_then_skips() {
        // The flush is exactly-once-per-gate-close: subsequent gated
        // drains must not invoke the inferer until speech returns and
        // a new run of inferences begins. Pins the discipline that the
        // flush doesn't degrade to "infer on every gated drain", which
        // would re-introduce the hallucination cost the gate exists to
        // prevent.
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSilenceVad),
        );
        session.feed(&one_second_of_speech()).unwrap();
        session.set_last_speech_at_for_test(Some(std::time::Instant::now()));

        // Drain 1: gate open, inference runs.
        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "in flight".into(),
                ..Default::default()
            }],
        };
        let _ = session.drain_with_inferer(&mut inferer).unwrap();
        assert_eq!(inferer.calls, 1, "drain 1 runs inference");

        // Feed more audio so the interval gate is open for drain 2.
        session.feed(&one_second_of_speech()).unwrap();
        // Close the gate.
        let past = std::time::Instant::now()
            .checked_sub(2 * session.vad_hangover_for_test())
            .expect("hangover-relative Instant subtraction is well-defined");
        session.set_last_speech_at_for_test(Some(past));

        // Drain 2: first gated drain after a run — flush fires.
        let _ = session.drain_with_inferer(&mut inferer).unwrap();
        assert_eq!(inferer.calls, 2, "drain 2 fires the flush");

        // Drain 3+: still gated, no new speech — flush must NOT fire
        // again. Feed audio between calls to clear the interval-gate
        // skip path; the flush should still be suppressed.
        for _ in 0..3 {
            session.feed(&one_second_of_speech()).unwrap();
            let out = session.drain_with_inferer(&mut inferer).unwrap();
            assert!(out.is_empty(), "subsequent gated drains return empty");
        }
        assert_eq!(
            inferer.calls, 2,
            "flush must fire exactly once per gate-close, not on every drain"
        );
    }

    /// Smoke test that requires a real GGUF model. Ignored by default; run
    /// with `cargo test --features whisper -- --ignored` after dropping a
    /// model file at the path indicated by the `HUSH_TEST_MODEL` env var.
    #[test]
    #[ignore = "requires HUSH_TEST_MODEL env var pointing at a real GGUF file"]
    fn end_to_end_transcribes_silence() {
        let path = std::env::var("HUSH_TEST_MODEL")
            .expect("set HUSH_TEST_MODEL to a path to a whisper GGUF file");
        let transcriber = WhisperTranscription::new(path).expect("model load");

        // One second of silence at 16 kHz mono. We expect the model to
        // produce either an empty string or a non-speech token; either way
        // it should not error.
        let audio = CapturedAudio {
            samples: vec![0.0_f32; 16_000],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        };
        let _ = transcriber.transcribe(&audio).expect("inference");
    }

    /// `share_context` must hand back the SAME weights (no second copy)
    /// behind an INDEPENDENT gate (dictation never queues behind the
    /// meeting pump, #248), with the slider atomics shared.
    #[test]
    #[ignore = "requires HUSH_TEST_MODEL env var pointing at a real GGUF file"]
    fn share_context_shares_weights_but_not_the_gate() {
        let path = std::env::var("HUSH_TEST_MODEL")
            .expect("set HUSH_TEST_MODEL to a path to a whisper GGUF file");
        let dictation = WhisperTranscription::new(path).expect("model load");
        let meeting = dictation.share_context();

        assert!(Arc::ptr_eq(&dictation.ctx.ctx, &meeting.ctx.ctx));
        assert!(!Arc::ptr_eq(&dictation.ctx.gate, &meeting.ctx.gate));
        assert!(Arc::ptr_eq(
            &dictation.inference_threads,
            &meeting.inference_threads
        ));
        assert!(Arc::ptr_eq(&dictation.mic_gain_db, &meeting.mic_gain_db));

        // Holding one transcriber's gate (an in-flight meeting tick) must
        // not block the other's (a dictation stop).
        let _meeting_tick = meeting.ctx.lock();
        assert!(dictation.ctx.gate.try_lock().is_ok());
    }

    // ---- VAD-boundary windowing (#1013) -----------------------------

    #[test]
    fn boundary_tracker_reports_onset_and_region_end() {
        let silence = ms_to_vad_frames(600);
        let mut t = BoundaryTracker::new(silence);
        // 5 silence frames, then 10 speech frames (onset at frame 5).
        for _ in 0..5 {
            assert_eq!(t.observe(false), None);
        }
        assert_eq!(
            t.observe(true),
            Some(BoundaryEvent::Onset(vad_frames_to_ms(5)))
        );
        for _ in 0..9 {
            assert_eq!(t.observe(true), None);
        }
        // Last speech frame = 14. Silence until the threshold is met.
        let mut ev = None;
        for _ in 0..silence {
            ev = t.observe(false);
        }
        assert_eq!(
            ev,
            Some(BoundaryEvent::Boundary(
                vad_frames_to_ms(15) + VAD_BOUNDARY_TAIL_PAD_MS
            ))
        );
        // Region is closed: more silence reports nothing.
        assert_eq!(t.observe(false), None);
    }

    #[test]
    fn boundary_tracker_ignores_short_blips() {
        let silence = ms_to_vad_frames(600);
        let mut t = BoundaryTracker::new(silence);
        assert!(matches!(t.observe(true), Some(BoundaryEvent::Onset(_))));
        t.observe(true);
        for _ in 0..silence + 5 {
            assert_eq!(t.observe(false), None, "a 2-frame blip must not commit");
        }
    }

    #[test]
    fn boundary_tracker_brief_dip_does_not_split_region() {
        let silence = ms_to_vad_frames(600);
        let mut t = BoundaryTracker::new(silence);
        for _ in 0..10 {
            t.observe(true);
        }
        for _ in 0..silence - 1 {
            assert_eq!(t.observe(false), None);
        }
        // Speech resumes just before the threshold — same region, no
        // new onset.
        assert_eq!(t.observe(true), None);
    }

    #[test]
    fn boundary_mode_trims_leading_silence_and_commits_region() {
        // ~1 s silence, ~1.5 s speech, ~0.8 s silence, as Silero would
        // score it.
        let mut probs = std::collections::VecDeque::new();
        probs.extend(std::iter::repeat(0.0).take(31));
        probs.extend(std::iter::repeat(0.9).take(47));
        probs.extend(std::iter::repeat(0.0).take(25));
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            SlidingWindowConfig {
                commit_tail_ms: 10_000,
                ..vad_gate_streaming_config()
            },
            Box::new(ScriptedVad {
                probs,
                calls: std::sync::Arc::clone(&calls),
            }),
        );
        session.boundary = Some(BoundaryTracker::new(ms_to_vad_frames(600)));

        let frames = 31 + 47 + 25;
        session
            .feed(&vec![0.1_f32; frames * crate::vad::FRAME_LEN_SAMPLES])
            .unwrap();
        // Leading silence trimmed to onset (31 frames ≈ 992 ms) − 300 ms pad.
        assert_eq!(
            session.state.window_start_ms_for_test(),
            vad_frames_to_ms(31) - VAD_BOUNDARY_LEAD_PAD_MS
        );
        assert!(session.pending_boundary_ms.is_some());

        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 100,
                end_ms: 1_500,
                text: "hello from the region".into(),
                ..Default::default()
            }],
        };
        let out = session.drain_with_inferer(&mut inferer).unwrap();
        assert_eq!(inferer.calls, 1);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_final, "a closed region commits as final");
        assert_eq!(out[0].text, "hello from the region");
        assert!(session.pending_boundary_ms.is_none());
        assert!(
            !session.was_inferring,
            "no gate-close flush over the trailing silence"
        );
    }

    #[test]
    fn boundary_tracker_does_not_trim_over_an_uncommitted_blip() {
        let silence = ms_to_vad_frames(600);
        let mut t = BoundaryTracker::new(silence);
        // Short blip (below the region minimum) …
        assert!(matches!(t.observe(true), Some(BoundaryEvent::Onset(_))));
        t.observe(true);
        for _ in 0..silence + 2 {
            t.observe(false);
        }
        // … then real speech: no Onset, because trimming would drop
        // the blip that never got its own boundary.
        assert_eq!(t.observe(true), None);
        assert!(t.has_uncommitted_speech());
    }

    #[test]
    fn boundary_tracker_trims_again_after_a_committed_region() {
        let silence = ms_to_vad_frames(600);
        let mut t = BoundaryTracker::new(silence);
        for _ in 0..10 {
            t.observe(true);
        }
        let mut saw_boundary = false;
        for _ in 0..silence {
            saw_boundary |= matches!(t.observe(false), Some(BoundaryEvent::Boundary(_)));
        }
        assert!(saw_boundary);
        assert!(!t.has_uncommitted_speech());
        assert!(matches!(t.observe(true), Some(BoundaryEvent::Onset(_))));
    }

    #[test]
    fn finish_skips_tail_decode_when_only_silence_remains() {
        let mut st = SlidingWindowState::new(WHISPER_SAMPLE_RATE, vad_gate_streaming_config());
        st.feed_mono(&vec![0.0_f32; 16_000 * 3]);
        let mut inferer = CountingInferer {
            calls: 0,
            segments_per_call: vec![StreamSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "Thank you.".into(),
                ..Default::default()
            }],
        };
        let out = finish_with_boundary(&mut st, &mut inferer, None, Some(false)).unwrap();
        assert!(out.is_empty());
        assert_eq!(inferer.calls, 0, "no decode over trailing silence");
    }

    #[test]
    fn finish_runs_tail_decode_when_boundary_mode_off_or_speech_left() {
        for speech_left in [None, Some(true)] {
            let mut st = SlidingWindowState::new(WHISPER_SAMPLE_RATE, vad_gate_streaming_config());
            st.feed_mono(&vec![0.1_f32; 16_000 * 2]);
            let mut inferer = CountingInferer {
                calls: 0,
                segments_per_call: vec![StreamSegment {
                    start_ms: 0,
                    end_ms: 1_000,
                    text: "tail words".into(),
                    ..Default::default()
                }],
            };
            let out = finish_with_boundary(&mut st, &mut inferer, None, speech_left).unwrap();
            assert_eq!(out.len(), 1, "{speech_left:?}");
        }
    }

    #[test]
    fn boundary_mode_on_by_default() {
        let session = WhisperStreamingSession::new_for_test(
            meeting_capture_format(),
            vad_gate_streaming_config(),
            Box::new(AlwaysSpeechVad),
        );
        // HUSH_VAD_BOUNDARY isn't set by any test; boundary windowing is
        // the default since the #1013 follow-up (`=0` opts out).
        assert!(session.boundary.is_some());
    }
}
