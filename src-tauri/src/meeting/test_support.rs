//! Test-only support for `meeting::manager` and sibling modules.
//!
//! Keeps `manager.rs` focused on runtime state and lifecycle code while
//! preserving the existing test API surface (notably
//! [`SessionManager::new_for_test`]).

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::audio::{
    AudioCapture, AudioDevice, AudioSession, AudioSource, CaptureFormat, CapturedAudio,
};
use crate::db::SqliteDatabase;
use crate::repository::Repository;
use crate::transcription::{streaming::StreamingTranscribeSession, Transcribe, Utterance};

use super::manager::SessionManager;
use super::{
    MeetingAppOverride, MeetingAppOverrideRepository, MeetingSession, MeetingSessionRepository,
    NewMeetingAppOverride, NewMeetingSession, NewPersistedUtterance, PersistedUtterance,
    SqliteMeetingSessionRepository,
};

/// Test-only no-op audio backend used by `SessionManager::new_for_test`.
/// Returns empty capture sessions instantly so the pump's spawn path
/// runs without a real audio device. Lives outside `manager.rs` so the
/// test-only constructor can stay available to IPC-layer tests without
/// keeping runtime code in the god file.
struct NoOpAudio;

impl AudioCapture for NoOpAudio {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![])
    }

    fn start(&self, _: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn stop(&self) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }

    fn is_recording(&self) -> bool {
        false
    }

    fn start_session(&self, source: AudioSource) -> Result<Box<dyn AudioSession>> {
        Ok(Box::new(NoOpSession { source }))
    }
}

struct NoOpSession {
    source: AudioSource,
}

impl AudioSession for NoOpSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }
}

/// Test-only override repo. Returns an empty list so the classifier falls
/// through to the static defaults — same behaviour the pre-#112
/// `SessionManager` exhibited.
struct NoOpAppOverrides;

#[async_trait]
impl MeetingAppOverrideRepository for NoOpAppOverrides {
    async fn list(&self) -> Result<Vec<MeetingAppOverride>> {
        Ok(vec![])
    }

    async fn upsert(&self, _: NewMeetingAppOverride) -> Result<MeetingAppOverride> {
        Err(anyhow!("NoOpAppOverrides::upsert not supported"))
    }

    async fn set_profile(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<MeetingAppOverride> {
        Err(anyhow!("NoOpAppOverrides::set_profile not supported"))
    }

    async fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

impl SessionManager {
    /// Test-only constructor that wires the manager up against a no-op
    /// audio backend and an empty transcribe slot. Use from IPC-layer
    /// tests where the manager is constructed but its pump path is not
    /// exercised — keeps each call site from repeating the stub-audio
    /// plumbing.
    pub fn new_for_test(repo: Arc<dyn MeetingSessionRepository>) -> Self {
        let audio: Arc<dyn AudioCapture> = Arc::new(NoOpAudio);
        let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> = Arc::new(Mutex::new(None));
        let emitter: Arc<dyn crate::events::EventEmitter> =
            Arc::new(crate::events::NoopEventEmitter);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let app_overrides: Arc<dyn MeetingAppOverrideRepository> = Arc::new(NoOpAppOverrides);
        Self::new(
            repo,
            audio,
            transcribe,
            emitter,
            diarize,
            app_overrides,
            Arc::new(AtomicU32::new(0f32.to_bits())),
        )
    }
}

/// Test-only audio backend that produces empty capture sessions instantly.
/// Lets `start_manual` succeed without a real mic and makes the pump's
/// chunk-and-transcribe cycle a no-op (no samples, no transcript, no
/// utterance appended). The pump task is still spawned and runs until
/// cancelled, so tests that exercise `start_manual` must also call
/// `stop_manual` to drain it.
struct StubParallelAudio;

impl AudioCapture for StubParallelAudio {
    fn list_input_devices(&self) -> Result<Vec<AudioDevice>> {
        Ok(vec![])
    }

    fn start(&self, _: Option<&str>) -> Result<()> {
        Ok(())
    }

    fn stop(&self) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }

    fn is_recording(&self) -> bool {
        false
    }

    fn start_session(&self, source: AudioSource) -> Result<Box<dyn AudioSession>> {
        Ok(Box::new(StubSession { source }))
    }
}

struct StubSession {
    source: AudioSource,
}

impl AudioSession for StubSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn drain_into(&self, _sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        Ok(CaptureFormat {
            sample_rate: 16_000,
            channels: 1,
        })
    }

    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }
}

/// No-op streaming session for tests. Every `feed` and `drain` call succeeds
/// and returns nothing — the pump tick-loop runs cleanly with zero utterances.
pub(super) struct NoopStreamingSession;

impl StreamingTranscribeSession for NoopStreamingSession {
    fn feed(&mut self, _captured: &[f32]) -> Result<()> {
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        Ok(vec![])
    }

    fn finish(self: Box<Self>) -> Result<Vec<Utterance>> {
        Ok(vec![])
    }
}

/// No-op `Transcribe` backend that supports streaming via `NoopStreamingSession`.
/// Used by `fresh_manager` / `manager_with_repo` so the lifecycle's
/// fail-fast transcriber check passes and the pump has a working streaming
/// session without a real Whisper model.
pub(super) struct NoopStreamTranscribe;

impl Transcribe for NoopStreamTranscribe {
    fn transcribe(&self, _audio: &CapturedAudio) -> Result<String> {
        Ok(String::new())
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn start_stream(
        &self,
        _format: CaptureFormat,
        _prompt: &str,
    ) -> Result<Box<dyn StreamingTranscribeSession>> {
        Ok(Box::new(NoopStreamingSession))
    }
}

pub(super) async fn fresh_manager() -> SessionManager {
    let db = SqliteDatabase::open_in_memory().await.unwrap();
    let repo: Arc<dyn MeetingSessionRepository> =
        Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
    manager_with_repo(repo)
}

/// Same as [`fresh_manager`] but with `transcribe = None`. Use for tests
/// that specifically exercise the fail-fast "no transcriber loaded" path.
pub(super) async fn fresh_manager_no_transcriber() -> SessionManager {
    let db = SqliteDatabase::open_in_memory().await.unwrap();
    let repo: Arc<dyn MeetingSessionRepository> =
        Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
    let audio: Arc<dyn AudioCapture> = Arc::new(StubParallelAudio);
    let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> = Arc::new(Mutex::new(None));
    let emitter: Arc<dyn crate::events::EventEmitter> = Arc::new(crate::events::NoopEventEmitter);
    let diarize: Arc<dyn crate::diarization::Diarize> = Arc::new(crate::diarization::NoopDiarizer);
    let app_overrides: Arc<dyn MeetingAppOverrideRepository> = Arc::new(NoOpAppOverrides);
    SessionManager::new(
        repo,
        audio,
        transcribe,
        emitter,
        diarize,
        app_overrides,
        Arc::new(AtomicU32::new(0f32.to_bits())),
    )
}

/// Same as [`fresh_manager`] but lets the caller supply a pre-built repo —
/// used by tests that need to insert rows directly before constructing the
/// manager that will exercise them.
pub(super) fn manager_with_repo(repo: Arc<dyn MeetingSessionRepository>) -> SessionManager {
    let audio: Arc<dyn AudioCapture> = Arc::new(StubParallelAudio);
    let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> =
        Arc::new(Mutex::new(Some(Arc::new(NoopStreamTranscribe))));
    let emitter: Arc<dyn crate::events::EventEmitter> = Arc::new(crate::events::NoopEventEmitter);
    let diarize: Arc<dyn crate::diarization::Diarize> = Arc::new(crate::diarization::NoopDiarizer);
    let app_overrides: Arc<dyn MeetingAppOverrideRepository> = Arc::new(NoOpAppOverrides);
    SessionManager::new(
        repo,
        audio,
        transcribe,
        emitter,
        diarize,
        app_overrides,
        Arc::new(AtomicU32::new(0f32.to_bits())),
    )
}

pub(super) fn make_partial(text: &str, started: u64, ended: u64, label: &str) -> Utterance {
    Utterance {
        text: text.to_owned(),
        started_at_ms: started,
        ended_at_ms: ended,
        is_final: false,
        speaker_label: Some(label.to_owned()),
    }
}

pub(super) fn make_final(text: &str, started: u64, ended: u64, label: &str) -> Utterance {
    Utterance {
        text: text.to_owned(),
        started_at_ms: started,
        ended_at_ms: ended,
        is_final: true,
        speaker_label: Some(label.to_owned()),
    }
}

/// Failing `close_session` repo wrapper used by the #492 race tests.
/// Delegates every other call to an inner SQLite repo so `start_manual`
/// and `append_utterance` work normally; only `close_session` is
/// overridden. Optional `on_close_session` callback runs *before* the
/// failure is returned so the test can inject the "concurrent
/// start_manual claimed the slot" race condition deterministically.
pub(super) struct FailingCloseRepo {
    pub(super) inner: Arc<dyn MeetingSessionRepository>,
    pub(super) on_close_session: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[async_trait]
impl Repository<MeetingSession, NewMeetingSession, i64> for FailingCloseRepo {
    async fn list(&self) -> Result<Vec<MeetingSession>> {
        self.inner.list().await
    }

    async fn create(&self, new: NewMeetingSession) -> Result<MeetingSession> {
        self.inner.create(new).await
    }

    async fn update(&self, item: MeetingSession) -> Result<()> {
        self.inner.update(item).await
    }

    async fn delete(&self, id: i64) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[async_trait]
impl MeetingSessionRepository for FailingCloseRepo {
    async fn close_session(&self, _id: i64) -> Result<()> {
        if let Some(cb) = self.on_close_session.as_ref() {
            cb();
        }
        Err(anyhow!("simulated close_session failure"))
    }

    async fn append_utterance(
        &self,
        new: NewPersistedUtterance,
    ) -> Result<Option<PersistedUtterance>> {
        self.inner.append_utterance(new).await
    }

    async fn list_utterances(&self, session_id: i64) -> Result<Vec<PersistedUtterance>> {
        self.inner.list_utterances(session_id).await
    }

    async fn set_notes(&self, id: i64, notes: Option<String>) -> Result<()> {
        self.inner.set_notes(id, notes).await
    }

    async fn set_name(&self, id: i64, name: Option<String>) -> Result<()> {
        self.inner.set_name(id, name).await
    }

    async fn get_by_id(&self, id: i64) -> Result<Option<MeetingSession>> {
        self.inner.get_by_id(id).await
    }

    async fn list_open_sessions(&self) -> Result<Vec<MeetingSession>> {
        self.inner.list_open_sessions().await
    }

    async fn search_sessions(&self, query: &str) -> Result<Vec<MeetingSession>> {
        self.inner.search_sessions(query).await
    }
}

/// Recording diarizer for the merged-dispatch tests. Saves the
/// chronological sequence of `started_at_ms` values it receives + the
/// audio chunk lengths, then writes deterministic `"Speaker A"` labels
/// so tests can assert order without standing up a real diarizer.
pub(super) struct RecordingDiarizer {
    pub(super) seen_starts: Mutex<Vec<u64>>,
    pub(super) seen_audio_lens: Mutex<Vec<usize>>,
}

impl crate::diarization::Diarize for RecordingDiarizer {
    fn label_utterances(
        &self,
        utterances: &mut [crate::transcription::Utterance],
        audio: &[Vec<f32>],
        _format: crate::audio::CaptureFormat,
    ) {
        let mut seen = self.seen_starts.lock().unwrap();
        for u in utterances.iter() {
            seen.push(u.started_at_ms);
        }
        let mut seen_audio = self.seen_audio_lens.lock().unwrap();
        for chunk in audio.iter() {
            seen_audio.push(chunk.len());
        }
        for u in utterances.iter_mut() {
            u.speaker_label = Some("Speaker A".to_owned());
        }
    }
}
