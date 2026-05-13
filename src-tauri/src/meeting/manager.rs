//! Meeting Mode session manager — owns the [`SessionState`] /
//! [`ActiveSession`] types and the per-session wiring (classifier,
//! audio backend, transcribe slot, partials map, event emitter,
//! diarizer).
//!
//! Lifecycle methods (`start_manual`, `stop_manual`,
//! `append_if_active`) live in [`super::lifecycle`] — extracted
//! under #488 so each file has one job. The chunking pump lives
//! in [`super::pump`] (#108). Speaker-label fallback + classifier
//! defaults live in [`super::classifier`] (#431).
//!
//! ## Privacy invariant (load-bearing)
//!
//! The manager only ever sees `Utterance`s from the transcription
//! layer — never raw audio bytes that survive the transcribe call.
//! `CapturedAudio.samples` is owned by the transcription closure
//! and dropped when it returns; the persistence layer only sees
//! text + timestamps.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

#[cfg(test)]
use anyhow::{anyhow, Result};

use crate::audio::AudioCapture;
#[cfg(test)]
use crate::audio::CapturedAudio;
#[cfg(test)]
use crate::audio::{AudioSession, AudioSource};
#[cfg(test)]
use crate::transcription::Transcribe;
use crate::transcription::Utterance;

use super::classifier::AppClassifier;
#[cfg(test)]
use super::MeetingAppKind;
use super::MeetingSessionRepository;

/// Test-only no-op audio backend used by `SessionManager::new_for_test`.
/// Returns empty capture sessions instantly so the pump's spawn path
/// runs without a real audio device. Lives at module scope (not in
/// the `tests` submod) so the test-only `new_for_test` constructor
/// can reach it from outside its own test module — IPC-layer tests
/// in `crate::ipc` use it via `SessionManager::new_for_test`.
#[cfg(test)]
struct NoOpAudio;

#[cfg(test)]
impl AudioCapture for NoOpAudio {
    fn list_input_devices(&self) -> Result<Vec<crate::audio::AudioDevice>> {
        Ok(vec![])
    }
    fn start(&self, _: Option<&str>) -> Result<()> {
        Ok(())
    }
    fn stop(&self) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: crate::audio::CaptureFormat {
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

#[cfg(test)]
struct NoOpSession {
    source: AudioSource,
}

#[cfg(test)]
impl AudioSession for NoOpSession {
    fn source(&self) -> &AudioSource {
        &self.source
    }
    fn stop(self: Box<Self>) -> Result<CapturedAudio> {
        Ok(CapturedAudio {
            samples: vec![],
            format: crate::audio::CaptureFormat {
                sample_rate: 16_000,
                channels: 1,
            },
        })
    }
}

/// Test-only override repo. Returns an empty list so the
/// classifier falls through to the static defaults — same behaviour
/// the pre-#112 SessionManager exhibited.
#[cfg(test)]
struct NoOpAppOverrides;

#[cfg(test)]
#[async_trait::async_trait]
impl super::MeetingAppOverrideRepository for NoOpAppOverrides {
    async fn list(&self) -> Result<Vec<super::MeetingAppOverride>> {
        Ok(vec![])
    }
    async fn upsert(&self, _: super::NewMeetingAppOverride) -> Result<super::MeetingAppOverride> {
        Err(anyhow!("NoOpAppOverrides::upsert not supported"))
    }
    async fn set_profile(
        &self,
        _: &str,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<super::MeetingAppOverride> {
        Err(anyhow!("NoOpAppOverrides::set_profile not supported"))
    }
    async fn delete(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// Manages the lifecycle of meeting-mode sessions.
///
/// Holds an in-memory pointer to the currently-active session id so
/// the IPC layer's `stop_dictation` path can append utterances to it
/// without re-querying the database. The pointer is `Mutex<Option<i64>>`:
/// `None` means no session active; `Some(id)` means dictation results
/// route into that session's `utterances` table in addition to the
/// existing history insert.
///
/// `Arc<dyn MeetingSessionRepository>` is held internally so the
/// manager owns the persistence handle without forcing every call
/// site to thread it through. Cheap to clone (`Arc`).
/// Wire-shape for the `meeting:source-failed` Tauri event. Fired
/// when the meeting pump drops a per-source capture path mid-session
/// (TCC revoke, device unplug, inference panic). Without this
/// signal the panel keeps showing "recording from mic + system
/// audio" while one of those sources has silently gone dead.
///
/// Lives here (rather than in the pump or the IPC adapter) because
/// both sides reference the field shape — the pump emits, the
/// frontend listens. `camelCase` so the Tauri JSON bridge gives
/// JS consumers idiomatic field names.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingSourceFailedPayload<'a> {
    pub session_id: i64,
    pub source_kind: &'a str,
    pub reason: &'a str,
    /// `true` when the failure came from `audio::DeviceLost` — the
    /// user's mic / AirPods disconnected mid-session or vanished
    /// during pre-warm. Lets the frontend branch on a typed flag
    /// instead of substring-matching `reason`, which #617 flagged
    /// as fragile to backend wording changes.
    pub device_lost: bool,
}

/// Tauri event name the pump fires through
/// [`crate::events::EventEmitter::emit`] when [`MeetingSourceFailedPayload`]
/// is the wire body. Centralised so the frontend's listener
/// (`Events.MeetingSourceFailed`) and the backend emit site can't
/// drift.
pub(super) const MEETING_SOURCE_FAILED_EVENT: &str = "meeting:source-failed";

/// Payload emitted by [`SessionManager::start_manual`] when a new session
/// opens successfully (both manual button-press and HAL auto-start paths).
/// Centralised so the frontend's listener (`Events.MeetingSessionStarted`)
/// and every backend emit site stay in sync.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeetingSessionStartedPayload {
    pub session_id: i64,
}

/// Tauri event name for [`MeetingSessionStartedPayload`]. Matches the
/// TypeScript constant `Events.MeetingSessionStarted` in `events.ts`.
pub(super) const MEETING_SESSION_STARTED_EVENT: &str = "meeting:session-started";

pub struct SessionManager {
    // All fields are `pub(super)` so the lifecycle peer
    // (`crate::meeting::lifecycle`) can drive `start_manual` /
    // `stop_manual` / `append_if_active` without going through
    // accessor noise. Visibility is scoped to `super` (= `meeting`)
    // — outside the meeting module the fields stay private.
    pub(super) repo: Arc<dyn MeetingSessionRepository>,
    /// User-overrides repo (#112). Read at every session start so
    /// edits in the Settings panel take effect without an app
    /// restart. The cached `classifier` field below is rebuilt from
    /// a fresh override snapshot inside `start_manual`.
    pub(super) app_overrides: Arc<dyn super::MeetingAppOverrideRepository>,
    pub(super) classifier: AppClassifier,
    /// Audio backend the pump uses to open per-source capture
    /// sessions. Cloned from `AppState::audio` at construction.
    pub(super) audio: Arc<dyn AudioCapture>,
    /// Live transcribe handle. Same `Arc<Mutex<...>>` `AppState`
    /// holds so model hot-swap reaches in-flight pumps on the
    /// next chunk automatically.
    pub(super) transcribe: crate::ipc::TranscribeSlot,
    /// Session state, see [`SessionState`]. The `Opening` sentinel
    /// is what makes concurrent `start_manual` calls safe: the
    /// first call flips Idle → Opening under the lock, drops the
    /// lock for the async DB / handle work, and only then commits
    /// to Active. A second concurrent call sees `Opening` and
    /// rejects, instead of slipping past the precondition check
    /// and creating an orphan session.
    pub(super) state: Mutex<SessionState>,
    /// In-memory in-flight partial utterances, keyed by
    /// `session_id` then by `speaker_label` ("mic" / "system"). The
    /// streaming pump (#108 PR3) updates these on each inference
    /// tick; the meeting-mode IPC merges them into `meeting_session_get`'s
    /// response so the panel renders revising partials without a
    /// new event channel. Cleared per-source when the pump
    /// commits a final, and entirely on session stop.
    ///
    /// `RwLock` because the IPC poll path (~1/s) reads these and
    /// the pump tick (~2/s) writes them — readers shouldn't block
    /// each other, and the pump's brief exclusive write fits
    /// inside one tick.
    ///
    /// Inner key is `String` (the speaker label) rather than
    /// `&'static str` so a future per-speaker diarization (#111)
    /// can drop in without changing this map's shape.
    pub(super) partials: Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    /// Surface pump-side events (per-source failure mid-session) to
    /// the frontend. Production wires this to a
    /// [`crate::ipc::events::TauriEventEmitter`]; tests use
    /// [`crate::events::NoopEventEmitter`] or a
    /// `RecordingEventEmitter` that captures emit calls for
    /// assertion. The pump fires `meeting:source-failed` through
    /// this seam (see [`MeetingSourceFailedPayload`]).
    pub(super) event_emitter: Arc<dyn crate::events::EventEmitter>,
    /// Speaker diarization. Production wires
    /// [`crate::diarization::FlagGatedDiarizer`] which routes to
    /// [`crate::diarization::onnx::OnnxDiarizer`] when the
    /// Speakers toggle is on and the wespeaker model is loaded,
    /// else [`crate::diarization::NoopDiarizer`]. When the
    /// diarizer abstains, `dispatch_utterances` falls back to
    /// the source-derived `"mic"` / `"system"` tag from
    /// `AudioSource::speaker_tag()`.
    pub(super) diarize: Arc<dyn crate::diarization::Diarize>,
    /// Live microphone gain in dB (#531). Shared Arc from `RuntimeFlags`.
    /// Passed to `PumpContext` at session start so the pump can apply
    /// the current gain value each tick without a session restart.
    pub(super) mic_gain_db: Arc<AtomicU32>,
}

/// Lifecycle state for the manager's session slot. Three-valued
/// rather than `Option<ActiveSession>` because the start path needs
/// an intermediate "I have claimed the slot, but the DB row /
/// capture handles aren't open yet" state. Without it, two
/// concurrent `start_manual` IPC calls could both observe `None`
/// before either commits, and end up creating two database rows /
/// pump tasks for what the user expects to be one session.
pub(super) enum SessionState {
    Idle,
    Opening,
    Active(ActiveSession),
}

/// In-memory state for an open meeting session. Held inside the
/// manager's `active` mutex; `None` means no session in flight.
pub(super) struct ActiveSession {
    pub(super) id: i64,
    /// Wall-clock start. Used by the pump to compute per-utterance
    /// `started_at_ms` / `ended_at_ms` offsets that don't drift
    /// across out-of-order chunk completions (chunk N+1 transcribes
    /// faster than chunk N).
    pub(super) started_at: Instant,
    /// Cancellation flag the pump task polls between sleeps. Set on
    /// `stop_manual`; the pump completes its in-flight chunk, drains
    /// + transcribes one final time, then exits.
    pub(super) cancel: Arc<AtomicBool>,
    /// Pump task. Joined on `stop_manual` so the final chunk's
    /// transcription + append are observed before the session row
    /// is closed. Wrapped in `Mutex<Option<...>>` so `stop_manual`
    /// can take it out without the borrow checker complaining.
    pub(super) pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Set to `true` when `stop_manual`'s `repo.close_session`
    /// call fails and the recovery path restores the session for
    /// a retry (#249). A subsequent `stop_manual` then skips the
    /// cancel + pump-join steps (the pump is already gone — it
    /// finished and exited on the first try) and goes straight
    /// to retrying the DB close. Without this flag the second
    /// stop would store `true` into a fresh `AtomicBool` no
    /// task reads, and `take()` an already-empty `pump_handle`,
    /// burning the user's "let me retry" intent on no-op work.
    pub(super) close_attempted: bool,
}

impl SessionManager {
    /// Boot-time reconciliation: any sessions whose `ended_at` is
    /// still NULL are leftover from a previous run that exited
    /// without `stop_manual` running (process kill, OS crash,
    /// panic). Mark them closed so the panel doesn't render a
    /// "session in progress" badge for a session whose pump task
    /// died with the previous process (#249).
    ///
    /// Best-effort: a transient DB failure during reconciliation
    /// is logged and swallowed — it shouldn't block app startup.
    /// On the next run we'll try again, and the panel surfaces
    /// these sessions with `endedAt: null` either way (so the
    /// failure mode is "weird in-progress badge for a clearly
    /// historical session" rather than "app won't start").
    pub async fn reconcile_orphan_sessions(&self) {
        let open = match self.repo.list_open_sessions().await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "meeting reconcile: failed to list open sessions; skipping"
                );
                return;
            }
        };
        if open.is_empty() {
            return;
        }
        tracing::info!(
            count = open.len(),
            "meeting reconcile: closing {} orphan session(s) from previous run",
            open.len()
        );
        for session in open {
            if let Err(e) = self.repo.close_session(session.id).await {
                tracing::warn!(
                    error = ?e,
                    session_id = session.id,
                    "meeting reconcile: failed to close orphan session"
                );
            }
        }
    }

    pub fn new(
        repo: Arc<dyn MeetingSessionRepository>,
        audio: Arc<dyn AudioCapture>,
        transcribe: crate::ipc::TranscribeSlot,
        event_emitter: Arc<dyn crate::events::EventEmitter>,
        diarize: Arc<dyn crate::diarization::Diarize>,
        app_overrides: Arc<dyn super::MeetingAppOverrideRepository>,
        mic_gain_db: Arc<AtomicU32>,
    ) -> Self {
        Self {
            repo,
            app_overrides,
            classifier: AppClassifier::default_table(),
            audio,
            transcribe,
            state: Mutex::new(SessionState::Idle),
            partials: Arc::new(RwLock::new(HashMap::new())),
            event_emitter,
            diarize,
            mic_gain_db,
        }
    }

    /// Snapshot of the currently in-flight partial utterances for a
    /// session, one entry per source ("mic" + "system" with the
    /// canonical labels the pump uses today). Returns an empty Vec
    /// if the session has no in-flight partials yet.
    ///
    /// The IPC layer's `meeting_session_get` calls this and merges
    /// the result into the response so the panel's poll sees
    /// partials alongside the persisted finals. The list ordering
    /// is alphabetical-by-label so the rendering order is stable
    /// across polls (frontend rendering reads this verbatim,
    /// not a sorted clone).
    pub fn current_partials_for(&self, session_id: i64) -> Vec<Utterance> {
        let guard = match self.partials.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(per_session) = guard.get(&session_id) else {
            return Vec::new();
        };
        let mut out: Vec<Utterance> = per_session.values().cloned().collect();
        out.sort_by(|a, b| {
            a.speaker_label
                .as_deref()
                .unwrap_or("")
                .cmp(b.speaker_label.as_deref().unwrap_or(""))
        });
        out
    }

    /// Test-only constructor that wires the manager up against a
    /// no-op audio backend and an empty transcribe slot. Use from
    /// IPC-layer tests where the manager is constructed but its
    /// pump path is not exercised — keeps each call site from
    /// repeating the stub-audio plumbing.
    #[cfg(test)]
    pub fn new_for_test(repo: Arc<dyn MeetingSessionRepository>) -> Self {
        let audio: Arc<dyn AudioCapture> = Arc::new(NoOpAudio);
        let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> = Arc::new(Mutex::new(None));
        let emitter: Arc<dyn crate::events::EventEmitter> =
            Arc::new(crate::events::NoopEventEmitter);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let app_overrides: Arc<dyn super::MeetingAppOverrideRepository> =
            Arc::new(NoOpAppOverrides);
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

    // `start_manual`, `stop_manual`, and `append_if_active` live in
    // `crate::meeting::lifecycle` — extracted under #488. The state
    // machine + struct definitions stay here; the methods that drive
    // the state machine live in the peer module so each file has one
    // job. See lifecycle.rs's module docs for the visibility
    // rationale (every relevant field on `SessionManager`,
    // `ActiveSession`, and `SessionState` is `pub(super)`).

    /// Read-only snapshot of the active session id, if any. The
    /// frontend polls this on mount + after every state change so
    /// the panel can render "session in progress" affordances.
    pub fn active_session_id(&self) -> Option<i64> {
        self.state.lock().ok().and_then(|guard| match &*guard {
            SessionState::Active(a) => Some(a.id),
            SessionState::Idle | SessionState::Opening => None,
        })
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        // App shutdown path: if a meeting session is still active,
        // signal cancel and abort the pump. Tokio aborts cancel at
        // the next await point and unwinds the task — its local
        // `handles: Vec<Box<dyn AudioSession>>` drops, each handle's
        // own `Drop` impl runs to release the cpal mic / SCK
        // streams, and the active-sessions refcount drops back to
        // zero.
        //
        // Without this Drop, an app exit with an open meeting
        // detaches the pump task — it keeps polling the cancel flag
        // (which never flips) and re-opens capture handles on every
        // chunk, churning the audio devices until the runtime
        // itself shuts down. The session row also stays open in the
        // database (no `ended_at`).
        //
        // Best-effort: we can't await `repo.close_session(id)` in a
        // sync Drop, so the row stays unclosed; the next launch
        // sees it as an in-progress meeting that ended with no
        // tail. Worth tightening if it ever bites — the recovery
        // would be a startup pass that closes any sessions whose
        // wall-clock end is older than the app's last known
        // alive-time.
        let active = self.state.lock().ok().and_then(|mut guard| {
            match std::mem::replace(&mut *guard, SessionState::Idle) {
                SessionState::Active(a) => Some(a),
                state @ (SessionState::Opening | SessionState::Idle) => {
                    *guard = state;
                    None
                }
            }
        });
        if let Some(active) = active {
            active.cancel.store(true, Ordering::Release);
            if let Ok(mut guard) = active.pump_handle.lock() {
                if let Some(handle) = guard.take() {
                    handle.abort();
                }
            }
            // Clear partials — Drop on app shutdown shouldn't leave
            // a stale entry that a fresh session with the same id
            // (vanishingly rare but possible across restarts that
            // re-use rowids) could merge into its first poll.
            if let Ok(mut guard) = self.partials.write() {
                guard.remove(&active.id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioDevice, CaptureFormat, CapturedAudio};
    use crate::db::SqliteDatabase;
    use crate::meeting::pump::{diarize_and_dispatch_merged, dispatch_utterances, TickBucket};
    use crate::meeting::SqliteMeetingSessionRepository;

    /// Test-only audio backend that produces empty capture sessions
    /// instantly. Lets `start_manual` succeed without a real mic and
    /// makes the pump's chunk-and-transcribe cycle a no-op (no
    /// samples, no transcript, no utterance appended). The pump task
    /// is still spawned and runs until cancelled, so tests that
    /// exercise start_manual must also call stop_manual to drain it.
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

    async fn fresh_manager() -> SessionManager {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let repo: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
        manager_with_repo(repo)
    }

    /// Same as [`fresh_manager`] but lets the caller supply a
    /// pre-built repo — used by the orphan-reconciliation test
    /// that needs to insert open rows directly before constructing
    /// the manager that should close them at boot.
    fn manager_with_repo(repo: Arc<dyn MeetingSessionRepository>) -> SessionManager {
        let audio: Arc<dyn AudioCapture> = Arc::new(StubParallelAudio);
        let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> = Arc::new(Mutex::new(None));
        let emitter: Arc<dyn crate::events::EventEmitter> =
            Arc::new(crate::events::NoopEventEmitter);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let app_overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
            Arc::new(NoOpAppOverrides);
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

    #[tokio::test]
    async fn start_manual_opens_a_session_and_records_active_id() {
        let mgr = fresh_manager().await;
        assert!(mgr.active_session_id().is_none(), "no session at boot");

        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("us.zoom.xos".into()),
                None,
            )
            .await
            .unwrap();
        assert_eq!(session.app_name, "us.zoom.xos");
        assert_eq!(session.app_kind, MeetingAppKind::Meeting); // classifier lookup
        assert!(session.ended_at.is_none(), "new session is open");

        assert_eq!(mgr.active_session_id(), Some(session.id));
        // Drain the pump so it doesn't outlive the test.
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn start_manual_rejects_concurrent_starts() {
        let mgr = fresh_manager().await;
        mgr.start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();
        let err = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .expect_err("second start must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already active"),
            "error must name the precondition; got: {msg}"
        );
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn start_manual_rejects_empty_sources_and_keeps_state_idle() {
        // The Opening sentinel is what makes concurrent starts safe;
        // pin that an *invalid* start (empty source list) returns
        // the slot to Idle so a follow-up valid start succeeds.
        // Without the rollback, the slot would be stuck in Opening
        // and every subsequent start would error indefinitely.
        let mgr = fresh_manager().await;
        let err = mgr
            .start_manual(Vec::new(), None, None)
            .await
            .expect_err("empty source list must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("at least one audio source"),
            "error must name the precondition; got: {msg}"
        );

        // The slot is back to Idle — a valid start now succeeds.
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .expect("post-rollback start must succeed");
        assert_eq!(mgr.active_session_id(), Some(session.id));
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn stop_manual_closes_the_session_and_clears_active_id() {
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        mgr.stop_manual().await.unwrap();
        assert!(mgr.active_session_id().is_none(), "active cleared on stop");

        // No regression test that ended_at is non-null because the
        // SqliteMeetingSessionRepository::close_session test pins
        // that already; the manager's job is just to call it.
        let _ = session;
    }

    #[tokio::test]
    async fn reconcile_orphan_sessions_closes_open_rows_from_previous_run() {
        // Simulate a previous-process state: rows exist with
        // `ended_at = NULL` because that process never ran
        // `stop_manual`. A fresh manager pointing at the same
        // repo should close them on boot via
        // `reconcile_orphan_sessions` (#249).
        use crate::meeting::NewMeetingSession;

        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let repo: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));

        // Two orphans + one already-closed row to pin that
        // already-closed rows aren't touched.
        let orphan_a = repo
            .create(NewMeetingSession {
                app_name: "Zoom".into(),
                app_kind: MeetingAppKind::Meeting,
                sources: vec!["mic".into()],
                app_title: None,
            })
            .await
            .unwrap();
        let orphan_b = repo
            .create(NewMeetingSession {
                app_name: "Teams".into(),
                app_kind: MeetingAppKind::Meeting,
                sources: vec!["mic".into(), "system".into()],
                app_title: None,
            })
            .await
            .unwrap();
        let already_closed = repo
            .create(NewMeetingSession {
                app_name: "Discord".into(),
                app_kind: MeetingAppKind::Meeting,
                sources: vec!["mic".into()],
                app_title: None,
            })
            .await
            .unwrap();
        repo.close_session(already_closed.id).await.unwrap();
        let closed_ended = repo
            .get_by_id(already_closed.id)
            .await
            .unwrap()
            .unwrap()
            .ended_at;
        assert!(
            closed_ended.is_some(),
            "preflight: already-closed row has ended_at"
        );

        // Fresh manager == fresh process. Reconcile.
        let mgr = manager_with_repo(Arc::clone(&repo));
        mgr.reconcile_orphan_sessions().await;

        // Both orphans now closed; pre-closed row's timestamp
        // hasn't drifted (close_session has the COALESCE guard).
        for id in [orphan_a.id, orphan_b.id] {
            let row = repo.get_by_id(id).await.unwrap().unwrap();
            assert!(
                row.ended_at.is_some(),
                "orphan {id} should be closed by reconcile"
            );
        }
        let after_reconcile = repo
            .get_by_id(already_closed.id)
            .await
            .unwrap()
            .unwrap()
            .ended_at;
        assert_eq!(
            after_reconcile, closed_ended,
            "already-closed row's ended_at must not drift"
        );

        // Idempotent: a second reconcile is a no-op.
        mgr.reconcile_orphan_sessions().await;
        assert!(
            repo.list_open_sessions().await.unwrap().is_empty(),
            "no open rows remain after reconcile"
        );
    }

    #[tokio::test]
    async fn stop_manual_with_no_active_session_errors() {
        let mgr = fresh_manager().await;
        let err = mgr
            .stop_manual()
            .await
            .expect_err("stop without active must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no meeting session active"),
            "error must explain the precondition; got: {msg}"
        );
    }

    /// Failing `close_session` repo wrapper used by the #492 race
    /// tests. Delegates every other call to an inner SQLite repo so
    /// `start_manual` / `append_utterance` work normally; only
    /// `close_session` is overridden. Optional `on_close_session`
    /// callback runs *before* the failure is returned so the test
    /// can inject the "concurrent start_manual claimed the slot"
    /// race condition deterministically.
    struct FailingCloseRepo {
        inner: Arc<dyn MeetingSessionRepository>,
        on_close_session: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    #[async_trait::async_trait]
    impl
        crate::repository::Repository<
            crate::meeting::MeetingSession,
            crate::meeting::NewMeetingSession,
            i64,
        > for FailingCloseRepo
    {
        async fn list(&self) -> Result<Vec<crate::meeting::MeetingSession>> {
            self.inner.list().await
        }
        async fn create(
            &self,
            new: crate::meeting::NewMeetingSession,
        ) -> Result<crate::meeting::MeetingSession> {
            self.inner.create(new).await
        }
        async fn update(&self, item: crate::meeting::MeetingSession) -> Result<()> {
            self.inner.update(item).await
        }
        async fn delete(&self, id: i64) -> Result<()> {
            self.inner.delete(id).await
        }
    }

    #[async_trait::async_trait]
    impl MeetingSessionRepository for FailingCloseRepo {
        async fn close_session(&self, _id: i64) -> Result<()> {
            if let Some(cb) = self.on_close_session.as_ref() {
                cb();
            }
            Err(anyhow!("simulated close_session failure"))
        }
        async fn append_utterance(
            &self,
            new: crate::meeting::NewPersistedUtterance,
        ) -> Result<crate::meeting::PersistedUtterance> {
            self.inner.append_utterance(new).await
        }
        async fn list_utterances(
            &self,
            session_id: i64,
        ) -> Result<Vec<crate::meeting::PersistedUtterance>> {
            self.inner.list_utterances(session_id).await
        }
        async fn set_notes(&self, id: i64, notes: Option<String>) -> Result<()> {
            self.inner.set_notes(id, notes).await
        }
        async fn get_by_id(&self, id: i64) -> Result<Option<crate::meeting::MeetingSession>> {
            self.inner.get_by_id(id).await
        }
        async fn list_open_sessions(&self) -> Result<Vec<crate::meeting::MeetingSession>> {
            self.inner.list_open_sessions().await
        }
        async fn search_sessions(
            &self,
            query: &str,
        ) -> Result<Vec<crate::meeting::MeetingSession>> {
            self.inner.search_sessions(query).await
        }
    }

    /// #492: with no concurrent start in flight, a `close_session`
    /// failure restores the session for retry — preserving the
    /// pre-fix behaviour that #249 relies on. Slot ends up
    /// `Active(old_id)` with `close_attempted = true`.
    #[tokio::test]
    async fn stop_manual_close_failure_restores_session_for_retry_when_idle() {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let inner: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
        let failing: Arc<dyn MeetingSessionRepository> = Arc::new(FailingCloseRepo {
            inner: Arc::clone(&inner),
            on_close_session: None,
        });
        let mgr = manager_with_repo(failing);

        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
            )
            .await
            .unwrap();

        let err = mgr.stop_manual().await.expect_err("close fails by design");
        assert!(format!("{err:#}").contains("simulated close_session failure"));

        // Slot should now be Active(session.id) with close_attempted = true,
        // ready for the user's retry.
        let guard = mgr.state.lock().unwrap();
        match &*guard {
            SessionState::Active(a) => {
                assert_eq!(a.id, session.id, "old session id preserved for retry");
                assert!(
                    a.close_attempted,
                    "close_attempted should be set so retry skips pump teardown"
                );
            }
            SessionState::Idle => panic!("expected Active(old) after close failure; got Idle"),
            SessionState::Opening => {
                panic!("expected Active(old) after close failure; got Opening")
            }
        }
    }

    /// #492 — the race the bug describes. While `stop_manual`
    /// awaits `close_session`, a concurrent `start_manual` claims
    /// the slot for a NEW session. Pre-fix the recovery path
    /// would unconditionally overwrite that with `Active(<old id>)`,
    /// silently dropping the new session. Post-fix the recovery
    /// only fires when the slot is still `Idle` — when it's been
    /// claimed by a new Opening/Active state, the close error
    /// surfaces but the new session survives.
    ///
    /// Implementation: the failing repo's `close_session` callback
    /// flips the slot to a synthetic `Active(<new id>)` state right
    /// before returning Err, simulating the concurrent claim
    /// without spawning real concurrent tasks.
    #[tokio::test]
    async fn stop_manual_close_failure_does_not_clobber_concurrent_start() {
        use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
        use std::sync::OnceLock;

        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let inner: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));

        // The slot-flip callback needs the manager's state mutex,
        // but the manager is built with the failing repo — so the
        // repo can't yet hold an Arc to a manager that doesn't
        // exist. Sidestep with a `OnceLock<Arc<SessionManager>>`:
        // build the failing repo with a callback that reads the
        // OnceLock, build the manager wrapping that repo, then
        // populate the OnceLock with the manager Arc.
        const NEW_SESSION_ID: i64 = 999;
        let mgr_slot: Arc<OnceLock<Arc<SessionManager>>> = Arc::new(OnceLock::new());
        let callback_fired = Arc::new(AtomicI64::new(0));

        let cb_fired = Arc::clone(&callback_fired);
        let mgr_slot_for_cb = Arc::clone(&mgr_slot);
        let on_close: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            // Simulate a concurrent `start_manual` claiming the
            // slot during stop_manual's `close_session` await.
            let mgr = mgr_slot_for_cb
                .get()
                .expect("mgr_slot populated before stop_manual runs");
            let mut guard = mgr.state.lock().unwrap();
            *guard = SessionState::Active(ActiveSession {
                id: NEW_SESSION_ID,
                started_at: std::time::Instant::now(),
                cancel: Arc::new(AtomicBool::new(false)),
                pump_handle: Mutex::new(None),
                close_attempted: false,
            });
            cb_fired.fetch_add(1, AtomicOrdering::Relaxed);
        });

        let failing: Arc<dyn MeetingSessionRepository> = Arc::new(FailingCloseRepo {
            inner: Arc::clone(&inner),
            on_close_session: Some(on_close),
        });
        let mgr = Arc::new(manager_with_repo(failing));
        mgr_slot
            .set(Arc::clone(&mgr))
            .ok()
            .expect("OnceLock populated exactly once");

        // Start a session under the (now-failing) repo. That writes
        // an open row to the DB (via inner) and parks the slot in
        // Active(session.id).
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
            )
            .await
            .unwrap();

        // Now stop. close_session will fail AND its callback will
        // flip the slot to Active(NEW_SESSION_ID), simulating the
        // race. The recovery path must NOT overwrite that.
        let err = mgr.stop_manual().await.expect_err("close fails by design");
        assert!(format!("{err:#}").contains("simulated close_session failure"));
        assert_eq!(
            callback_fired.load(AtomicOrdering::Relaxed),
            1,
            "the slot-flip callback must have fired during close_session"
        );

        // The slot should still be Active(NEW_SESSION_ID) — the
        // pre-fix bug would have overwritten this with
        // Active(session.id, close_attempted: true).
        let guard = mgr.state.lock().unwrap();
        match &*guard {
            SessionState::Active(a) => {
                assert_eq!(
                    a.id, NEW_SESSION_ID,
                    "new session must survive the close-failure recovery; \
                     pre-#492 the old id ({}) clobbered it",
                    session.id
                );
                assert!(
                    !a.close_attempted,
                    "close_attempted must stay false on the new session"
                );
            }
            SessionState::Idle => {
                panic!("expected Active(NEW_SESSION_ID) preserved; got Idle")
            }
            SessionState::Opening => {
                panic!("expected Active(NEW_SESSION_ID) preserved; got Opening")
            }
        }
    }

    #[tokio::test]
    async fn append_if_active_returns_false_when_no_session() {
        let mgr = fresh_manager().await;
        let appended = mgr.append_if_active("hello", 1_000).await.unwrap();
        assert!(!appended, "no session = no append, no error");
    }

    #[tokio::test]
    async fn append_if_active_persists_utterance_with_cumulative_timestamps() {
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
            )
            .await
            .unwrap();

        let appended = mgr.append_if_active("first sentence", 2_000).await.unwrap();
        assert!(appended);
        let appended = mgr
            .append_if_active("second sentence", 3_000)
            .await
            .unwrap();
        assert!(appended);

        // Cumulative-ms arithmetic: first @ [0, 2000], second @
        // [2000, 5000]. Pinned because the panel renders these as
        // a timeline; a regression that drops the cumulative
        // adjustment would render every utterance starting at 0.
        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 2);
        assert_eq!(utterances[0].started_at_ms, 0);
        assert_eq!(utterances[0].ended_at_ms, 2_000);
        assert_eq!(utterances[1].started_at_ms, 2_000);
        assert_eq!(utterances[1].ended_at_ms, 5_000);
        mgr.stop_manual().await.unwrap();
    }

    // -- Partials store + dispatch -------------------------------------
    //
    // The streaming pump's per-tick output goes through
    // `dispatch_utterances`: finals land in the DB and clear the
    // matching in-memory partial; partials replace the in-memory
    // entry for their source. The IPC layer reads the partials store
    // via `current_partials_for` to merge into `meeting_session_get`.
    // Unit-test the dispatch + read path against a real
    // SqliteMeetingSessionRepository to exercise both halves of the
    // contract.

    fn make_partial(text: &str, started: u64, ended: u64, label: &str) -> Utterance {
        Utterance {
            text: text.to_owned(),
            started_at_ms: started,
            ended_at_ms: ended,
            is_final: false,
            speaker_label: Some(label.to_owned()),
        }
    }

    fn make_final(text: &str, started: u64, ended: u64, label: &str) -> Utterance {
        Utterance {
            text: text.to_owned(),
            started_at_ms: started,
            ended_at_ms: ended,
            is_final: true,
            speaker_label: Some(label.to_owned()),
        }
    }

    #[tokio::test]
    async fn current_partials_for_returns_empty_for_new_session() {
        // Pin: a session with no in-flight inference yet has no
        // partials. The IPC poll path relies on this to return an
        // empty Vec rather than None / errors.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();
        let partials = mgr.current_partials_for(session.id);
        assert!(partials.is_empty(), "no partials at session start");
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_partial_is_readable_via_current_partials_for() {
        // Pin the pump's partial-write path: a partial dispatched
        // for session S + label "mic" appears in the IPC poll's
        // response on the next call.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("revising tail", 1_500, 3_000, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let partials = mgr.current_partials_for(session.id);
        assert_eq!(partials.len(), 1);
        assert_eq!(partials[0].text, "revising tail");
        assert_eq!(partials[0].started_at_ms, 1_500);
        assert!(!partials[0].is_final);
        assert_eq!(partials[0].speaker_label.as_deref(), Some("mic"));

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_partial_replaces_previous_partial_for_same_source() {
        // The pump's per-source slot holds at most one partial. A
        // newer partial for the same source overwrites the older
        // one — that's the "in-flight tail revising" behaviour the
        // panel's italic treatment depends on.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("hello", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("hello world", 0, 1_500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let partials = mgr.current_partials_for(session.id);
        assert_eq!(partials.len(), 1, "one partial per source, not stacked");
        assert_eq!(partials[0].text, "hello world");
        assert_eq!(partials[0].ended_at_ms, 1_500);

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_keeps_per_source_partials_independent() {
        // mic + system run their own streaming sessions; the
        // partials store keys by speaker_label so the two don't
        // overwrite each other. Pin so a future map-shape change
        // (e.g. switching to Vec<Utterance>) preserves
        // independence.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("you side", 0, 1_000, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        dispatch_utterances(
            session.id,
            "system",
            vec![make_partial("remote side", 0, 1_000, "system")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let partials = mgr.current_partials_for(session.id);
        assert_eq!(partials.len(), 2);
        // Sorted alphabetically by label — "mic" before "system".
        assert_eq!(partials[0].speaker_label.as_deref(), Some("mic"));
        assert_eq!(partials[0].text, "you side");
        assert_eq!(partials[1].speaker_label.as_deref(), Some("system"));
        assert_eq!(partials[1].text, "remote side");

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_final_clears_matching_partial_and_persists_row() {
        // The keystone handoff: a final dispatched for session S +
        // label L (a) appears in repo.list_utterances and (b)
        // removes the partial for the same label from the in-memory
        // store. Without (b) the panel would briefly show the same
        // text twice (italic partial + solid final) until the next
        // partial overwrote.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("about to firm up", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        assert_eq!(mgr.current_partials_for(session.id).len(), 1);

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("about to firm up", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        // Partial slot for "mic" is cleared.
        assert!(
            mgr.current_partials_for(session.id).is_empty(),
            "final commits should clear the matching partial"
        );

        // Final lands in the DB.
        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].text, "about to firm up");
        assert_eq!(utterances[0].speaker_label.as_deref(), Some("mic"));

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_final_does_not_clear_partial_for_other_source() {
        // Cross-source isolation: a final for "mic" shouldn't clear
        // the in-flight partial for "system". Pin so a future bug
        // that uses the wrong key doesn't silently cause partials
        // from one source to vanish on the other's commit.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "system",
            vec![make_partial("remote still talking", 0, 2_000, "system")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("you finished a sentence", 0, 1_500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let partials = mgr.current_partials_for(session.id);
        assert_eq!(partials.len(), 1, "system partial must survive mic final");
        assert_eq!(partials[0].speaker_label.as_deref(), Some("system"));
        assert_eq!(partials[0].text, "remote still talking");

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_skips_empty_finals() {
        // A whitespace-only final shouldn't pollute the persisted
        // history. The streaming session usually filters these but
        // dispatch is the last line of defence.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("   ", 0, 1_000, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert!(
            utterances.is_empty(),
            "whitespace final must not be persisted"
        );
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_preserves_pre_set_speaker_label() {
        // Post-#111 contract: when a diarizer has already stamped
        // `speaker_label` (e.g. with "Speaker A"), dispatch must NOT
        // overwrite it with the source-derived fallback. Pin so a
        // future refactor that drops the `is_none()` guard fails loud.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let mut u = make_final("hello world", 0, 1_000, "mic");
        u.speaker_label = Some("Speaker A".to_owned());

        dispatch_utterances(
            session.id,
            // The source label is "system" but the diarizer-set
            // "Speaker A" wins; the fallback is only applied when
            // the label is None.
            "system",
            vec![u],
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].speaker_label.as_deref(), Some("Speaker A"));
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_falls_back_to_source_label_when_unlabelled() {
        // Symmetric: when the diarizer is Noop (or otherwise leaves
        // `speaker_label = None`), dispatch fills the slot with the
        // source-derived label so the panel always has something to
        // colour-code by.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let mut u = make_final("hello", 0, 1_000, "");
        u.speaker_label = None;

        dispatch_utterances(session.id, "system", vec![u], &mgr.partials, &mgr.repo).await;

        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].speaker_label.as_deref(), Some("system"));
        mgr.stop_manual().await.unwrap();
    }

    /// Recording diarizer for the merged-dispatch test (#206). Saves
    /// the chronological sequence of `started_at_ms` values it
    /// receives + the audio chunk lengths (#111 PR-F), then writes
    /// deterministic `"Speaker A"` labels so the test can assert
    /// order without standing up a real diarizer.
    struct RecordingDiarizer {
        seen_starts: Mutex<Vec<u64>>,
        seen_audio_lens: Mutex<Vec<usize>>,
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

    #[tokio::test]
    async fn diarize_and_dispatch_merged_runs_diarizer_in_chronological_order() {
        // The whole point of #206: pre-fix the diarizer ran twice
        // (once per source), each time over its own per-source
        // chronological slice. Post-fix it runs ONCE over the
        // merged-and-sorted batch — so a mic utterance at t=100
        // followed by a system utterance at t=200 is what the
        // diarizer sees, regardless of how the pump assembled the
        // tick buckets.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
            seen_audio_lens: Mutex::new(Vec::new()),
        });
        let recorder_dyn: Arc<dyn crate::diarization::Diarize> = recorder.clone();

        // Mic finals at t=200 and t=400; system finals at t=100 and
        // t=300. The pump assembles buckets in source order
        // (mic-first then system), so the merge step has to
        // re-order chronologically.
        let mic_bucket = TickBucket {
            source_label: "mic".to_owned(),
            utterances: vec![
                make_final("mic-200", 200, 280, "mic"),
                make_final("mic-400", 400, 480, "mic"),
            ],
            audio: vec![Vec::new(), Vec::new()],
        };
        let sys_bucket = TickBucket {
            source_label: "system".to_owned(),
            utterances: vec![
                make_final("sys-100", 100, 180, "system"),
                make_final("sys-300", 300, 380, "system"),
            ],
            audio: vec![Vec::new(), Vec::new()],
        };

        diarize_and_dispatch_merged(
            session.id,
            vec![mic_bucket, sys_bucket],
            &recorder_dyn,
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        // The diarizer saw all four starts in chronological order.
        let seen = recorder.seen_starts.lock().unwrap().clone();
        assert_eq!(seen, vec![100, 200, 300, 400]);

        // All four landed in the DB; mic ones tagged "mic" pre-
        // dispatch (by RecordingDiarizer's "Speaker A" label, which
        // dispatch_utterances respects via its is_none guard).
        let persisted = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(persisted.len(), 4);
        for u in &persisted {
            assert_eq!(
                u.speaker_label.as_deref(),
                Some("Speaker A"),
                "diarizer label should win over the source fallback"
            );
        }

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn diarize_and_dispatch_merged_is_a_no_op_for_empty_buckets() {
        // Defensive: the pump only calls into the helper when the
        // tick produced at least one utterance, but pin the empty-
        // path behaviour so a future caller can't crash through it.
        let mgr = fresh_manager().await;
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);

        diarize_and_dispatch_merged(0, vec![], &diarize, &mgr.partials, &mgr.repo).await;
        diarize_and_dispatch_merged(
            0,
            vec![TickBucket {
                source_label: "mic".into(),
                utterances: vec![],
                audio: vec![],
            }],
            &diarize,
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        // No assertions needed beyond "didn't panic"; mgr.repo is
        // empty so the existing list_utterances path covers it.
    }

    #[tokio::test]
    async fn diarize_and_dispatch_merged_threads_per_utterance_audio() {
        // #111 PR-F: the dispatch path must hand each utterance's
        // audio chunk to the diarizer in the same chronological
        // order as the utterances. Without this, OnnxDiarizer's
        // length-mismatch guard short-circuits and the feature is
        // a no-op.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
            seen_audio_lens: Mutex::new(Vec::new()),
        });
        let recorder_dyn: Arc<dyn crate::diarization::Diarize> = recorder.clone();

        // Two source buckets, distinct audio sizes per utterance so
        // the assertion is unambiguous.
        let mic_bucket = TickBucket {
            source_label: "mic".to_owned(),
            utterances: vec![
                make_final("mic-200", 200, 280, "mic"),
                make_final("mic-400", 400, 480, "mic"),
            ],
            // 200 and 400 samples respectively — distinct from the
            // system bucket so we can verify ordering.
            audio: vec![vec![0.0; 200], vec![0.0; 400]],
        };
        let sys_bucket = TickBucket {
            source_label: "system".to_owned(),
            utterances: vec![
                make_final("sys-100", 100, 180, "system"),
                make_final("sys-300", 300, 380, "system"),
            ],
            audio: vec![vec![0.0; 100], vec![0.0; 300]],
        };

        diarize_and_dispatch_merged(
            session.id,
            vec![mic_bucket, sys_bucket],
            &recorder_dyn,
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        // The diarizer received audio chunks in chronological order
        // (sys-100 → mic-200 → sys-300 → mic-400), so the recorded
        // lengths must be [100, 200, 300, 400].
        let lens = recorder.seen_audio_lens.lock().unwrap().clone();
        assert_eq!(
            lens,
            vec![100, 200, 300, 400],
            "audio chunks must be threaded in same chronological order as utterances"
        );
    }

    #[tokio::test]
    async fn diarize_and_dispatch_merged_recovers_from_audio_length_mismatch() {
        // Defensive: if the pump and dispatch fall out of sync (a
        // bug or a future refactor), the dispatch path falls back
        // to empty audio chunks rather than panicking. The diarizer
        // still runs (just without signal); source-only labels
        // stand. This is the "we'd rather degrade than crash" path.
        //
        // Uses two buckets (mic + system) so the single-source
        // guard from #369 doesn't kick in — that guard
        // intentionally skips the diarizer for single-source input,
        // which would otherwise mask the length-mismatch behaviour
        // this test pins.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
            seen_audio_lens: Mutex::new(Vec::new()),
        });
        let recorder_dyn: Arc<dyn crate::diarization::Diarize> = recorder.clone();

        // Mic bucket has 2 utterances but only 1 audio chunk — the
        // mismatch should trigger the fallback to empty chunks for
        // those two utterances. System bucket is well-formed so the
        // multi-source path runs the diarizer.
        let mic_bucket = TickBucket {
            source_label: "mic".to_owned(),
            utterances: vec![
                make_final("a", 100, 200, "mic"),
                make_final("b", 300, 400, "mic"),
            ],
            audio: vec![vec![0.0; 50]],
        };
        let sys_bucket = TickBucket {
            source_label: "system".to_owned(),
            utterances: vec![make_final("s", 250, 350, "system")],
            audio: vec![vec![0.0; 75]],
        };

        diarize_and_dispatch_merged(
            session.id,
            vec![mic_bucket, sys_bucket],
            &recorder_dyn,
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        // Chronological order: mic-100 (empty fallback), sys-250
        // (75), mic-300 (empty fallback). Two zeros from the
        // mismatched mic bucket, one 75 from the well-formed
        // system bucket.
        let lens = recorder.seen_audio_lens.lock().unwrap().clone();
        assert_eq!(
            lens,
            vec![0, 75, 0],
            "length-mismatch fallback should hand the diarizer empty audio chunks for the broken bucket only"
        );
    }

    #[tokio::test]
    async fn diarize_and_dispatch_merged_skips_diarizer_for_single_source() {
        // #369: when only one source bucket arrives — the canonical
        // case once the unified Record flow runs in mic-only mode —
        // the ONNX diarizer call is skipped because its
        // multi-speaker labelling is wasted (and noisy: spurious
        // Speaker A / Speaker B alternation against a single
        // talker). Utterances flow through with their source-
        // derived labels intact via dispatch_utterances' fallback.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
            seen_audio_lens: Mutex::new(Vec::new()),
        });
        let recorder_dyn: Arc<dyn crate::diarization::Diarize> = recorder.clone();

        let bucket = TickBucket {
            source_label: "mic".to_owned(),
            utterances: vec![
                make_final("a", 100, 200, "mic"),
                make_final("b", 300, 400, "mic"),
            ],
            audio: vec![vec![0.0; 200], vec![0.0; 400]],
        };

        diarize_and_dispatch_merged(
            session.id,
            vec![bucket],
            &recorder_dyn,
            &mgr.partials,
            &mgr.repo,
        )
        .await;

        let seen = recorder.seen_starts.lock().unwrap().clone();
        assert!(
            seen.is_empty(),
            "diarizer should not be invoked on single-source input; saw {:?}",
            seen
        );

        // Utterances still landed in the DB with their source
        // label — the dispatch fallback at dispatch_utterances
        // ensures speaker_label is populated even without the
        // diarizer's contribution.
        let persisted = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(persisted.len(), 2);
        for u in &persisted {
            assert_eq!(u.speaker_label.as_deref(), Some("mic"));
        }

        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn stop_manual_clears_partials_for_the_session() {
        // Defence in depth: stop_manual clears any partials still
        // in the store for the closing session. Without this, a
        // subsequent IPC poll between stop_manual returning and the
        // pump's last dispatch could expose a stale partial.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None, None)
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("incomplete", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
        )
        .await;
        assert_eq!(mgr.current_partials_for(session.id).len(), 1);

        mgr.stop_manual().await.unwrap();
        assert!(
            mgr.current_partials_for(session.id).is_empty(),
            "stop_manual must clear in-flight partials"
        );
    }

    #[test]
    fn classifier_recognises_default_meeting_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("us.zoom.xos"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Microsoft Teams"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Discord"), MeetingAppKind::Meeting);
    }

    #[test]
    fn classifier_recognises_meeting_apps_across_platforms() {
        // The default table has to cover the variant strings each
        // OS's `active-win-pos-rs` returns: macOS bundle ids,
        // Linux process names, Windows .exe basenames. Pin a
        // sample of each per app so a future "drop a Windows
        // entry while refactoring" regression fails loud.
        let c = AppClassifier::default_table();
        // macOS bundle IDs
        assert_eq!(c.classify("us.zoom.xos"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("com.microsoft.teams2"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("com.hnc.Discord"), MeetingAppKind::Meeting);
        assert_eq!(
            c.classify("com.tinyspeck.slackmacgap"),
            MeetingAppKind::Meeting
        );
        // Linux process names (lowercase)
        assert_eq!(c.classify("zoom"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("discord"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("slack"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("teams-for-linux"), MeetingAppKind::Meeting);
        // Windows executables
        assert_eq!(c.classify("Zoom.exe"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("ms-teams.exe"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Discord.exe"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("slack.exe"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Webex.exe"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Skype.exe"), MeetingAppKind::Meeting);
    }

    #[test]
    fn classifier_recognises_default_media_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("Spotify"), MeetingAppKind::Media);
        assert_eq!(c.classify("YouTube"), MeetingAppKind::Media);
    }

    #[test]
    fn classifier_recognises_media_apps_across_platforms() {
        // Same shape as the meeting-apps cross-platform pin, for
        // the media side of the table.
        let c = AppClassifier::default_table();
        // macOS bundle IDs / display names
        assert_eq!(c.classify("com.spotify.client"), MeetingAppKind::Media);
        assert_eq!(c.classify("Apple Music"), MeetingAppKind::Media);
        // Linux process names
        assert_eq!(c.classify("spotify"), MeetingAppKind::Media);
        assert_eq!(c.classify("vlc"), MeetingAppKind::Media);
        // Windows executables
        assert_eq!(c.classify("Spotify.exe"), MeetingAppKind::Media);
        assert_eq!(c.classify("vlc.exe"), MeetingAppKind::Media);
        assert_eq!(c.classify("Plex.exe"), MeetingAppKind::Media);
        assert_eq!(c.classify("iTunes.exe"), MeetingAppKind::Media);
    }

    #[test]
    fn classifier_returns_other_for_unknown_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("RandomEditor.app"), MeetingAppKind::Other);
        assert_eq!(c.classify(""), MeetingAppKind::Other);
    }

    #[test]
    fn classifier_override_overrides_a_default_meeting_app() {
        // The user can re-classify a default Meeting app as Other to
        // ignore it (e.g. Slack on a workspace where they don't take
        // calls and only want manual sessions).
        let c = AppClassifier::with_overrides(vec![("Slack".into(), MeetingAppKind::Other)]);
        assert_eq!(c.classify("Slack"), MeetingAppKind::Other);
    }

    #[test]
    fn classifier_override_classifies_unknown_app() {
        // An app the default table doesn't know about gets the
        // user-supplied kind. The "internal-tool web app is a
        // meeting app" use case from #112.
        let c = AppClassifier::with_overrides(vec![(
            "com.acme.huddle".into(),
            MeetingAppKind::Meeting,
        )]);
        assert_eq!(c.classify("com.acme.huddle"), MeetingAppKind::Meeting);
    }

    #[test]
    fn classifier_override_promotes_a_default_media_app() {
        // YouTube is Media by default; an override flips it to
        // Meeting (e.g. live-streamed conference call).
        let c = AppClassifier::with_overrides(vec![("YouTube".into(), MeetingAppKind::Meeting)]);
        assert_eq!(c.classify("YouTube"), MeetingAppKind::Meeting);
    }

    #[test]
    fn classifier_override_does_not_affect_other_apps() {
        // An override for one app must not leak into the
        // classification of others. Pin so a future bug that uses
        // prefix matching or wildcards fails loud.
        let c = AppClassifier::with_overrides(vec![(
            "com.acme.huddle".into(),
            MeetingAppKind::Meeting,
        )]);
        assert_eq!(c.classify("Spotify"), MeetingAppKind::Media);
        assert_eq!(c.classify("us.zoom.xos"), MeetingAppKind::Meeting);
    }

    #[test]
    fn meeting_source_failed_payload_serializes_with_camel_case_device_lost() {
        // #617: pin the wire shape for the `meeting:source-failed`
        // event. The frontend listener at
        // `src/routes/+page.svelte` reads `deviceLost` (camelCase)
        // off the payload to branch banner copy without
        // substring-matching `reason`. Drift between the Rust
        // field name and the JSON output silently demotes mic
        // disconnects to the generic banner — the lie this PR
        // exists to prevent.
        let payload = super::MeetingSourceFailedPayload {
            session_id: 42,
            source_kind: "mic",
            reason: "audio device disconnected mid-session",
            device_lost: true,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["sessionId"], 42);
        assert_eq!(json["sourceKind"], "mic");
        assert_eq!(json["deviceLost"], true);
        // Reason is still present — keep it for log/debug surfacing
        // even though the frontend now branches on `deviceLost`.
        assert_eq!(json["reason"], "audio device disconnected mid-session");
    }

    #[test]
    fn meeting_source_failed_payload_device_lost_false_for_non_disconnect_failures() {
        // The flag is opt-in true; serializer must round-trip false
        // verbatim (not omit, since the frontend reads it
        // unconditionally).
        let payload = super::MeetingSourceFailedPayload {
            session_id: 7,
            source_kind: "system-audio",
            reason: "transcription task panicked",
            device_lost: false,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["deviceLost"], false);
    }
}
