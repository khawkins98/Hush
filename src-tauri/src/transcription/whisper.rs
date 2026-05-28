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
//! ## Why `Mutex<WhisperContext>` rather than per-call construction
//!
//! Loading a GGUF file is the most expensive thing whisper.cpp does — order
//! of seconds for `base`, tens of seconds for `large-v3`. We pay it once at
//! construction and serialise inference behind a mutex. The mutex is fine
//! because dictation is fundamentally serial (one mic, one user, one
//! transcript at a time); the IPC layer can wrap us in an `Arc` and hand it
//! to multiple Tauri command handlers without contention concerns.
//!
//! ## Threading
//!
//! `whisper.cpp` is not internally thread-safe across `whisper_full` calls
//! on the same context. We hold the mutex across the full inference call.
//! Inference itself is configured to use the worker pool whisper.cpp manages
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

/// `whisper-rs` backed implementation of [`Transcribe`].
///
/// Construct with [`WhisperTranscription::new`]; the constructor loads the
/// model (a one-time multi-second cost on cold start) and the resulting
/// handle can transcribe many recordings in succession.
///
/// The context is held behind an `Arc<Mutex<...>>` so the streaming
/// session ([`WhisperStreamingSession`]) can hold its own clone of the
/// handle and run inferences from a different thread (the meeting
/// pump's blocking pool). The mutex serialises whisper.cpp calls; the
/// `Arc` shares ownership across the legacy one-shot path and any
/// number of concurrent streaming sessions (one per active meeting
/// audio source).
pub struct WhisperTranscription {
    /// Loaded GGUF model. Held behind an `Arc<Mutex>` because:
    /// (a) `whisper.cpp` is not safe to call concurrently on the same
    /// context (see module note) — the mutex enforces serialisation
    /// across the dictation hot path and any in-flight streaming
    /// sessions, and
    /// (b) the streaming session needs to outlive the borrow that
    /// produced it (the meeting pump moves it across `spawn_blocking`
    /// boundaries) — the `Arc` handles that without `'static`-bound
    /// trait method gymnastics.
    ctx: Arc<Mutex<WhisperContext>>,
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
}

impl LoadedContext {
    fn into_owned(self, model_path: PathBuf) -> Result<WhisperTranscription> {
        Ok(WhisperTranscription {
            ctx: Arc::new(Mutex::new(self.ctx)),
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

        // Acquire the context for the duration of inference. A poisoned
        // mutex here means a previous call panicked mid-inference; we
        // surface that as a regular error rather than re-panicking, since a
        // failed transcription should not take the whole app down.
        //
        // The context is wrapped in `Arc<Mutex<...>>` since #108
        // (streaming) — `lock()` is identical to the bare `Mutex`
        // shape, so the rest of the run_inference body is unchanged.
        let ctx = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper context mutex poisoned"))?;

        // `create_state` is required per-call: the state holds the decoder
        // KV cache, which must not be shared across concurrent inferences
        // (the mutex covers concurrency, but a fresh state also avoids
        // cross-utterance leakage of attention state).
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
        // All meeting streaming sessions share this one Arc<Mutex<WhisperContext>>.
        // `infer` and `finish` hold the lock across the entire inference with no early
        // drop, so a live meeting and a finalizing meeting sharing it would freeze
        // the live transcript behind the old `finish()` for up to 60 s. This shared
        // Arc is therefore the load-bearing constraint that defers concurrent meetings
        // in v1 — `SessionManager::start_manual` awaits any in-flight finalization
        // before opening a new session. To enable concurrency, give each session (or
        // finalization) its own WhisperContext. See learnings.md 2026-05-26
        // "Deferred: concurrent meetings" for the resume guide.
        let session = WhisperStreamingSession::new(
            Arc::clone(&self.ctx),
            format,
            prompt.to_owned(),
            SlidingWindowConfig::meeting_defaults(),
            Arc::clone(&self.inference_threads),
            vad_session,
        );
        Ok(Box::new(session))
    }
}

/// Streaming session backed by `whisper-rs` sliding-window inference.
///
/// Holds:
/// - A clone of the parent [`WhisperTranscription`]'s `Arc<Mutex<WhisperContext>>`
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
    ctx: Option<Arc<Mutex<WhisperContext>>>,
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
}

impl WhisperStreamingSession {
    fn new(
        ctx: Arc<Mutex<WhisperContext>>,
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
        while self.vad_residual.len() - offset >= frame_len {
            let frame = &self.vad_residual[offset..offset + frame_len];
            match self.vad_session.score_frame(frame) {
                Ok(prob) if prob >= self.vad_threshold => {
                    self.last_speech_at = Some(std::time::Instant::now());
                }
                Ok(_) => {}
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
                }
            }
            offset += frame_len;
        }
        self.vad_residual.drain(..offset);
    }

    /// Test-only drain that runs the gate against an arbitrary
    /// inferer. Lets the VAD-gate tests assert "inferer was / was not
    /// invoked" without constructing a real `WhisperContext`. The
    /// production [`StreamingTranscribeSession::drain`] runs the same
    /// gate check then dispatches against a real `WhisperInferer`
    /// built from `self.ctx`.
    #[cfg(test)]
    pub(super) fn drain_with_inferer(
        &mut self,
        inferer: &mut dyn WhisperLikeInferer,
    ) -> Result<Vec<Utterance>> {
        if self.should_gate() {
            return Ok(Vec::new());
        }
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
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        // VAD gate (#974): skip inference entirely when no recent
        // speech is within the hangover window. Whisper.cpp is the
        // expensive bit; skipping it on silence is the load-bearing
        // win — preventing hallucinations on non-speech windows like
        // a Zoom hold beep or a typing sound.
        if self.should_gate() {
            return Ok(Vec::new());
        }
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
        let mut inferer = WhisperInferer {
            ctx,
            prompt: &self.prompt,
            inference_threads: Arc::clone(&self.inference_threads),
            whisper_state: &mut self.whisper_state,
            inferences_on_current_state: &mut self.inferences_on_current_state,
            state_recreate_interval: self.state_recreate_interval,
        };
        self.state.finish(&mut inferer)
    }
}

/// Adapter that plugs whisper.cpp inference into the
/// [`WhisperLikeInferer`] trait the policy state machine calls. Lives
/// here (not in `streaming.rs`) so the policy module can be tested
/// without the `whisper` Cargo feature.
struct WhisperInferer<'a> {
    ctx: Arc<Mutex<WhisperContext>>,
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
        if !self.prompt.is_empty() {
            params.set_initial_prompt(self.prompt);
        }

        let ctx = self
            .ctx
            .lock()
            .map_err(|_| anyhow!("whisper context mutex poisoned"))?;
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
            out.push(StreamSegment {
                start_ms,
                end_ms,
                text,
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
    use super::*;
    use crate::audio::CaptureFormat;
    use std::sync::Mutex;

    // Tests that mutate HUSH_WHISPER_STATE_RECREATE_INTERVAL must hold this
    // lock for the full read-mutate-restore cycle; Rust test threads run in
    // parallel and a remove_var in one test can race a set_var in another.
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

    use crate::vad::VadSession;

    /// VAD that always reports speech — every frame's probability is
    /// 1.0, so `drain` should never gate.
    struct AlwaysSpeechVad;
    impl VadSession for AlwaysSpeechVad {
        fn score_frame(&mut self, _frame: &[f32]) -> Result<f32> {
            Ok(1.0)
        }
    }

    /// VAD that always reports silence — every frame's probability is
    /// 0.0, so once the hangover elapses `drain` must gate.
    struct AlwaysSilenceVad;
    impl VadSession for AlwaysSilenceVad {
        fn score_frame(&mut self, _frame: &[f32]) -> Result<f32> {
            Ok(0.0)
        }
    }

    /// VAD whose probabilities follow a scripted sequence, defaulting
    /// to 0.0 when the queue is exhausted. Counts every call so the
    /// framing test can assert "exactly one call per full frame".
    /// Uses an `Arc<AtomicUsize>` rather than `Rc<RefCell<usize>>`
    /// because `VadSession: Send` requires the impl be thread-safe.
    struct ScriptedVad {
        probs: std::collections::VecDeque<f32>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl VadSession for ScriptedVad {
        fn score_frame(&mut self, _frame: &[f32]) -> Result<f32> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.probs.pop_front().unwrap_or(0.0))
        }
    }

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
}
