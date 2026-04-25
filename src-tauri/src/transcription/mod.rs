//! Local Whisper transcription pipeline.
//!
//! Concept inspired by VoiceInk's whisper.cpp Swift bridge.
//! Reimplemented from observed public behaviour; no source code referenced.
//! See §13.8 of the PRD.
//!
//! ## Responsibilities
//!
//! - Define the [`Transcribe`] trait at the OS / heavy-dep boundary so the
//!   IPC layer (TODO(#7)) can hold an `Arc<dyn Transcribe>` without knowing
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
//! - Personal Dictionary prompt biasing: TODO(#4).
//! - Tauri event progress reporting: deferred until the IPC layer exists.
//! - GPU acceleration features in `whisper-rs`: M1 is CPU-only.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on [`Transcribe`], never on the concrete
//! [`WhisperTranscription`] type, so unit tests of the IPC layer can
//! substitute a deterministic mock without pulling in whisper.cpp or a real
//! GGUF model.

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
/// Always compiled (no feature gate) so the IPC layer (TODO(#7)) can hold an
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
    /// replacements (TODO(#4)) live in a separate post-processing stage so
    /// the raw model output stays available for debugging and history.
    fn transcribe(&self, audio: &CapturedAudio) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that the trait is object-safe. If this ever fails
    /// to compile, a higher layer cannot store an `Arc<dyn Transcribe>`,
    /// which is how the IPC layer (TODO(#7)) plugs in either the whisper
    /// backend or a test mock.
    #[test]
    fn transcribe_trait_is_object_safe() {
        fn _assert_object_safe(_: &dyn Transcribe) {}
    }
}
