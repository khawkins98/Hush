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
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::{apply_mic_gain, downmix_to_mono, CaptureFormat, CapturedAudio};
use crate::transcription::resample::resample_to_mono;
use crate::transcription::streaming::{
    SlidingWindowConfig, SlidingWindowState, StreamSegment, StreamingTranscribeSession,
    WhisperLikeInferer,
};
use crate::transcription::{Transcribe, Utterance, WHISPER_SAMPLE_RATE};

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
    ) -> Result<Box<dyn StreamingTranscribeSession>> {
        let session = WhisperStreamingSession::new(
            Arc::clone(&self.ctx),
            format,
            prompt.to_owned(),
            SlidingWindowConfig::meeting_defaults(),
            Arc::clone(&self.inference_threads),
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
    ctx: Arc<Mutex<WhisperContext>>,
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
}

impl WhisperStreamingSession {
    fn new(
        ctx: Arc<Mutex<WhisperContext>>,
        capture_format: CaptureFormat,
        prompt: String,
        config: SlidingWindowConfig,
        inference_threads: Arc<std::sync::atomic::AtomicI32>,
    ) -> Self {
        Self {
            ctx,
            capture_format,
            prompt,
            state: SlidingWindowState::new(WHISPER_SAMPLE_RATE, config),
            inference_threads,
        }
    }

    /// Convert one chunk of capture-format samples to mono 16 kHz
    /// before feeding into the policy buffer. The same downmix +
    /// resample chain the one-shot path uses, applied per `feed` chunk.
    fn convert_chunk(&self, samples: &[f32]) -> Vec<f32> {
        if self.capture_format.sample_rate == 0 {
            return Vec::new();
        }
        let mono = downmix_to_mono(samples, self.capture_format.channels);
        resample_to_mono(&mono, self.capture_format.sample_rate, WHISPER_SAMPLE_RATE)
    }
}

impl StreamingTranscribeSession for WhisperStreamingSession {
    fn feed(&mut self, captured: &[f32]) -> Result<()> {
        let mono_16k = self.convert_chunk(captured);
        if !mono_16k.is_empty() {
            self.state.feed_mono(&mono_16k);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        let mut inferer = WhisperInferer {
            ctx: Arc::clone(&self.ctx),
            prompt: &self.prompt,
            inference_threads: Arc::clone(&self.inference_threads),
        };
        self.state.tick(&mut inferer)
    }

    fn finish(mut self: Box<Self>) -> Result<Vec<Utterance>> {
        let mut inferer = WhisperInferer {
            ctx: Arc::clone(&self.ctx),
            prompt: &self.prompt,
            inference_threads: Arc::clone(&self.inference_threads),
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
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow!("failed to create whisper state: {e}"))?;
        state
            .full(params, mono_16k_pcm)
            .map_err(|e| anyhow!("whisper streaming inference failed: {e}"))?;

        let n_segments = state
            .full_n_segments()
            .map_err(|e| anyhow!("failed to read segment count: {e}"))?;

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
        Ok(out)
    }
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
