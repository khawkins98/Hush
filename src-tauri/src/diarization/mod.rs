//! Speaker diarization seam.
//!
//! Per-speaker labels for utterances inside a meeting session. The
//! pre-#111 pump tagged every utterance with its capture source —
//! `"mic"` for the local user, `"system"` for remote participants on
//! a typical Zoom / Meet call. That is fine when the conversation
//! has exactly two distinguishable parties (you on mic, everyone
//! else lumped into "system"), but breaks down for any session with
//! more than one remote speaker — every remote utterance gets the
//! same `"system"` label and the panel can't render speaker turns.
//!
//! This module establishes a [`Diarize`] trait at the heavy-dep
//! boundary so the pump can ask "who said this?" without knowing
//! whether the answer comes from a silence-gap heuristic, a small
//! ONNX speaker-embedding model, or some future cloud diarizer.
//!
//! ## Phased delivery
//!
//! - **D1 — [`EnergyDiarizer`].** Silence-gap heuristic, no model.
//!   Splits a per-source utterance run into Speaker A / Speaker B
//!   by alternating-talker rule whenever the gap between consecutive
//!   utterances exceeds a threshold. Roughly 70% accurate on
//!   two-speaker conversations; cheap; ships before any model
//!   download.
//! - **D2 — model-based.** ONNX speaker-embedding model gated on the
//!   same SHA-verified download pipeline as Whisper models. Better
//!   accuracy; opt-in. Will need the trait to grow audio access (the
//!   D1 trait takes audio + format already so the signature stays
//!   stable).
//!
//! ## Why a trait, not a free function
//!
//! Same reason as [`crate::transcription::Transcribe`]: the
//! production impl is heavy (audio analysis + clustering, eventually
//! ONNX runtime), tests want determinism, and the IPC layer doesn't
//! want to know which one is wired. `Arc<dyn Diarize>` lives on
//! `AppState` and threads through the meeting `SessionManager` into
//! the pump's per-chunk dispatch.
//!
//! ## Production wiring
//!
//! [`NoopDiarizer`] is wired in production as of #243. The pump
//! runs every batch of finals through it, which leaves
//! `speaker_label = None`; `dispatch_utterances` then stamps the
//! source-derived `"mic"` / `"system"` tag so the panel renders
//! the You / Remote split.
//!
//! [`EnergyDiarizer`] (D1 silence-gap heuristic) is kept on disk
//! but not wired. Hands-on testing on a mic + system-audio
//! Meeting Mode session showed the cross-source merge collapsed
//! every utterance to "Speaker A" — the heuristic only works on
//! a single-stream mic recording, and Meeting Mode's whole point
//! is the multi-source case. The wiring change in `ipc/mod.rs`
//! has the full reasoning. D2 (model-based ONNX speaker
//! embeddings, #111) is the upgrade path that can actually
//! distinguish voices across sources.
//!
//! To trial `EnergyDiarizer` on a mic-only flow, swap it back in
//! at `ipc/mod.rs::AppStateBuilder::build_default` (the comment
//! there carries the toggle instructions).

use crate::audio::CaptureFormat;
use crate::transcription::Utterance;

pub mod catalog;
pub mod cluster;
// `features` (Mel-Filterbank extraction) and `onnx` are only used
// by the OnnxDiarizer impl. Gating both behind the
// `diarization-onnx` feature keeps `realfft` (the only dep used
// by `features`) out of `--no-default-features` builds. Audit
// review of the #111 chain flagged the unconditional `realfft`
// pull as wasted build cost when the diarizer feature is off.
#[cfg(feature = "diarization-onnx")]
pub mod features;
#[cfg(feature = "diarization-onnx")]
pub mod onnx;

/// Tag a batch of utterances with speaker labels in place.
///
/// Called by the meeting pump after each batch of finals lands from
/// the streaming inference session, before the source-derived
/// (`"mic"` / `"system"`) label is stamped. An impl that wants to
/// override the source-derived label sets `speaker_label = Some(...)`
/// on each utterance; the pump skips its own source stamp when the
/// label is already set.
///
/// `audio_chunks` is the per-utterance audio (parallel to
/// `utterances`) for impls that want to look at the signal directly
/// (D2's ONNX path). The D1 [`EnergyDiarizer`] ignores it — its
/// alternating-talker heuristic only needs the timestamps that are
/// already on each `Utterance`. Pass an empty slice when no audio
/// is available; the trait does not require the chunks to be
/// populated.
///
/// `format` describes the sample-rate / channel layout of every
/// chunk in `audio_chunks` (assumed homogeneous within a single
/// pump call). Ignored by D1; D2 needs it for STFT / feature
/// extraction.
pub trait Diarize: Send + Sync {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        format: CaptureFormat,
    );
}

/// Fallback impl. Leaves `speaker_label` as it is so the pump's
/// source-derived stamp (`"mic"` / `"system"`) wins via
/// `dispatch_utterances`'s `is_none` guard. Pre-#201 this was the
/// production wiring; post-#201 it stays as the swap-back option
/// for sessions where the user prefers source-only labels.
pub struct NoopDiarizer;

/// Hot-swappable diarizer slot (#301). AppState owns one of these
/// and hands an `Arc::clone` to [`FlagGatedDiarizer`]; the IPC
/// `download_diarizer_model` path replaces the inner Arc after a
/// successful download so the new `OnnxDiarizer` takes effect on
/// the next meeting tick — no app restart.
///
/// `RwLock<Arc<dyn Diarize>>` rather than `Mutex` because reads
/// happen on every meeting-pump tick and writes happen at most a
/// couple of times per app session (download / re-load). Reader
/// concurrency matters; writer contention doesn't.
pub type DiarizeSlot = std::sync::Arc<std::sync::RwLock<std::sync::Arc<dyn Diarize>>>;

/// Composite diarizer that routes to one of two inner impls based
/// on the `diarization_enabled` settings flag (#111).
///
/// The `AppState`'s `Arc<AtomicBool>` is shared with this struct,
/// so flips of the toggle in Settings → Meeting → Speakers take
/// effect on the *next* meeting tick — no session restart needed.
/// The `inner` slot is itself a [`DiarizeSlot`] so the IPC
/// download path can hot-swap the diarizer without rebuilding the
/// FlagGatedDiarizer.
///
/// Constructed in `AppStateBuilder::build_default`:
/// - `enabled` → `Arc::clone(&app_state.diarization_enabled)`
/// - `inner` → `Arc::clone(&app_state.diarize_slot)`. Initial
///   value is `OnnxDiarizer` if the wespeaker model is on disk +
///   the `diarization-onnx` feature is built in, else
///   `NoopDiarizer`.
/// - `fallback` → `NoopDiarizer` (always the safe default for the
///   off-state branch)
pub struct FlagGatedDiarizer {
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    inner: DiarizeSlot,
    fallback: std::sync::Arc<dyn Diarize>,
}

impl FlagGatedDiarizer {
    pub fn new(
        enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
        inner: DiarizeSlot,
        fallback: std::sync::Arc<dyn Diarize>,
    ) -> Self {
        Self {
            enabled,
            inner,
            fallback,
        }
    }
}

impl Diarize for FlagGatedDiarizer {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        format: CaptureFormat,
    ) {
        if self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            // Recover from poison rather than killing diarization
            // for the rest of the session — same shape as
            // OnnxDiarizer's session-mutex recovery.
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            inner.label_utterances(utterances, audio_chunks, format);
        } else {
            self.fallback
                .label_utterances(utterances, audio_chunks, format);
        }
    }
}

impl Diarize for NoopDiarizer {
    fn label_utterances(
        &self,
        _utterances: &mut [Utterance],
        _audio_chunks: &[Vec<f32>],
        _format: CaptureFormat,
    ) {
        // intentional no-op
    }
}

/// Default silence-gap threshold, in milliseconds. Gaps shorter than
/// this between consecutive utterances stay with the current
/// speaker; longer gaps flip to the other speaker.
///
/// 1.5 s is a compromise: shorter (≤500 ms) misclassifies natural
/// breath pauses as speaker turns; longer (≥3 s) misses fast back-
/// and-forth. Tuned against the alternating-talker assumption — once
/// a session has more than two participants, the heuristic is wrong
/// regardless of the threshold.
pub const DEFAULT_SILENCE_GAP_MS: u64 = 1500;

/// D1 diarizer. Uses the gap between consecutive utterance
/// timestamps to detect speaker turns; alternates Speaker A / Speaker
/// B starting from the first utterance.
///
/// **Accuracy**: ~70% on clean two-speaker conversations with
/// distinct turn-taking. Falls apart on:
/// - More than two speakers (everyone past A/B gets one of the two
///   labels at random).
/// - Overlapping speech (the pump's per-source batches don't surface
///   overlap timing cleanly).
/// - Monologues with long internal pauses (looks like a speaker turn
///   to the heuristic).
///
/// D2's model-based impl handles all three; D1 is a stop-gap.
pub struct EnergyDiarizer {
    silence_threshold_ms: u64,
}

impl Default for EnergyDiarizer {
    fn default() -> Self {
        Self {
            silence_threshold_ms: DEFAULT_SILENCE_GAP_MS,
        }
    }
}

impl EnergyDiarizer {
    /// Construct with a custom silence threshold. Mostly a test
    /// hook; production wiring uses [`Default`].
    pub fn with_silence_threshold_ms(silence_threshold_ms: u64) -> Self {
        Self {
            silence_threshold_ms,
        }
    }
}

impl Diarize for EnergyDiarizer {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        _audio_chunks: &[Vec<f32>],
        _format: CaptureFormat,
    ) {
        if utterances.is_empty() {
            return;
        }
        // Speaker labels are 1-indexed letters: "Speaker A", "Speaker
        // B". Two-speaker is the only case D1 handles — extending past
        // the 2-letter table would imply a model the trait doesn't
        // have access to yet.
        let mut current = 0u8; // 0 = A, 1 = B
        utterances[0].speaker_label = Some(label_for(current));

        for i in 1..utterances.len() {
            let prev_end = utterances[i - 1].ended_at_ms;
            let curr_start = utterances[i].started_at_ms;
            let gap = curr_start.saturating_sub(prev_end);
            if gap >= self.silence_threshold_ms {
                current ^= 1;
            }
            utterances[i].speaker_label = Some(label_for(current));
        }
    }
}

fn label_for(idx: u8) -> String {
    let ch = b'A' + idx;
    format!("Speaker {}", ch as char)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::CaptureFormat;
    use crate::transcription::Utterance;

    fn fmt() -> CaptureFormat {
        // The format is unused by D1 but the trait requires one;
        // 16 kHz mono is the canonical Whisper input shape.
        CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        }
    }

    fn utt(start_ms: u64, end_ms: u64, text: &str) -> Utterance {
        Utterance {
            text: text.to_owned(),
            started_at_ms: start_ms,
            ended_at_ms: end_ms,
            is_final: true,
            speaker_label: None,
        }
    }

    #[test]
    fn noop_leaves_labels_alone() {
        let mut us = vec![utt(0, 1000, "hello"), utt(2000, 3000, "world")];
        us[0].speaker_label = Some("mic".to_owned());
        NoopDiarizer.label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("mic"));
        assert_eq!(us[1].speaker_label.as_deref(), None);
    }

    #[test]
    fn energy_empty_input_is_noop() {
        let mut us: Vec<Utterance> = Vec::new();
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert!(us.is_empty());
    }

    #[test]
    fn energy_single_utterance_gets_speaker_a() {
        let mut us = vec![utt(0, 1000, "hi")];
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
    }

    #[test]
    fn energy_short_gap_keeps_same_speaker() {
        // 200 ms gap is a natural inter-utterance pause; should not
        // flip the speaker.
        let mut us = vec![utt(0, 1000, "first"), utt(1200, 2000, "second")];
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
        assert_eq!(us[1].speaker_label.as_deref(), Some("Speaker A"));
    }

    #[test]
    fn energy_long_gap_flips_speaker() {
        // 2 s gap exceeds the 1.5 s default — the second utterance is
        // attributed to a new speaker.
        let mut us = vec![utt(0, 1000, "first"), utt(3000, 4000, "second")];
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
        assert_eq!(us[1].speaker_label.as_deref(), Some("Speaker B"));
    }

    #[test]
    fn energy_alternates_back_after_third_gap() {
        // Three-utterance ABA pattern: long gap, long gap, both
        // flips, ending back at A.
        let mut us = vec![
            utt(0, 1000, "a1"),
            utt(3000, 4000, "b1"),
            utt(6000, 7000, "a2"),
        ];
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
        assert_eq!(us[1].speaker_label.as_deref(), Some("Speaker B"));
        assert_eq!(us[2].speaker_label.as_deref(), Some("Speaker A"));
    }

    #[test]
    fn energy_overrides_existing_labels() {
        // A pre-stamped source label (from an earlier dispatch path)
        // gets overwritten — D1 is the source of truth when wired.
        let mut us = vec![utt(0, 1000, "x")];
        us[0].speaker_label = Some("mic".to_owned());
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
    }

    #[test]
    fn energy_custom_threshold() {
        // 500 ms threshold — what was a "natural pause" at the 1.5 s
        // default is now a speaker turn.
        let mut us = vec![utt(0, 1000, "first"), utt(1700, 2500, "second")];
        EnergyDiarizer::with_silence_threshold_ms(500).label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
        assert_eq!(us[1].speaker_label.as_deref(), Some("Speaker B"));
    }

    #[test]
    fn energy_negative_gap_does_not_flip() {
        // Out-of-order utterances (curr.started_at_ms < prev.ended_at_ms)
        // — the streaming pump shouldn't emit these but the heuristic
        // shouldn't crash or flip on them. `saturating_sub` keeps the
        // gap at 0, which is well below threshold.
        let mut us = vec![utt(0, 5000, "long first"), utt(3000, 4000, "overlap")];
        EnergyDiarizer::default().label_utterances(&mut us, &[], fmt());
        assert_eq!(us[0].speaker_label.as_deref(), Some("Speaker A"));
        assert_eq!(us[1].speaker_label.as_deref(), Some("Speaker A"));
    }

    /// Sentinel diarizer that records whether it was called. Lets the
    /// FlagGatedDiarizer tests verify routing without standing up a
    /// real ONNX session.
    struct RecordingDiarizer {
        called: std::sync::atomic::AtomicBool,
    }

    impl Diarize for RecordingDiarizer {
        fn label_utterances(
            &self,
            _utterances: &mut [Utterance],
            _audio_chunks: &[Vec<f32>],
            _format: CaptureFormat,
        ) {
            self.called
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    #[test]
    fn flag_gated_routes_to_inner_when_enabled() {
        let inner = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let fallback = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let diarizer = FlagGatedDiarizer::new(
            enabled,
            std::sync::Arc::new(std::sync::RwLock::new(
                inner.clone() as std::sync::Arc<dyn Diarize>
            )),
            fallback.clone() as std::sync::Arc<dyn Diarize>,
        );
        let mut us = vec![utt(0, 1000, "x")];
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(
            inner.called.load(std::sync::atomic::Ordering::Relaxed),
            "inner should have been called when flag is on"
        );
        assert!(
            !fallback.called.load(std::sync::atomic::Ordering::Relaxed),
            "fallback should NOT have been called when flag is on"
        );
    }

    #[test]
    fn flag_gated_routes_to_fallback_when_disabled() {
        let inner = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let fallback = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let diarizer = FlagGatedDiarizer::new(
            enabled,
            std::sync::Arc::new(std::sync::RwLock::new(
                inner.clone() as std::sync::Arc<dyn Diarize>
            )),
            fallback.clone() as std::sync::Arc<dyn Diarize>,
        );
        let mut us = vec![utt(0, 1000, "x")];
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(
            !inner.called.load(std::sync::atomic::Ordering::Relaxed),
            "inner should NOT have been called when flag is off"
        );
        assert!(
            fallback.called.load(std::sync::atomic::Ordering::Relaxed),
            "fallback should have been called when flag is off"
        );
    }

    #[test]
    fn flag_gated_observes_runtime_flips() {
        // The whole point of an Arc<AtomicBool>: a single diarizer
        // instance must respect the flag changing across calls
        // without being rebuilt. Settings → toggle → next meeting
        // tick uses the new value.
        let inner = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let fallback = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let diarizer = FlagGatedDiarizer::new(
            std::sync::Arc::clone(&enabled),
            std::sync::Arc::new(std::sync::RwLock::new(
                inner.clone() as std::sync::Arc<dyn Diarize>
            )),
            fallback.clone() as std::sync::Arc<dyn Diarize>,
        );

        let mut us = vec![utt(0, 1000, "x")];
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(fallback.called.load(std::sync::atomic::Ordering::Relaxed));
        // Reset the recorder for the second pass.
        fallback
            .called
            .store(false, std::sync::atomic::Ordering::Relaxed);

        enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(
            inner.called.load(std::sync::atomic::Ordering::Relaxed),
            "after flipping flag on, inner takes over"
        );
        assert!(
            !fallback.called.load(std::sync::atomic::Ordering::Relaxed),
            "after flipping flag on, fallback is skipped"
        );
    }
}
