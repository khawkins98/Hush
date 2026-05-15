//! `AppStateBuilder` — the explicit-builder pattern for [`super::state::AppState`].
//!
//! Extracted from `ipc/mod.rs` under #597 (item 6). No behaviour change.
//!
//! Replaces a 7-positional-arg constructor whose call sites read like an
//! unlabelled tuple. Each `.field(value)` call is self-documenting at the
//! call site, and adding a future required field becomes a one-method
//! addition rather than a breaking-arg-list change at every caller.
//!
//! The production constructor (`AppState::build_default`) lives in
//! [`super::state`] and delegates to [`AppStateBuilder::build`] after
//! filling in defaults. Tests construct an `AppState` by composing the
//! builder explicitly with `Mem*` repositories.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::audio::AudioCapture;
use crate::dictionary::{ReplacementRepository, VocabularyRepository};
use crate::history::HistoryRepository;
use crate::settings::SettingsRepository;
use crate::transcription::Transcribe;

use super::pipeline::{redirect_decision, RedirectDecision};
use super::state::{
    encode_autostart_mode, AppState, DataServices, InferenceState, ModelStore, PttState,
    RuntimeFlags, TranscribeSlot, UpdateCheckCache,
};

/// Builder for [`AppState`].
///
/// Replaces a 7-positional-arg constructor whose call sites read like an
/// unlabelled tuple. Each `.field(value)` call is self-documenting at the
/// call site, and adding a future required field (e.g. a download-state
/// service or a system-audio source) becomes a one-method addition rather
/// than a breaking-arg-list change at every caller.
///
/// `transcribe` is `Option<Arc<dyn Transcribe>>` rather than required
/// because the production backend is gated behind the `whisper` feature
/// AND a loaded model — both legitimately absent on a fresh install. The
/// rest are required; [`AppStateBuilder::build`] returns an error naming
/// the missing field if any of them are unset, which is more useful than
/// a panic when (e.g.) a future test refactor accidentally forgets one.
#[derive(Default)]
pub struct AppStateBuilder {
    audio: Option<Arc<dyn AudioCapture>>,
    transcribe: Option<Arc<dyn Transcribe>>,
    /// Pre-built `Arc<Mutex<...>>` for the transcribe slot. Set via
    /// [`AppStateBuilder::transcribe_arc`] when the caller (the
    /// production wiring in `build_default`) needs to share the same
    /// Arc with the meeting pump. When unset, `build` wraps
    /// [`Self::transcribe`] in a fresh Arc — the hot-swap surface
    /// stays inside `AppState` only.
    transcribe_arc: Option<TranscribeSlot>,
    /// Pre-built Arc for the meeting-pump slot. See [`AppState::transcribe_meeting`]
    /// (#248). When unset, `build` creates a fresh empty slot — fine
    /// for tests that don't drive the meeting pump.
    transcribe_meeting_arc: Option<TranscribeSlot>,
    diarize: Option<Arc<dyn crate::diarization::Diarize>>,
    history: Option<Arc<dyn HistoryRepository>>,
    replacements: Option<Arc<dyn ReplacementRepository>>,
    vocabulary: Option<Arc<dyn VocabularyRepository>>,
    settings: Option<Arc<dyn SettingsRepository>>,
    meetings: Option<Arc<dyn crate::meeting::MeetingSessionRepository>>,
    meeting_app_overrides: Option<Arc<dyn crate::meeting::MeetingAppOverrideRepository>>,
    speakers: Option<Arc<dyn crate::speakers::SpeakerStore>>,
    meeting_manager: Option<Arc<crate::meeting::SessionManager>>,
    models_dir: Option<PathBuf>,
    ptt_combo: Option<crate::hotkey::ptt::PttCombo>,
    ptt_active: Option<bool>,
    hud_enabled: Option<bool>,
    sound_cues_enabled: Option<bool>,
    sound_cue_start_enabled: Option<bool>,
    sound_cue_complete_enabled: Option<bool>,
    meeting_autostart_mode: Option<crate::meeting::MeetingAutostartMode>,
    diarization_enabled: Option<bool>,
    speaker_identity_enabled: Option<bool>,
    /// Pre-built `Arc<AtomicBool>` for the diarization-enabled
    /// flag. Set via [`AppStateBuilder::diarization_enabled_arc`]
    /// when the production wiring (`build_default`) needs to
    /// share the same Arc with the meeting pump's
    /// [`crate::diarization::FlagGatedDiarizer`]. When unset,
    /// `build` constructs a fresh Arc seeded from
    /// [`Self::diarization_enabled`].
    diarization_enabled_arc: Option<Arc<std::sync::atomic::AtomicBool>>,
    speaker_identity_enabled_arc: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Pre-built [`crate::diarization::DiarizeSlot`] for hot-swap
    /// support (#301). Set via
    /// [`AppStateBuilder::diarize_slot`] when the production
    /// wiring needs to share the same slot with the
    /// `FlagGatedDiarizer` so the post-download swap propagates.
    /// When unset, `build` constructs a fresh slot seeded with a
    /// `NoopDiarizer` — fine for tests that don't exercise the
    /// download / swap path.
    diarize_slot: Option<crate::diarization::DiarizeSlot>,
    /// Pre-built shared thread-count atomic (#255). Set via
    /// [`AppStateBuilder::inference_threads_arc`] when
    /// `build_default` wants to share the loaded
    /// `WhisperTranscription`'s atomic with the IPC writer. When
    /// unset, `build` constructs a fresh Arc seeded from the
    /// default thread count — fine for tests.
    inference_threads_arc: Option<Arc<std::sync::atomic::AtomicI32>>,
    /// Pre-built shared mic-gain atomic (#531). Set via
    /// [`AppStateBuilder::mic_gain_db_arc`] when `build_default`
    /// wants to share the loaded `WhisperTranscription`'s atomic
    /// with the IPC writer and the meeting pump. When unset,
    /// `build` constructs a fresh Arc at 0.0 dB — fine for tests.
    mic_gain_db_arc: Option<Arc<std::sync::atomic::AtomicU32>>,
    /// Debug log state (#532). Set via
    /// [`AppStateBuilder::debug_log`] when `run()` wires the
    /// tracing layer. When unset, `build` constructs a new
    /// (empty) state — fine for tests.
    debug_log: Option<crate::debug_log::DebugLogState>,
    /// Per-phase startup timings (#584 Angle 1). Populated by
    /// `AppState::build_default` as it walks the boot path; tests
    /// leave it unset (the IPC reads back an empty `Vec`).
    startup_timings: Option<Vec<crate::ipc::commands::system::StartupPhase>>,
}

impl AppStateBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn audio(mut self, audio: Arc<dyn AudioCapture>) -> Self {
        self.audio = Some(audio);
        self
    }

    /// Optional. `None` means "no transcriber loaded yet"; the IPC layer
    /// surfaces [`commands::IpcError::TranscriptionUnavailable`] for
    /// dictation calls while in this state.
    pub fn transcribe(mut self, transcribe: Option<Arc<dyn Transcribe>>) -> Self {
        self.transcribe = transcribe;
        self
    }

    /// Hand the builder the pre-built `Arc<Mutex<...>>` so the meeting
    /// pump can hold the same Arc and observe model hot-swaps. When
    /// supplied, the builder uses this directly instead of wrapping
    /// `transcribe()` in a fresh Arc.
    pub fn transcribe_arc(mut self, transcribe: TranscribeSlot) -> Self {
        self.transcribe_arc = Some(transcribe);
        self
    }

    /// Hand the builder the pre-built meeting-pump slot. Production
    /// (#248) loads a second `WhisperTranscription` instance and
    /// wires it here so `SessionManager` reads from a slot
    /// independent of the dictation one. Tests can leave this
    /// unset; `build` then constructs an empty slot.
    pub fn transcribe_meeting_arc(mut self, transcribe: TranscribeSlot) -> Self {
        self.transcribe_meeting_arc = Some(transcribe);
        self
    }

    /// Optional. Defaults to [`crate::diarization::NoopDiarizer`] —
    /// the meeting pump's existing source-derived `"mic"` /
    /// `"system"` labels survive. Override to wire D1 / D2.
    pub fn diarize(mut self, diarize: Arc<dyn crate::diarization::Diarize>) -> Self {
        self.diarize = Some(diarize);
        self
    }

    pub fn history(mut self, history: Arc<dyn HistoryRepository>) -> Self {
        self.history = Some(history);
        self
    }

    pub fn replacements(mut self, replacements: Arc<dyn ReplacementRepository>) -> Self {
        self.replacements = Some(replacements);
        self
    }

    pub fn vocabulary(mut self, vocabulary: Arc<dyn VocabularyRepository>) -> Self {
        self.vocabulary = Some(vocabulary);
        self
    }

    pub fn settings(mut self, settings: Arc<dyn SettingsRepository>) -> Self {
        self.settings = Some(settings);
        self
    }

    pub fn meeting_manager(mut self, mgr: Arc<crate::meeting::SessionManager>) -> Self {
        self.meeting_manager = Some(mgr);
        self
    }

    pub fn meetings(mut self, meetings: Arc<dyn crate::meeting::MeetingSessionRepository>) -> Self {
        self.meetings = Some(meetings);
        self
    }

    pub fn meeting_app_overrides(
        mut self,
        overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository>,
    ) -> Self {
        self.meeting_app_overrides = Some(overrides);
        self
    }

    pub fn speakers(mut self, speakers: Arc<dyn crate::speakers::SpeakerStore>) -> Self {
        self.speakers = Some(speakers);
        self
    }

    pub fn models_dir(mut self, models_dir: PathBuf) -> Self {
        self.models_dir = Some(models_dir);
        self
    }

    pub fn ptt_combo(mut self, combo: crate::hotkey::ptt::PttCombo) -> Self {
        self.ptt_combo = Some(combo);
        self
    }

    pub fn ptt_active(mut self, active: bool) -> Self {
        self.ptt_active = Some(active);
        self
    }

    pub fn hud_enabled(mut self, enabled: bool) -> Self {
        self.hud_enabled = Some(enabled);
        self
    }

    pub fn sound_cues_enabled(mut self, enabled: bool) -> Self {
        self.sound_cues_enabled = Some(enabled);
        self
    }

    pub fn sound_cue_start_enabled(mut self, enabled: bool) -> Self {
        self.sound_cue_start_enabled = Some(enabled);
        self
    }

    pub fn sound_cue_complete_enabled(mut self, enabled: bool) -> Self {
        self.sound_cue_complete_enabled = Some(enabled);
        self
    }

    pub fn meeting_autostart_mode(mut self, mode: crate::meeting::MeetingAutostartMode) -> Self {
        self.meeting_autostart_mode = Some(mode);
        self
    }

    pub fn diarization_enabled(mut self, enabled: bool) -> Self {
        self.diarization_enabled = Some(enabled);
        self
    }

    pub fn speaker_identity_enabled(mut self, enabled: bool) -> Self {
        self.speaker_identity_enabled = Some(enabled);
        self
    }

    /// Set the pre-built `Arc<AtomicBool>` that the FlagGatedDiarizer
    /// already holds. The AppState's `diarization_enabled` field
    /// becomes that same Arc, so the IPC `set_diarization_enabled`
    /// path flips both views with one atomic store.
    pub fn diarization_enabled_arc(mut self, arc: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.diarization_enabled_arc = Some(arc);
        self
    }

    pub fn speaker_identity_enabled_arc(
        mut self,
        arc: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.speaker_identity_enabled_arc = Some(arc);
        self
    }

    /// Set the pre-built shared thread-count atomic (#255).
    /// `build_default` cloned this out of the just-loaded
    /// `WhisperTranscription::shared_inference_threads()`, so the
    /// AppState field, the IPC writer, and the transcriber all
    /// read/write through the same atomic.
    pub fn inference_threads_arc(mut self, arc: Arc<std::sync::atomic::AtomicI32>) -> Self {
        self.inference_threads_arc = Some(arc);
        self
    }

    /// Set the pre-built shared mic-gain atomic (#531).
    /// `build_default` clones this out of the loaded
    /// `WhisperTranscription::shared_mic_gain_db()` so the
    /// AppState field, the IPC writer, the dictation transcriber,
    /// and the meeting pump all read/write through the same atomic.
    pub fn mic_gain_db_arc(mut self, arc: Arc<std::sync::atomic::AtomicU32>) -> Self {
        self.mic_gain_db_arc = Some(arc);
        self
    }

    /// Set the [`crate::debug_log::DebugLogState`] created in `run()`
    /// so `AppState` owns a handle to the ring buffer. Tests can omit
    /// this; `build` will construct a fresh (empty) state.
    pub fn debug_log(mut self, state: crate::debug_log::DebugLogState) -> Self {
        self.debug_log = Some(state);
        self
    }

    /// Hand the builder the per-phase startup-timing trace captured
    /// in `AppState::build_default` (#584 Angle 1). Tests leave this
    /// unset; the resulting `AppState.startup_timings` is an empty
    /// `Vec` and the IPC simply returns no rows.
    pub fn startup_timings(
        mut self,
        timings: Vec<crate::ipc::commands::system::StartupPhase>,
    ) -> Self {
        self.startup_timings = Some(timings);
        self
    }

    /// Set the pre-built [`crate::diarization::DiarizeSlot`] (#301).
    /// The `FlagGatedDiarizer` holds an `Arc::clone` of the same
    /// slot, so the IPC `download_diarizer_model` path can
    /// hot-swap the inner diarizer post-download.
    pub fn diarize_slot(mut self, slot: crate::diarization::DiarizeSlot) -> Self {
        self.diarize_slot = Some(slot);
        self
    }

    /// Construct the [`AppState`], or return a descriptive error naming
    /// the first required field that wasn't set.
    pub fn build(self) -> Result<AppState> {
        Ok(AppState {
            audio: self
                .audio
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: audio not set"))?,
            inference: InferenceState {
                transcribe: self
                    .transcribe_arc
                    .unwrap_or_else(|| Arc::new(Mutex::new(self.transcribe))),
                transcribe_meeting: self
                    .transcribe_meeting_arc
                    .unwrap_or_else(|| Arc::new(Mutex::new(None))),
                diarize: self
                    .diarize
                    .unwrap_or_else(|| Arc::new(crate::diarization::NoopDiarizer)),
                transcriber_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                diarize_slot: self.diarize_slot.unwrap_or_else(|| {
                    Arc::new(std::sync::RwLock::new(
                        Arc::new(crate::diarization::NoopDiarizer)
                            as Arc<dyn crate::diarization::Diarize>,
                    ))
                }),
            },
            data: DataServices {
                history: self
                    .history
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: history not set"))?,
                replacements: self
                    .replacements
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: replacements not set"))?,
                vocabulary: self
                    .vocabulary
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: vocabulary not set"))?,
                meetings: self
                    .meetings
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meetings not set"))?,
                meeting_app_overrides: self.meeting_app_overrides.ok_or_else(|| {
                    anyhow::anyhow!("AppStateBuilder: meeting_app_overrides not set")
                })?,
                speakers: self
                    .speakers
                    .unwrap_or_else(|| Arc::new(crate::speakers::MemSpeakerStore)),
            },
            settings: self
                .settings
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: settings not set"))?,
            meeting_manager: self
                .meeting_manager
                .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: meeting_manager not set"))?,
            models: ModelStore {
                models_dir: self
                    .models_dir
                    .ok_or_else(|| anyhow::anyhow!("AppStateBuilder: models_dir not set"))?,
                downloads: Arc::new(Mutex::new(HashMap::new())),
            },
            http: reqwest::Client::builder()
                // Whisper-large-v3 is ~3 GB; ten-minute timeout is on
                // the optimistic side of "any reasonable home
                // connection". Real fix is resumable downloads, but
                // that's out of scope for this PR.
                .timeout(std::time::Duration::from_secs(600))
                .user_agent(concat!("hush/", env!("CARGO_PKG_VERSION")))
                // Redirect policy is host-restricted, not just hop-
                // capped. The default `Policy::default()` follows up
                // to 10 redirects to *any* host — a BGP/DNS hijack of
                // huggingface.co could redirect to an arbitrary server
                // and we'd transfer bytes there before the SHA-256
                // verification rejects them. SHA still catches a
                // swapped file, but the bandwidth + latency leak to
                // the attacker's host is avoidable.
                //
                // We allow up to four hops (HF's `/resolve/main/`
                // typically goes huggingface.co → cdn-lfs.huggingface.co
                // → a signed URL on the same CDN; four leaves headroom
                // for a future re-architecture).
                //
                // Browser-like trust model (#258): a hop is allowed
                // if EITHER its destination is on an HF host OR the
                // immediately-previous URL was on an HF host. The
                // second clause covers HF → S3-signed-URL chains
                // that surface when HF routes large-file serving
                // through a third-party CDN. Without it we'd reject
                // the perfectly-legitimate "HF told us to fetch the
                // file from this signed AWS URL" hop and the
                // download dies with no clear user-facing reason.
                //
                // Only HTTPS is ever followed — an http:// hop from
                // anywhere is rejected, including from an HF host.
                // Defends against a downgrade attack via a
                // (hypothetical) compromised HF redirect.
                .redirect(reqwest::redirect::Policy::custom(
                    |attempt| match redirect_decision(attempt.previous(), attempt.url()) {
                        RedirectDecision::Follow => attempt.follow(),
                        RedirectDecision::Stop(reason) => attempt.error(reason),
                    },
                ))
                .build()
                .map_err(|e| {
                    anyhow::anyhow!("AppStateBuilder: reqwest client build failed: {e}")
                })?,
            pending_foreground: Mutex::new(None),
            update_check: UpdateCheckCache {
                last: Mutex::new(None),
                inflight: Arc::new(tokio::sync::Mutex::new(())),
            },
            ptt: PttState {
                combo: Arc::new(std::sync::RwLock::new(self.ptt_combo.unwrap_or_else(
                    || crate::hotkey::ptt::PttCombo::single(crate::hotkey::ptt::DEFAULT_PTT_KEY),
                ))),
                active: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.ptt_active.unwrap_or(false),
                )),
                listener_spawned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            debug_log: self.debug_log.unwrap_or_default(),
            runtime_flags: RuntimeFlags {
                hud_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.hud_enabled.unwrap_or(true),
                )),
                sound_cues_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cues_enabled.unwrap_or(false),
                )),
                sound_cue_start_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cue_start_enabled.unwrap_or(true),
                )),
                sound_cue_complete_enabled: Arc::new(std::sync::atomic::AtomicBool::new(
                    self.sound_cue_complete_enabled.unwrap_or(true),
                )),
                meeting_autostart_mode: Arc::new(std::sync::atomic::AtomicU8::new(
                    encode_autostart_mode(
                        self.meeting_autostart_mode
                            .unwrap_or(crate::meeting::MeetingAutostartMode::Off),
                    ),
                )),
                diarization_enabled: self.diarization_enabled_arc.unwrap_or_else(|| {
                    Arc::new(std::sync::atomic::AtomicBool::new(
                        self.diarization_enabled.unwrap_or(false),
                    ))
                }),
                speaker_identity_enabled: self.speaker_identity_enabled_arc.unwrap_or_else(|| {
                    Arc::new(std::sync::atomic::AtomicBool::new(
                        self.speaker_identity_enabled.unwrap_or(false),
                    ))
                }),
                inference_threads: self
                    .inference_threads_arc
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicI32::new(4))),
                mic_gain_db: self
                    .mic_gain_db_arc
                    .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicU32::new(0f32.to_bits()))),
                autostart_path_stale: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            startup_timings: self.startup_timings.unwrap_or_default(),
            hotkey_toggle_error: Mutex::new(None),
        })
    }
}

// `AppHandleMeetingEventEmitter` was the production glue between the
// meeting module's `MeetingEventEmitter` trait and `tauri::AppHandle::emit`.
// Both went away in #431: the meeting module now consumes
// `crate::events::EventEmitter` directly, and the production wrapper is
// `crate::ipc::events::TauriEventEmitter` (constructed below in the
// `SessionManager::new` call site).
