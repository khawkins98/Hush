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
//! whether the answer comes from a silence-gap heuristic, an ONNX
//! speaker-embedding model, or some future cloud diarizer.
//!
//! ## Why a trait, not a free function
//!
//! Same reason as [`crate::transcription::Transcribe`]: the
//! production impl is heavy (ONNX runtime + clustering), tests want
//! determinism, and the IPC layer doesn't want to know which one
//! is wired. `Arc<dyn Diarize>` lives on `AppState` and threads
//! through the meeting `SessionManager` into the pump's per-chunk
//! dispatch.
//!
//! ## Production wiring
//!
//! [`FlagGatedDiarizer`] is the production wrapper. It reads the
//! `diarization_enabled` `AtomicBool` from `AppState` every pump
//! tick: when on, calls into the inner [`DiarizeSlot`]
//! ([`crate::diarization::onnx::OnnxDiarizer`] when the wespeaker
//! model is loaded, [`NoopDiarizer`] otherwise); when off, falls
//! through to the source-derived `"mic"` / `"system"` stamp so
//! the panel renders the You / Remote split.
//!
//! ## Removed in #310
//!
//! Pre-#310 this module also held an `EnergyDiarizer` D1 silence-
//! gap heuristic. It was wired in production briefly under #201
//! but reverted to `NoopDiarizer` in #243 — cross-source utterance
//! merging collapsed every label to "Speaker A" because the
//! heuristic assumed a single audio stream. D2 ([`OnnxDiarizer`],
//! #111) supersedes it. The class plus 8 tests sat unused until
//! #310 deleted them.

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
/// `utterances`) for impls that want to look at the signal
/// directly. The current production impl
/// (`onnx::OnnxDiarizer`) consumes them; `NoopDiarizer` ignores
/// them. Pass an empty slice when no audio is available; the
/// trait does not require the chunks to be populated, and impls
/// that need them must check `audio_chunks.len() ==
/// utterances.len()` before reading.
///
/// `format` describes the sample-rate / channel layout of every
/// chunk in `audio_chunks` (assumed homogeneous within a single
/// pump call). The ONNX path needs it for STFT / Mel-FB feature
/// extraction.
pub trait Diarize: Send + Sync {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        format: CaptureFormat,
    );

    /// Reset per-session cluster state. Called by the meeting pump at the
    /// start of each new session so speaker IDs from a previous meeting
    /// don't bleed into the next one. The default no-op is correct for
    /// stateless impls (`NoopDiarizer`).
    fn reset(&self) {}
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
/// - `enabled` → `Arc::clone(&app_state.runtime_flags.diarization_enabled)`
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

    /// Forward reset to the inner diarizer regardless of the enabled flag.
    /// When re-enabled after being turned off, the inner diarizer should
    /// start with clean state for the new session.
    fn reset(&self) {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.reset();
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

    #[test]
    fn flag_gated_observes_slot_swap_mid_session() {
        // Audit-2 caught the gap: we tested the *flag* flip is
        // live, but never tested the *slot* swap path that #301's
        // download IPC actually exercises. Pre-PR-G the inner was
        // owned by FlagGatedDiarizer directly; post-#304 it's a
        // shared `DiarizeSlot = Arc<RwLock<Arc<dyn Diarize>>>` so
        // a write through the shared slot must propagate to the
        // FlagGatedDiarizer's read on the next call. This test
        // pins that behaviour.
        let initial = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let replacement = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let fallback = std::sync::Arc::new(RecordingDiarizer {
            called: std::sync::atomic::AtomicBool::new(false),
        });
        let slot: DiarizeSlot = std::sync::Arc::new(std::sync::RwLock::new(
            initial.clone() as std::sync::Arc<dyn Diarize>
        ));
        let enabled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let diarizer = FlagGatedDiarizer::new(
            enabled,
            std::sync::Arc::clone(&slot),
            fallback.clone() as std::sync::Arc<dyn Diarize>,
        );

        // Pre-swap: initial sees the call.
        let mut us = vec![utt(0, 1000, "x")];
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(
            initial.called.load(std::sync::atomic::Ordering::Relaxed),
            "before swap, initial diarizer should be called"
        );
        assert!(
            !replacement
                .called
                .load(std::sync::atomic::Ordering::Relaxed),
            "before swap, replacement should not have been called"
        );

        // Swap: write a new Arc into the slot. This is the move
        // the IPC `download_diarizer_model` makes after a
        // successful download + load.
        {
            let mut guard = slot.write().expect("slot write lock");
            *guard = replacement.clone() as std::sync::Arc<dyn Diarize>;
        }

        // Reset the initial recorder so we can prove it does NOT
        // get called this time.
        initial
            .called
            .store(false, std::sync::atomic::Ordering::Relaxed);

        // Post-swap: replacement sees the call, initial does not.
        diarizer.label_utterances(&mut us, &[], fmt());
        assert!(
            replacement
                .called
                .load(std::sync::atomic::Ordering::Relaxed),
            "after swap, replacement diarizer should be called"
        );
        assert!(
            !initial.called.load(std::sync::atomic::Ordering::Relaxed),
            "after swap, the previous diarizer should NOT be called"
        );
        assert!(
            !fallback.called.load(std::sync::atomic::Ordering::Relaxed),
            "fallback should never be called while the flag is on"
        );
    }
}
