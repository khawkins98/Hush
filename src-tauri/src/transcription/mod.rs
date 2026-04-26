//! Local Whisper transcription pipeline.
//!
//! Concept inspired by VoiceInk's whisper.cpp Swift bridge.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Define the [`Transcribe`] trait at the OS / heavy-dep boundary so the
//!   IPC layer can hold an `Arc<dyn Transcribe>` without knowing
//!   whether the concrete backend is real, mocked, or absent at compile
//!   time.
//! - Provide a `whisper-rs` backed implementation behind the `whisper` Cargo
//!   feature, gated because the underlying whisper.cpp build needs `cmake`
//!   and a C++ toolchain that we deliberately do not require for the bulk of
//!   the codebase.
//! - Bridge the audio module's `CapturedAudio` (arbitrary sample rate, 1+
//!   channels) to whisper.cpp's required input format (16 kHz mono f32 PCM).
//!
//! ## Pipeline
//!
//! ```text
//! CapturedAudio → downmix to mono → resample to 16 kHz → whisper-rs → String
//! ```
//!
//! Downmix lives in [`crate::audio::downmix_to_mono`] (pure-logic, already
//! unit-tested). Resampling lives in [`resample`] in this module. The
//! whisper-rs glue is in [`whisper`] and is feature-gated.
//!
//! ## Out of scope for M1 (intentional)
//!
//! - Model auto-download, SHA verification, picker UI: deferred to M3. The
//!   constructor takes a caller-provided model path and lets the caller
//!   decide where the file came from.
//! - Personal Dictionary prompt biasing: TODO(#6).
//! - Tauri event progress reporting: deferred until the IPC layer exists.
//! - GPU acceleration features in `whisper-rs`: M1 is CPU-only.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on [`Transcribe`], never on the concrete
//! [`WhisperTranscription`] type, so unit tests of the IPC layer can
//! substitute a deterministic mock without pulling in whisper.cpp or a real
//! GGUF model.

pub mod catalog;
pub mod download;
pub mod resample;

#[cfg(feature = "whisper")]
pub mod whisper;

#[cfg(feature = "whisper")]
pub use whisper::WhisperTranscription;

use anyhow::Result;

use crate::audio::CapturedAudio;

/// Whisper.cpp's expected input sample rate. The library converts internally
/// to a mel spectrogram with fixed parameters, so any other rate must be
/// resampled before inference.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Trait at the OS / heavy-dep boundary.
///
/// Always compiled (no feature gate) so the IPC layer can hold an
/// `Arc<dyn Transcribe>` regardless of whether the `whisper` feature is on.
/// When the feature is off, the IPC layer is expected to plug in either a
/// hard-coded "transcription unavailable" backend or a test mock; in either
/// case the type system stays consistent.
///
/// Implementations are responsible for any format conversion required by
/// their underlying engine. Callers hand over [`CapturedAudio`] in whatever
/// native format the audio device produced; the implementation downmixes
/// and resamples as needed.
pub trait Transcribe: Send + Sync {
    /// Run inference over `audio` and return the recognised text.
    ///
    /// The returned string has been trimmed of leading and trailing
    /// whitespace but is otherwise unmodified — Personal Dictionary
    /// replacements (handled by [`crate::dictionary::apply_replacements`]
    /// at the IPC layer) live in a separate post-processing stage so
    /// the raw model output stays available for debugging and history.
    fn transcribe(&self, audio: &CapturedAudio) -> Result<String>;

    /// Run inference with an additional initial prompt — the
    /// vocabulary-biasing path. Backends that support prompt-biasing
    /// (whisper.cpp does, via `set_initial_prompt`) override to feed
    /// `prompt` to the decoder; backends that don't fall through to
    /// the no-prompt [`Self::transcribe`] via the default impl, so the
    /// IPC layer can call this method unconditionally without knowing
    /// which backend is plugged in.
    ///
    /// `prompt` is expected to be a comma-separated list of vocabulary
    /// terms in the form produced by
    /// [`crate::dictionary::format_vocabulary_prompt`]. Empty strings
    /// are treated as "no prompt" by every implementation.
    ///
    /// **Callers wanting to know whether the prompt was actually
    /// honoured** must check [`Self::supports_prompt_biasing`] —
    /// silently dropping the prompt (default impl) was a real
    /// problem before that method existed: a future ONNX or other
    /// non-Whisper backend would have the user's vocabulary terms
    /// silently produce zero effect, with no warning surfaced.
    fn transcribe_with_prompt(&self, audio: &CapturedAudio, _prompt: &str) -> Result<String> {
        self.transcribe(audio)
    }

    /// Whether [`Self::transcribe_with_prompt`] actually feeds `prompt`
    /// to the model, or silently drops it.
    ///
    /// Default returns `false` — safer to assume the prompt is dropped
    /// unless a backend opts in. The whisper-rs backend overrides to
    /// `true` because `whisper.cpp::set_initial_prompt` is a real
    /// signal into the decoder. A future ONNX/Parakeet backend that
    /// doesn't expose decoder prompting would inherit the `false`
    /// default and the IPC layer's vocabulary path would log a
    /// "vocabulary terms ignored" warning instead of pretending
    /// they took effect.
    ///
    /// This method exists so the IPC layer can disambiguate "no
    /// terms configured" from "terms configured but the backend
    /// can't bias". Without it, vocabulary terms could appear to
    /// work (no error) while having zero observable effect on
    /// transcripts — a silent UX bug we want to surface explicitly
    /// instead.
    fn supports_prompt_biasing(&self) -> bool {
        false
    }

    /// Short, human-readable identifier for the model running this
    /// transcriber, persisted on each history row so the user can later
    /// see which model produced a given transcript.
    ///
    /// Default returns `"unknown"`; the whisper-rs backend overrides
    /// with the model file's basename (e.g. `ggml-base.bin`). The
    /// catalog id (e.g. `whisper-base`) would be more user-friendly,
    /// but we don't currently thread it through the trait — a future
    /// refactor that gives the loader the catalog id at construction
    /// time can return that here.
    fn model_label(&self) -> String {
        "unknown".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that the trait is object-safe. If this ever fails
    /// to compile, a higher layer cannot store an `Arc<dyn Transcribe>`,
    /// which is how the IPC layer plugs in either the whisper backend or a
    /// test mock.
    #[test]
    fn transcribe_trait_is_object_safe() {
        fn _assert_object_safe(_: &dyn Transcribe) {}
    }

    /// Default trait method — `supports_prompt_biasing` returns false
    /// when not overridden. Pin so a future trait change doesn't
    /// silently flip the default and let vocabulary terms appear to
    /// work on backends that drop them.
    #[test]
    fn default_supports_prompt_biasing_is_false() {
        struct Stub;
        impl Transcribe for Stub {
            fn transcribe(&self, _audio: &CapturedAudio) -> Result<String> {
                Ok(String::new())
            }
        }
        assert!(!Stub.supports_prompt_biasing());
    }

    /// A backend that overrides to true is observably distinct. Pin
    /// so the IPC layer's `if !supports_prompt_biasing { warn }`
    /// branch is exercised correctly for both states.
    #[test]
    fn override_supports_prompt_biasing_returns_true() {
        struct PromptingBackend;
        impl Transcribe for PromptingBackend {
            fn transcribe(&self, _audio: &CapturedAudio) -> Result<String> {
                Ok(String::new())
            }
            fn supports_prompt_biasing(&self) -> bool {
                true
            }
        }
        assert!(PromptingBackend.supports_prompt_biasing());
    }
}
