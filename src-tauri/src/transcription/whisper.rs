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
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::{downmix_to_mono, CapturedAudio};
use crate::transcription::resample::resample_to_mono;
use crate::transcription::{Transcribe, WHISPER_SAMPLE_RATE};

/// Default thread count for whisper.cpp inference.
///
/// Whisper.cpp scales roughly linearly up to ~4 threads on Apple Silicon and
/// modern x86; beyond that the gains are small and the contention with the
/// UI thread starts to bite. M1 picks a fixed conservative value rather than
/// `num_cpus`-based auto-detection so behaviour is reproducible across
/// machines while we measure latency. The model picker (TODO M3) will expose
/// this as a setting.
const DEFAULT_INFERENCE_THREADS: i32 = 4;

/// `whisper-rs` backed implementation of [`Transcribe`].
///
/// Construct with [`WhisperTranscription::new`]; the constructor loads the
/// model (a one-time multi-second cost on cold start) and the resulting
/// handle can transcribe many recordings in succession.
pub struct WhisperTranscription {
    /// Loaded GGUF model. Held behind a mutex because `whisper.cpp` is not
    /// safe to call concurrently on the same context (see module note).
    ctx: Mutex<WhisperContext>,
    /// Where the model was loaded from. Kept for diagnostics — useful in
    /// error messages and the eventual settings panel.
    model_path: PathBuf,
}

impl WhisperTranscription {
    /// Load a GGUF model from `model_path` and return a ready-to-use handle.
    ///
    /// The path must point at a quantised GGUF file compatible with
    /// whisper.cpp (e.g. `ggml-base.q5_0.bin`). Auto-download and a model
    /// picker UI are deferred to M3 — for M1 the caller is responsible for
    /// supplying a path that exists.
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

impl LoadedContext {
    fn into_owned(self, model_path: PathBuf) -> Result<WhisperTranscription> {
        Ok(WhisperTranscription {
            ctx: Mutex::new(self.ctx),
            model_path,
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
        let pcm = Self::prepare_audio(audio)?;

        // Configure inference. Greedy with best_of=1 is the cheapest mode
        // and is sufficient for dictation; beam search is a quality/latency
        // trade we can expose later if user testing shows accuracy gains
        // worth the cost.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(DEFAULT_INFERENCE_THREADS);
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
    /// tree); the happy-path constructor is exercised manually until M3
    /// adds a managed test-model fixture.
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
