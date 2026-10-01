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
// Online cluster state + session-end re-cluster (#1013). Pure Rust, but
// only the ONNX diarizer drives it, so it is gated with it to keep
// `--no-default-features` builds free of dead code.
#[cfg(feature = "diarization-onnx")]
pub mod session;

/// Diagnostic re-test of the #641 ORT finding against rc.13. Test-only,
/// `#[ignore]`d, and gated on `parakeet` because that is the only build
/// where `ort` is present. Production diarization does not use it.
#[cfg(all(test, feature = "parakeet"))]
mod ort_probe;

/// Which cluster space an utterance is matched in (#1013 item 7).
///
/// Clusters in different namespaces can never be merged or matched —
/// a cannot-link constraint between capture channels. The pump picks
/// the namespace from the capture source; the diarizer only enforces
/// the separation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpeakerNamespace {
    /// The ordinary flat space: the remote stream in a mic + system
    /// session, or every source in an all-local session. Clusters are
    /// labelled `"Speaker N"`.
    #[default]
    Shared,
    /// In-room voices on the local mic of a mic + system session, when
    /// in-room separation is enabled (`HUSH_DIARIZER_LOCAL_SEPARATION`).
    /// The dominant cluster keeps the `"mic"` tag (rendered "You"), so
    /// the #1003 behaviour is unchanged for a single local talker;
    /// any further in-room voice gets an `"In-room N"` label (the local
    /// user counts as in-room 1).
    LocalRoom,
}

/// Per-utterance context the pump knows and the embedding does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkHint {
    pub namespace: SpeakerNamespace,
    /// `false` when the utterance's span overlaps local mic speech
    /// (#1013 item 6). The diarizer may still *assign* it to an
    /// existing speaker, but must not let it move a centroid or found
    /// a new speaker: overlapped spans are where turn-taking is
    /// contested and embeddings are least trustworthy.
    pub may_update: bool,
    /// The source tag the pump stamps when the diarizer abstains
    /// (`"mic"` / `"system"`). Needed so the session-end re-cluster can
    /// address rows that were persisted under the fallback label.
    pub fallback_label: &'static str,
}

impl Default for ChunkHint {
    fn default() -> Self {
        Self {
            namespace: SpeakerNamespace::Shared,
            may_update: true,
            fallback_label: crate::audio::SYSTEM_SPEAKER_TAG,
        }
    }
}

/// One persisted-label change produced by the session-end re-cluster
/// (#1013 item 5). Rows are addressed by `(session, start, end, old
/// label)`: the diarizer never sees row ids, and a source's timestamps
/// are unique within a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relabel {
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub old_label: String,
    pub new_label: String,
}

/// Read a boolean `HUSH_*` diarizer toggle. `1`/`true`/`on` and
/// `0`/`false`/`off` (any case) are honoured; unset or anything else
/// yields `default`.
pub fn env_flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "on" | "yes" => true,
            "0" | "false" | "off" | "no" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

/// `HUSH_DIARIZER_RECLUSTER` — session-end re-cluster + relabel (#1013
/// item 5). Default **on**.
pub fn recluster_enabled() -> bool {
    env_flag("HUSH_DIARIZER_RECLUSTER", true)
}

/// `HUSH_DIARIZER_OVERLAP_GUARD` — mic-activity overlap guard (#1013
/// item 6). Default **on** (it self-disables per session when the mic
/// is clearly hearing the remote audio; see `session.rs`).
pub fn overlap_guard_enabled() -> bool {
    env_flag("HUSH_DIARIZER_OVERLAP_GUARD", true)
}

/// `HUSH_DIARIZER_LOCAL_SEPARATION` — diarize the local mic in its own
/// namespace in mic + system sessions (#1013 item 7 / #1006 item 1).
/// Default **off**: a single local talker can still split into two
/// in-room clusters, which would regress the #1003 "mic = You" labels
/// for the common 1:1 call.
pub fn local_separation_enabled() -> bool {
    env_flag("HUSH_DIARIZER_LOCAL_SEPARATION", false)
}

/// RMS (linear, full scale 1.0) above which a 20 ms mic frame counts
/// as active. About -40 dBFS: clears a quiet room's noise floor, well
/// under conversational speech at a laptop or headset mic.
pub const MIC_ACTIVE_RMS: f32 = 0.01;

/// Fraction of a span's 20 ms frames that must be active before the
/// span counts as overlapping local speech. Keeps a lone "mm-hm" or a
/// keyboard click from tripping the guard on a long remote turn.
pub const MIC_OVERLAP_FRACTION: f32 = 0.3;

/// Whether `mic` (16 kHz mono, the local mic over the same span as a
/// remote utterance) carries enough speech-level energy to count as
/// overlapping local speech (#1013 item 6). Energy, not VAD: the guard
/// only needs "was the local user plausibly talking", and the frame
/// fraction absorbs transients. Thresholds are reasoned defaults, not
/// tuned on real calls.
pub fn mic_overlaps_speech(mic: &[f32]) -> bool {
    const FRAME: usize = 320; // 20 ms @ 16 kHz
    let frames = mic.len() / FRAME;
    if frames == 0 {
        return false;
    }
    let active = mic
        .chunks_exact(FRAME)
        .filter(|f| {
            let energy = f.iter().map(|x| x * x).sum::<f32>() / FRAME as f32;
            energy.sqrt() > MIC_ACTIVE_RMS
        })
        .count();
    active as f32 >= MIC_OVERLAP_FRACTION * frames as f32
}

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

    /// Snapshot the per-session cluster centroids for cross-session
    /// speaker identity resolution (#667). Returns
    /// `(cluster_id, centroid, utterance_count)` for every cluster
    /// that was assigned at least one utterance in this session.
    ///
    /// **Call this BEFORE `reset()`** — `reset()` clears the cluster
    /// state. The caller (lifecycle::stop_manual) is responsible for
    /// the ordering.
    ///
    /// Default returns an empty `Vec` — correct for stateless impls
    /// (`NoopDiarizer`) and for callers that haven't opted into the
    /// feature.
    fn session_centroids(&self) -> Vec<(usize, Vec<f32>, usize)> {
        Vec::new()
    }

    /// Update the active cosine-distance threshold used by the diarizer.
    /// Default no-op for stateless / threshold-less impls.
    fn set_distance_threshold(&self, _threshold: f32) {}

    /// [`Self::label_utterances`] with per-utterance [`ChunkHint`]s
    /// (`hints` parallel to `utterances`). The default ignores the
    /// hints, which is correct for impls without cluster state.
    fn label_utterances_with_hints(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        hints: &[ChunkHint],
        format: CaptureFormat,
    ) {
        let _ = hints;
        self.label_utterances(utterances, audio_chunks, format);
    }

    /// Session-end re-cluster (#1013 item 5): re-run a global
    /// clustering over every embedding retained this session, update
    /// the cluster state in place (so a following
    /// [`Self::session_centroids`] sees the re-clustered speakers), and
    /// return the label changes the caller must persist.
    ///
    /// Call **after** the last `label_utterances*` of the session and
    /// **before** `session_centroids()` / `reset()`. Default: no
    /// changes.
    fn finalize_session(&self) -> Vec<Relabel> {
        Vec::new()
    }
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

    /// Forward to the inner diarizer regardless of the enabled flag.
    /// If the user toggled diarization off mid-session, no embeddings
    /// were computed, so the inner will return an empty Vec — correct.
    /// If it was on, we snapshot the centroids before reset clears them.
    fn session_centroids(&self) -> Vec<(usize, Vec<f32>, usize)> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.session_centroids()
    }

    fn set_distance_threshold(&self, threshold: f32) {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.set_distance_threshold(threshold);
    }

    fn label_utterances_with_hints(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        hints: &[ChunkHint],
        format: CaptureFormat,
    ) {
        if self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            inner.label_utterances_with_hints(utterances, audio_chunks, hints, format);
        } else {
            self.fallback
                .label_utterances_with_hints(utterances, audio_chunks, hints, format);
        }
    }

    /// Forwarded regardless of the enabled flag, like
    /// `session_centroids`: if diarization was toggled off mid-session
    /// the inner simply has fewer (or no) retained embeddings.
    fn finalize_session(&self) -> Vec<Relabel> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner.finalize_session()
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
            words: None,
        }
    }

    #[test]
    fn mic_overlap_detects_sustained_speech_only() {
        let silence = vec![0.001_f32; 16_000];
        assert!(!mic_overlaps_speech(&silence));
        assert!(!mic_overlaps_speech(&[]));
        // 1 s of loud signal in a 2 s span = 50 % active.
        let mut half = vec![0.0_f32; 32_000];
        for (i, v) in half.iter_mut().take(16_000).enumerate() {
            *v = 0.1 * (i as f32 * 0.05).sin();
        }
        assert!(mic_overlaps_speech(&half));
        // A 100 ms blip in 3 s stays below the 30 % floor.
        let mut blip = vec![0.0_f32; 48_000];
        for v in blip.iter_mut().take(1_600) {
            *v = 0.5;
        }
        assert!(!mic_overlaps_speech(&blip));
    }

    #[test]
    fn env_flag_parses_common_spellings() {
        // Unique names so parallel tests can't race on them.
        std::env::set_var("HUSH_TEST_FLAG_A", "Off");
        std::env::set_var("HUSH_TEST_FLAG_B", "1");
        std::env::set_var("HUSH_TEST_FLAG_C", "maybe");
        assert!(!env_flag("HUSH_TEST_FLAG_A", true));
        assert!(env_flag("HUSH_TEST_FLAG_B", false));
        assert!(env_flag("HUSH_TEST_FLAG_C", true));
        assert!(!env_flag("HUSH_TEST_FLAG_UNSET_XYZ", false));
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
