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
            Arc::new(crate::speakers::MemSpeakerStore),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        _vad_session: Box<dyn crate::vad::VadSession>,
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
        Arc::new(crate::speakers::MemSpeakerStore),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        Arc::new(crate::speakers::MemSpeakerStore),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

/// Streaming session whose `finish()` blocks until a shared flag is set,
/// used to keep a meeting's background finalization in flight long enough
/// for a concurrency assertion. `feed`/`drain` are no-ops. `finish` flips
/// `started` (so the test can observe it began) then spins on `release`,
/// sleeping briefly between polls — this runs inside `spawn_blocking`, so
/// blocking the thread is safe and intended.
struct SlowFinishStreamingSession {
    release: Arc<std::sync::atomic::AtomicBool>,
    started: Arc<std::sync::atomic::AtomicBool>,
}

impl StreamingTranscribeSession for SlowFinishStreamingSession {
    fn feed(&mut self, _captured: &[f32]) -> Result<()> {
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        Ok(vec![])
    }

    fn finish(self: Box<Self>) -> Result<Vec<Utterance>> {
        self.started
            .store(true, std::sync::atomic::Ordering::Release);
        // Bounded spin so a test bug can't hang CI forever (~5 s cap).
        for _ in 0..1_000 {
            if self.release.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(vec![])
    }
}

struct SlowFinishTranscribe {
    release: Arc<std::sync::atomic::AtomicBool>,
    started: Arc<std::sync::atomic::AtomicBool>,
}

impl Transcribe for SlowFinishTranscribe {
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
        _vad_session: Box<dyn crate::vad::VadSession>,
    ) -> Result<Box<dyn StreamingTranscribeSession>> {
        Ok(Box::new(SlowFinishStreamingSession {
            release: Arc::clone(&self.release),
            started: Arc::clone(&self.started),
        }))
    }
}

/// A manager whose streaming `finish()` blocks on `release` — keeps the
/// background finalization in flight so the "new meeting awaits
/// finalization" gate can be observed deterministically. `started` flips
/// when `finish()` begins (unused by the current test but handy for
/// finer-grained ordering assertions).
pub(super) async fn manager_with_slow_finish(
    release: Arc<std::sync::atomic::AtomicBool>,
    started: Arc<std::sync::atomic::AtomicBool>,
) -> SessionManager {
    let db = SqliteDatabase::open_in_memory().await.unwrap();
    let repo: Arc<dyn MeetingSessionRepository> =
        Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
    let emitter: Arc<dyn crate::events::EventEmitter> = Arc::new(crate::events::NoopEventEmitter);
    manager_with_slow_finish_parts(release, started, repo, emitter)
}

/// Like [`manager_with_slow_finish`] but lets the caller supply the repo
/// and event emitter so they can read back the closed session row and
/// assert on the emitted `MeetingSessionEnded`. Used by the IPC-layer
/// dictation-during-finalize test (#947 review Gap 2), which is why it's
/// re-exported `pub(crate)` from the meeting module under `cfg(test)`.
pub(crate) fn manager_with_slow_finish_parts(
    release: Arc<std::sync::atomic::AtomicBool>,
    started: Arc<std::sync::atomic::AtomicBool>,
    repo: Arc<dyn MeetingSessionRepository>,
    emitter: Arc<dyn crate::events::EventEmitter>,
) -> SessionManager {
    let audio: Arc<dyn AudioCapture> = Arc::new(StubParallelAudio);
    let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> =
        Arc::new(Mutex::new(Some(Arc::new(SlowFinishTranscribe {
            release,
            started,
        }))));
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
        Arc::new(crate::speakers::MemSpeakerStore),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

/// Audio session that produces NO samples on the tick-drain path but
/// returns a non-empty `CapturedAudio` from `stop()`. This is the only
/// way to exercise the tail-feed branch in
/// [`super::pump::release_audio_handles`]: the stock `StubSession`'s
/// `stop()` returns `samples: vec![]`, so `if samples.is_empty() {
/// continue; }` short-circuits and the tail-feed line never runs.
///
/// `drain_into` returns the format but appends nothing, so no per-tick
/// inference fires — any samples the recording streaming session sees
/// in `feed()` came from the tail-release path, not a tick drain.
pub(super) struct TailReleaseSession {
    source: AudioSource,
    /// Samples `stop()` hands back as the captured tail.
    tail_samples: Vec<f32>,
    format: CaptureFormat,
}

impl AudioSession for TailReleaseSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }

    fn drain_into(&self, _sink: &mut Vec<f32>) -> Result<CaptureFormat> {
        // No tick-drain samples — only the tail comes through stop().
        Ok(self.format)
    }

    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: self.tail_samples,
            format: self.format,
        })
    }
}

/// Streaming session that records every sample slice handed to `feed()`
/// (in arrival order) and, if it ever saw a tail feed, emits one final
/// utterance from `finish()`. Lets a pump test assert both the
/// load-bearing fact (tail samples reached `feed()` before `finish()`)
/// and the downstream effect (a final utterance is produced + persisted).
pub(super) struct RecordingFeedSession {
    /// Each `feed()` call appends the slice length it received. The
    /// tail-feed path is the only feed source in the `TailReleaseSession`
    /// setup, so a non-empty log proves the tail reached the session.
    fed_lens: Arc<Mutex<Vec<usize>>>,
}

impl StreamingTranscribeSession for RecordingFeedSession {
    fn feed(&mut self, captured: &[f32]) -> Result<()> {
        if !captured.is_empty() {
            self.fed_lens.lock().unwrap().push(captured.len());
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<Vec<Utterance>> {
        Ok(vec![])
    }

    fn finish(self: Box<Self>) -> Result<Vec<Utterance>> {
        // Only emit a tail final if something was actually fed — this
        // makes the downstream "tail utterance persisted" assertion fail
        // if the tail-feed line is removed (nothing fed → no final).
        if self.fed_lens.lock().unwrap().is_empty() {
            Ok(vec![])
        } else {
            Ok(vec![make_final("tail words", 0, 100, "mic")])
        }
    }
}

/// Build a [`super::pump::PumpContext`] wired for the tail-feed test.
/// The single mic source has a [`TailReleaseSession`] handle (empty tick
/// drains, non-empty `stop()`) and a [`RecordingFeedSession`] streaming
/// session. `cancel` is pre-set so [`super::pump::run_pump`] skips the
/// loop body and goes straight to the final-drain → `release_audio_handles`
/// → `flush_sessions` path. Returns the context, the in-memory repo (to
/// read back the persisted tail utterance), and the shared `fed_lens`
/// log (to assert the tail reached `feed()`).
pub(super) async fn build_tail_pump_context(
    tail_samples: Vec<f32>,
) -> (
    super::pump::PumpContext,
    Arc<dyn MeetingSessionRepository>,
    Arc<Mutex<Vec<usize>>>,
) {
    use std::sync::atomic::AtomicBool;

    let db = SqliteDatabase::open_in_memory().await.unwrap();
    let repo: Arc<dyn MeetingSessionRepository> =
        Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
    // A real open session row so dispatch's append_utterance has a target.
    let session = repo
        .create(NewMeetingSession {
            app_name: "Zoom".to_owned(),
            app_kind: super::MeetingAppKind::Meeting,
            sources: vec!["mic".to_owned()],
            app_title: None,
        })
        .await
        .unwrap();

    let source = AudioSource::default_microphone();
    let format = CaptureFormat {
        sample_rate: 16_000,
        channels: 1,
    };
    let handle: Box<dyn AudioSession> = Box::new(TailReleaseSession {
        source: source.clone(),
        tail_samples,
        format,
    });

    let fed_lens = Arc::new(Mutex::new(Vec::new()));
    let streaming: Box<dyn StreamingTranscribeSession> = Box::new(RecordingFeedSession {
        fed_lens: Arc::clone(&fed_lens),
    });

    let cancel = Arc::new(AtomicBool::new(true)); // pre-cancelled: skip the loop body
    let ctx = super::pump::PumpContext {
        session_id: session.id,
        repo: Arc::clone(&repo),
        sources: vec![source],
        handles: vec![Some(handle)],
        streaming_sessions: vec![Some(streaming)],
        partials: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        cancel,
        event_emitter: Arc::new(crate::events::NoopEventEmitter),
        diarize: Arc::new(crate::diarization::NoopDiarizer),
        mic_gain_db: Arc::new(AtomicU32::new(0f32.to_bits())),
        audio: Arc::new(StubParallelAudio),
        transcribe: Some(Arc::new(NoopStreamTranscribe)),
        session_start: std::time::Instant::now(),
        vocab_prompt: String::new(),
        replacement_rules: Arc::new(Vec::new()),
        audio_released_tx: None,
        speaker_store: Arc::new(crate::speakers::MemSpeakerStore),
        speaker_identity_enabled: Arc::new(AtomicBool::new(false)),
    };

    (ctx, repo, fed_lens)
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

/// Diarizer that reports a fixed, non-empty set of session centroids — a
/// stand-in for a real `OnnxDiarizer` that accumulated cluster state over a
/// meeting. Used by the #667 background-finalization regression
/// ([`super::pump::tests::background_finalization_resolves_against_real_centroids`]).
/// `label_utterances` is a no-op (the regression exercises the centroid
/// read at finalization, not per-tick labelling).
pub(super) struct CentroidDiarizer {
    /// `(cluster_id, centroid, utterance_count)`. The count is set ≥
    /// [`crate::speakers::MIN_UTTERANCE_COUNT_FOR_MATCH`] so the resolver
    /// does not skip the cluster as a cold-start.
    centroids: Vec<(usize, Vec<f32>, usize)>,
}

impl CentroidDiarizer {
    pub(super) fn with_one_cluster() -> Self {
        Self {
            centroids: vec![(0, vec![0.1f32; 256], 8)],
        }
    }
}

impl crate::diarization::Diarize for CentroidDiarizer {
    fn label_utterances(
        &self,
        _utterances: &mut [crate::transcription::Utterance],
        _audio: &[Vec<f32>],
        _format: crate::audio::CaptureFormat,
    ) {
        // no-op: this diarizer only models the centroid snapshot.
    }

    fn session_centroids(&self) -> Vec<(usize, Vec<f32>, usize)> {
        self.centroids.clone()
    }
}

/// Speaker store that records the cluster IDs handed to identity
/// resolution. A non-empty `resolved_clusters` after finalization proves
/// `session_centroids()` returned the session's real (non-empty) clusters
/// at resolve time — i.e. nothing clobbered the diarizer first.
pub(super) struct RecordingSpeakerStore {
    /// Cluster IDs that reached the resolver (via `link_utterances`'s
    /// `"Speaker N"` label / a `create`). Empty ⇒ resolution never ran on
    /// any real centroid.
    pub(super) created: Mutex<Vec<usize>>,
}

impl RecordingSpeakerStore {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            created: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl crate::speakers::SpeakerStore for RecordingSpeakerStore {
    async fn list_with_embeddings(&self) -> Result<Vec<(i64, Vec<f32>, i64)>> {
        // No known identities → the resolver takes the "first known
        // speaker" branch and calls `create` for each non-empty cluster.
        Ok(Vec::new())
    }
    async fn create(&self, centroid: &[f32], _utterance_count: i64) -> Result<i64> {
        // Record that a real centroid reached the resolver. The length is
        // 256 for the CentroidDiarizer fixture; record one entry per call.
        self.created.lock().unwrap().push(centroid.len());
        Ok(1)
    }
    async fn update_centroid(&self, _id: i64, _c: &[f32], _n: i64) -> Result<()> {
        Ok(())
    }
    async fn link_utterances(&self, _sid: i64, _label: &str, _id: i64) -> Result<()> {
        Ok(())
    }
    async fn rename(&self, _id: i64, _name: Option<String>) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _id: i64) -> Result<()> {
        Ok(())
    }
    async fn list(&self) -> Result<Vec<crate::speakers::SpeakerIdentity>> {
        Ok(Vec::new())
    }
    async fn merge(&self, _keep: i64, _absorb: i64) -> Result<()> {
        Ok(())
    }
}

/// Like [`build_tail_pump_context`] but lets the caller inject the diarizer
/// (so it can be a `FlagGatedDiarizer` reading through a hot-swappable
/// [`crate::diarization::DiarizeSlot`]), the speaker store, and the
/// speaker-identity-enabled flag. Used by the #667 regression to prove the
/// background finalization reads the session's real centroids when the slot
/// stays stable across the stop boundary.
#[allow(clippy::type_complexity)]
pub(super) async fn build_tail_pump_context_with_diarize(
    tail_samples: Vec<f32>,
    diarize: Arc<dyn crate::diarization::Diarize>,
    speaker_store: Arc<dyn crate::speakers::SpeakerStore>,
    speaker_identity_enabled: bool,
) -> (super::pump::PumpContext, Arc<dyn MeetingSessionRepository>) {
    use std::sync::atomic::AtomicBool;

    let (mut ctx, repo, _fed_lens) = build_tail_pump_context(tail_samples).await;
    ctx.diarize = diarize;
    ctx.speaker_store = speaker_store;
    ctx.speaker_identity_enabled = Arc::new(AtomicBool::new(speaker_identity_enabled));
    (ctx, repo)
}
