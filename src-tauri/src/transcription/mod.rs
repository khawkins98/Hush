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
use serde::{Deserialize, Serialize};

use crate::audio::{CaptureFormat, CapturedAudio};

/// One transcribed utterance, the unit a streaming backend emits.
///
/// Mirrors the row shape that Phase C of the meeting-mode pivot will
/// persist (one utterance row per speaker turn, grouped by session —
/// see `docs/system-audio-meeting-mode-proposal.md`). For the existing
/// one-shot dictation flow there's exactly one utterance per call,
/// `is_final = true`, with timestamps spanning the whole recording —
/// the legacy `transcribe()` path constructs that shape via the
/// default [`Transcribe::transcribe_chunks`] impl.
///
/// `started_at_ms` and `ended_at_ms` are offsets from the start of
/// the streaming session, not wall-clock timestamps. The session
/// owner pairs these with a wall-clock anchor when persisting.
///
/// `speaker_label` is `None` until diarization (Phase D) lands; until
/// then every utterance reads as "unknown speaker" in the UI. Including
/// the field on the type today means the streaming-emitting backend
/// can be retrofitted without a wire-shape break.
///
/// `serde` derives so this can flow over the IPC boundary as soon as
/// Phase C wires the meeting-mode panel to receive partial utterances
/// via Tauri events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Utterance {
    /// Trimmed transcript text. Identical post-processing rules as
    /// the legacy `transcribe()` return value (leading/trailing
    /// whitespace stripped; Personal Dictionary replacements applied
    /// at the IPC layer downstream, not here).
    pub text: String,
    /// Offset from the streaming session's start, milliseconds.
    pub started_at_ms: u64,
    /// Offset from the streaming session's start, milliseconds.
    pub ended_at_ms: u64,
    /// Whether this utterance is settled. Streaming backends emit
    /// partial utterances with `is_final = false` while the model is
    /// still revising the tail; the consumer is expected to replace
    /// the previous non-final utterance for the same time range when
    /// a newer non-final one arrives, and lock it in when a final
    /// arrives. The legacy one-shot path always emits `is_final = true`.
    pub is_final: bool,
    /// Diarization label (`"Speaker A"`, `"Speaker B"`, or
    /// user-renamed). `None` until Phase D ships speaker
    /// segmentation. Single-speaker dictation use cases leave this
    /// `None` indefinitely — that's expected, not a regression.
    pub speaker_label: Option<String>,
}

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

    /// Run inference incrementally over a stream of audio chunks and
    /// emit one or more [`Utterance`]s with timestamps.
    ///
    /// **Default impl is the one-shot fallback**: concatenates every
    /// chunk into a single buffer, calls [`Self::transcribe_with_prompt`],
    /// and returns a single `is_final = true` utterance spanning the
    /// whole duration. This means the IPC layer can call
    /// `transcribe_chunks` uniformly even on backends that don't yet
    /// implement real streaming — they degrade gracefully to the same
    /// behaviour as the existing dictation hot path.
    ///
    /// Backends that support real streaming (whisper.cpp's sliding-
    /// window mode, Parakeet's frame-by-frame decoder) override this
    /// method AND override [`Self::supports_streaming`] to return
    /// `true` — the IPC layer reads the capability flag to decide
    /// whether to surface partial-utterance events to the frontend.
    ///
    /// Why a default impl rather than a separate trait:
    /// `Arc<dyn Transcribe>` is the existing IPC-layer trait object;
    /// adding a separate `StreamingTranscribe` would force the IPC
    /// layer to choose between holding two parallel object types or
    /// downcast at every dispatch. The default-impl approach lets
    /// the streaming entry point be called against any existing
    /// backend with no breakage.
    ///
    /// `chunks` are interleaved-channels f32 PCM at the rate carried
    /// in `format`; the implementation handles downmix and resampling
    /// the same way the one-shot path does. `prompt` is the
    /// vocabulary-bias initial prompt — same semantics as
    /// [`Self::transcribe_with_prompt`].
    fn transcribe_chunks(
        &self,
        chunks: &[Vec<f32>],
        format: CaptureFormat,
        prompt: &str,
    ) -> Result<Vec<Utterance>> {
        // Compute the duration for the single returned utterance's
        // end timestamp before we move samples into the joined buffer.
        // `samples / channels / rate * 1000` rounded to ms.
        let total_frames: u64 = chunks
            .iter()
            .map(|c| (c.len() as u64) / (format.channels.max(1) as u64))
            .sum();
        let duration_ms = if format.sample_rate == 0 {
            0
        } else {
            (total_frames * 1000) / (format.sample_rate as u64)
        };

        // Concatenate. Pre-reserving capacity avoids the reallocation
        // tail that would otherwise grow as ~O(N²) for many small
        // chunks (every push past capacity copies the whole buffer).
        let total_len: usize = chunks.iter().map(Vec::len).sum();
        let mut samples = Vec::with_capacity(total_len);
        for chunk in chunks {
            samples.extend_from_slice(chunk);
        }

        let audio = CapturedAudio { samples, format };
        let text = self.transcribe_with_prompt(&audio, prompt)?;

        // The default fallback only ever emits a single final
        // utterance — there's no partial-result loop in the
        // one-shot path.
        Ok(vec![Utterance {
            text,
            started_at_ms: 0,
            ended_at_ms: duration_ms,
            is_final: true,
            speaker_label: None,
        }])
    }

    /// Whether [`Self::transcribe_chunks`] emits incremental partial
    /// utterances during inference, or only a single final at the
    /// end (the default-impl fallback).
    ///
    /// Default returns `false`. Whisper.cpp's sliding-window mode and
    /// any future Parakeet ONNX backend would override to `true`.
    /// The IPC layer reads this flag to decide whether to forward a
    /// per-utterance Tauri event to the frontend (so the meeting-mode
    /// panel can render utterances as they finalize) or wait for the
    /// terminal one (the existing dictation flow).
    fn supports_streaming(&self) -> bool {
        false
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

    // -- transcribe_chunks default impl tests ------------------------
    //
    // The default impl is the one-shot fallback that future streaming
    // backends override. It must (a) concatenate the chunks correctly,
    // (b) compute the duration_ms field from the total sample count,
    // (c) forward the prompt to `transcribe_with_prompt`, and
    // (d) emit exactly one `is_final = true` utterance with no
    // speaker label. These tests pin each of those properties so a
    // future change that "tightens" the fallback can't accidentally
    // change observable behaviour for the existing dictation flow.

    /// Stub that records the audio + prompt it was called with so the
    /// tests can assert the default `transcribe_chunks` forwarded the
    /// data correctly.
    struct RecordingTranscribe {
        last_audio_len: std::sync::Mutex<Option<usize>>,
        last_prompt: std::sync::Mutex<Option<String>>,
        canned_text: String,
    }

    impl Transcribe for RecordingTranscribe {
        fn transcribe(&self, audio: &CapturedAudio) -> Result<String> {
            *self.last_audio_len.lock().unwrap() = Some(audio.samples.len());
            *self.last_prompt.lock().unwrap() = Some(String::new());
            Ok(self.canned_text.clone())
        }
        fn transcribe_with_prompt(&self, audio: &CapturedAudio, prompt: &str) -> Result<String> {
            *self.last_audio_len.lock().unwrap() = Some(audio.samples.len());
            *self.last_prompt.lock().unwrap() = Some(prompt.to_owned());
            Ok(self.canned_text.clone())
        }
    }

    fn fmt(rate: u32, channels: u16) -> CaptureFormat {
        CaptureFormat {
            sample_rate: rate,
            channels,
        }
    }

    #[test]
    fn transcribe_chunks_default_emits_single_final_utterance() {
        let backend = RecordingTranscribe {
            last_audio_len: std::sync::Mutex::new(None),
            last_prompt: std::sync::Mutex::new(None),
            canned_text: "hello world".into(),
        };

        // 16 kHz mono, two chunks totalling 1.0 s of audio (16_000
        // samples). 0.4 s + 0.6 s.
        let chunks = vec![vec![0.1_f32; 6_400], vec![0.2_f32; 9_600]];
        let utterances = backend
            .transcribe_chunks(&chunks, fmt(16_000, 1), "")
            .unwrap();

        assert_eq!(
            utterances.len(),
            1,
            "exactly one utterance from one-shot fallback"
        );
        assert!(utterances[0].is_final, "fallback always emits final");
        assert_eq!(utterances[0].text, "hello world");
        assert_eq!(utterances[0].started_at_ms, 0);
        assert_eq!(utterances[0].ended_at_ms, 1_000, "1.0 s of audio");
        assert!(
            utterances[0].speaker_label.is_none(),
            "diarization not yet shipped"
        );

        // Audio was concatenated, not lost.
        assert_eq!(*backend.last_audio_len.lock().unwrap(), Some(16_000));
    }

    #[test]
    fn transcribe_chunks_default_forwards_prompt() {
        let backend = RecordingTranscribe {
            last_audio_len: std::sync::Mutex::new(None),
            last_prompt: std::sync::Mutex::new(None),
            canned_text: "ok".into(),
        };
        let chunks = vec![vec![0.0_f32; 16_000]];
        backend
            .transcribe_chunks(&chunks, fmt(16_000, 1), "Hush, Tauri")
            .unwrap();
        assert_eq!(
            *backend.last_prompt.lock().unwrap(),
            Some("Hush, Tauri".to_owned()),
            "default impl must funnel the prompt to transcribe_with_prompt"
        );
    }

    #[test]
    fn transcribe_chunks_default_handles_stereo_duration_correctly() {
        // Two interleaved channels at 48 kHz: 96_000 samples → 1.0 s.
        // The wrong formula (samples / rate, ignoring channels) would
        // give 2.0 s. Pinned because every future streaming backend's
        // duration logic should converge on this same arithmetic.
        let backend = RecordingTranscribe {
            last_audio_len: std::sync::Mutex::new(None),
            last_prompt: std::sync::Mutex::new(None),
            canned_text: "".into(),
        };
        let chunks = vec![vec![0.0_f32; 96_000]];
        let utterances = backend
            .transcribe_chunks(&chunks, fmt(48_000, 2), "")
            .unwrap();
        assert_eq!(utterances[0].ended_at_ms, 1_000);
    }

    #[test]
    fn transcribe_chunks_default_handles_empty_chunk_list_without_panic() {
        // Zero chunks → empty buffer → zero duration. The downstream
        // `transcribe_with_prompt` is still called; backends that
        // panic on empty audio would surface that here. The default
        // path should not introduce a divide-by-zero or panic of
        // its own.
        let backend = RecordingTranscribe {
            last_audio_len: std::sync::Mutex::new(None),
            last_prompt: std::sync::Mutex::new(None),
            canned_text: "".into(),
        };
        let utterances = backend.transcribe_chunks(&[], fmt(16_000, 1), "").unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].ended_at_ms, 0);
        assert_eq!(*backend.last_audio_len.lock().unwrap(), Some(0));
    }

    #[test]
    fn supports_streaming_default_is_false() {
        // Pinned for symmetry with `supports_prompt_biasing` — a
        // future trait change that flips the default would let a
        // backend silently signal "I do streaming" when its
        // transcribe_chunks impl is just the one-shot fallback. The
        // IPC layer would then forward partial-utterance Tauri events
        // for utterances that were never actually partial.
        struct Stub;
        impl Transcribe for Stub {
            fn transcribe(&self, _: &CapturedAudio) -> Result<String> {
                Ok(String::new())
            }
        }
        assert!(!Stub.supports_streaming());
    }

    #[test]
    fn utterance_serde_uses_camel_case_for_frontend_consumption() {
        // The Phase C meeting-mode panel will receive these via
        // Tauri events. Pin the wire shape so a Rust-side rename
        // fails loud rather than silently drifting from the
        // frontend's TypeScript view.
        let u = Utterance {
            text: "hello".into(),
            started_at_ms: 100,
            ended_at_ms: 1_500,
            is_final: true,
            speaker_label: Some("Speaker A".into()),
        };
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains(r#""startedAtMs":100"#), "got: {json}");
        assert!(json.contains(r#""endedAtMs":1500"#), "got: {json}");
        assert!(json.contains(r#""isFinal":true"#), "got: {json}");
        assert!(
            json.contains(r#""speakerLabel":"Speaker A""#),
            "got: {json}"
        );

        // Round-trip pins the deserialiser too.
        let back: Utterance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, u);
    }
}
