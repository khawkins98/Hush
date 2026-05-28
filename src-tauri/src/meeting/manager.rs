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

use crate::audio::AudioCapture;
use crate::transcription::Utterance;

use super::classifier::AppClassifier;
use super::MeetingSessionRepository;

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
    /// this seam (see [`super::events::MeetingSourceFailedPayload`]).
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
    /// Cross-session speaker identity store (#667). Queried at
    /// session close to link centroids to known identities.
    pub(super) speaker_store: Arc<dyn crate::speakers::SpeakerStore>,
    /// Whether speaker identity resolution is enabled. Shared Arc
    /// from `RuntimeFlags::speaker_identity_enabled`.
    pub(super) speaker_identity_enabled: Arc<std::sync::atomic::AtomicBool>,
    /// Single in-flight background finalization (whisper tail flush +
    /// diarize + speaker-identity + DB close + emit-ended) parked here
    /// by `stop_manual` once the pump confirms audio is released. At
    /// most one because a new meeting `start_manual` awaits this handle
    /// before claiming the slot — a concurrent meeting would otherwise
    /// share the diarizer cluster state and the meeting `WhisperContext`
    /// with the finalizing session. Hence `Option`, not a map.
    /// Concurrent meetings are explicitly deferred — see learnings.md
    /// 2026-05-26 "Deferred: concurrent meetings".
    /// Cleared by the next `start_manual` (await + take) or by `Drop`
    /// (abort-and-reconcile).
    pub(super) finalizing: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    /// Brief foreground window between `stop_manual` signalling cancel
    /// and the pump confirming it has released the audio device. Only
    /// this short interval blocks a concurrent meeting `start_manual`
    /// (the capture singleton isn't free yet); the *slow* tail flush
    /// runs in the background after the slot has already flipped back
    /// to `Idle`. See learnings.md 2026-05-26.
    Releasing,
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
    /// Pump task. On `stop_manual` this handle is *not* joined inline
    /// any more — it is parked in `SessionManager::finalizing` so the
    /// slow tail flush runs in the background while `stop_manual`
    /// returns promptly. Wrapped in `Mutex<Option<...>>` so
    /// `stop_manual` can take it out without the borrow checker
    /// complaining.
    pub(super) pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Resolves when the pump has explicitly released every audio handle
    /// (device freed) and is about to begin the background tail flush.
    /// `stop_manual` awaits this (with a timeout fallback) so it returns
    /// only after the capture singleton is actually free — then flips
    /// `Releasing → Idle`. The matching `oneshot::Sender` lives in the
    /// pump's `PumpContext`. `Mutex<Option<...>>` so `stop_manual` can
    /// `take()` the receiver out (a `oneshot::Receiver` is consumed by
    /// `.await`).
    pub(super) audio_released_rx: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
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

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: Arc<dyn MeetingSessionRepository>,
        audio: Arc<dyn AudioCapture>,
        transcribe: crate::ipc::TranscribeSlot,
        event_emitter: Arc<dyn crate::events::EventEmitter>,
        diarize: Arc<dyn crate::diarization::Diarize>,
        app_overrides: Arc<dyn super::MeetingAppOverrideRepository>,
        mic_gain_db: Arc<AtomicU32>,
        speaker_store: Arc<dyn crate::speakers::SpeakerStore>,
        speaker_identity_enabled: Arc<std::sync::atomic::AtomicBool>,
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
            speaker_store,
            speaker_identity_enabled,
            finalizing: Mutex::new(None),
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
            SessionState::Idle | SessionState::Opening | SessionState::Releasing => None,
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
                // Releasing means the pump was already signalled — its
                // continuation is parked in `finalizing` and aborted below.
                SessionState::Releasing => None,
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

        // Abort any in-flight background finalization (abort-and-reconcile,
        // per the proposal). The tail `finish()` runs in `spawn_blocking`
        // and cannot be cancelled, so we must NOT block shutdown joining it
        // — aborting drops the task at its next await; any session row that
        // didn't get closed is closed by `reconcile_orphan_sessions` on the
        // next launch, the same tail-loss guarantee as a crash.
        if let Ok(mut slot) = self.finalizing.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::audio::AudioSource;
    use crate::db::SqliteDatabase;
    use crate::meeting::events::MeetingSourceFailedPayload;
    use crate::meeting::pump::{diarize_and_dispatch_merged, dispatch_utterances, TickBucket};
    use crate::meeting::test_support::{
        fresh_manager, fresh_manager_no_transcriber, make_final, make_partial, manager_with_repo,
        FailingCloseRepo, RecordingDiarizer,
    };
    use crate::meeting::{MeetingAppKind, SqliteMeetingSessionRepository};

    #[tokio::test]
    async fn start_manual_opens_a_session_and_records_active_id() {
        let mgr = fresh_manager().await;
        assert!(mgr.active_session_id().is_none(), "no session at boot");

        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("us.zoom.xos".into()),
                None,
                Default::default(),
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
        mgr.start_manual(
            vec![AudioSource::default_microphone()],
            None,
            None,
            Default::default(),
        )
        .await
        .unwrap();
        let err = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            .start_manual(Vec::new(), None, None, Default::default())
            .await
            .expect_err("empty source list must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("at least one audio source"),
            "error must name the precondition; got: {msg}"
        );

        // The slot is back to Idle — a valid start now succeeds.
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .expect("post-rollback start must succeed");
        assert_eq!(mgr.active_session_id(), Some(session.id));
        mgr.stop_manual().await.unwrap();
    }

    /// #898: `start_manual` fails fast — before opening any audio handles —
    /// when no transcription model is loaded. The slot is returned to Idle
    /// so a follow-up start (once a model is loaded) succeeds.
    #[tokio::test]
    async fn start_manual_fails_when_no_transcriber_loaded() {
        let mgr = fresh_manager_no_transcriber().await;
        let err = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .expect_err("must error with no transcriber");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no transcription model loaded"),
            "error must name the precondition; got: {msg}"
        );

        // Slot is back to Idle — the user can load a model and retry.
        assert!(
            mgr.active_session_id().is_none(),
            "slot must be Idle after fail-fast so a subsequent start can succeed"
        );
    }

    #[tokio::test]
    async fn stop_manual_closes_the_session_and_clears_active_id() {
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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

    /// #492/#839 adapted for background finalization. The DB
    /// `close_session` now runs in the *background* pump continuation,
    /// not inline in `stop_manual`. So a close failure can no longer
    /// surface a retry to an already-returned Stop:
    ///   - `stop_manual` returns `Ok` once audio is released, regardless
    ///     of whether the eventual background close succeeds.
    ///   - The slot flips back to `Idle` (the device is free).
    ///   - A background close failure is logged and leaves the row open
    ///     for `reconcile_orphan_sessions` to close on next launch.
    #[tokio::test]
    async fn stop_manual_returns_ok_and_close_failure_leaves_row_open_for_reconcile() {
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
                Default::default(),
            )
            .await
            .unwrap();

        // Stop returns Ok even though the background close will fail —
        // close is no longer inline, so its failure can't propagate here.
        mgr.stop_manual()
            .await
            .expect("stop returns Ok once audio is released");

        // Slot flipped back to Idle (audio released) — a new session can
        // be claimed; active_session_id reports nothing in flight.
        assert!(
            mgr.active_session_id().is_none(),
            "slot must be Idle after stop_manual returns"
        );

        // Drain the background finalization so we can observe its effect
        // deterministically (the close will fail by design).
        let finalization = mgr.finalizing.lock().unwrap().take();
        if let Some(handle) = finalization {
            let _ = handle.await;
        }

        // The row is still open: the failing close left ended_at NULL. The
        // boot-time reconcile is what closes it (simulate that pass here to
        // pin the recovery story end-to-end). Read against the inner repo
        // since FailingCloseRepo only overrides close_session.
        let row = inner.get_by_id(session.id).await.unwrap().unwrap();
        assert!(
            row.ended_at.is_none(),
            "background close failure must leave the row open for reconcile"
        );
    }

    /// `stop_manual` returns *before* the slow streaming `finish()`
    /// completes — the whole point of background finalization. With a
    /// `finish()` that blocks until released, the stop must still resolve
    /// promptly (audio released) while the tail flush is still blocked.
    #[tokio::test]
    async fn stop_manual_returns_before_slow_finish_completes() {
        use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

        let release = Arc::new(StdAtomicBool::new(false));
        let started = Arc::new(StdAtomicBool::new(false));
        let mgr = crate::meeting::test_support::manager_with_slow_finish(
            Arc::clone(&release),
            Arc::clone(&started),
        )
        .await;

        mgr.start_manual(
            vec![AudioSource::default_microphone()],
            Some("Zoom".into()),
            None,
            Default::default(),
        )
        .await
        .unwrap();

        // Never release the barrier before stop returns — if stop_manual
        // awaited the full tail flush, this would hang on the bounded spin.
        tokio::time::timeout(std::time::Duration::from_secs(2), mgr.stop_manual())
            .await
            .expect("stop_manual must return well before the blocked finish()")
            .expect("stop returns Ok once audio released");

        // Slot is Idle (audio released) and the finalization is parked +
        // still in flight (finish() is blocked on the barrier).
        assert!(mgr.active_session_id().is_none());

        // Release and drain so the test doesn't leak a blocked task.
        release.store(true, StdOrdering::Release);
        let finalization = mgr.finalizing.lock().unwrap().take();
        if let Some(handle) = finalization {
            let _ = handle.await;
        }
    }

    /// A new *meeting* `start_manual` must await any in-flight background
    /// finalization before claiming the slot — a concurrent meeting would
    /// otherwise share the diarizer cluster state + meeting WhisperContext
    /// with the finalizing one. A slow streaming `finish()` keeps the
    /// finalization in flight so the gate is exercised, not skipped.
    #[tokio::test]
    async fn start_manual_meeting_awaits_in_flight_finalization() {
        use std::sync::atomic::{AtomicBool as StdAtomicBool, Ordering as StdOrdering};

        // Manager whose streaming `finish()` blocks until released, so the
        // background finalization is genuinely in flight after stop_manual.
        let release = Arc::new(StdAtomicBool::new(false));
        let started = Arc::new(StdAtomicBool::new(false));
        let mgr = crate::meeting::test_support::manager_with_slow_finish(
            Arc::clone(&release),
            Arc::clone(&started),
        )
        .await;

        let session_a = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
                Default::default(),
            )
            .await
            .unwrap();

        // Stop A — returns once audio is released; the background finish()
        // is now blocked on the barrier, so finalization is in flight.
        mgr.stop_manual().await.unwrap();

        // Spawn B's start. It must block on the await-finalization gate
        // until we release the barrier. Pin that it does NOT complete
        // while finalization is still in flight.
        let mgr = Arc::new(mgr);
        let mgr_for_b = Arc::clone(&mgr);
        let start_b = tokio::spawn(async move {
            mgr_for_b
                .start_manual(
                    vec![AudioSource::default_microphone()],
                    Some("Teams".into()),
                    None,
                    Default::default(),
                )
                .await
        });

        // Give B a chance to reach (and block on) the gate.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            !start_b.is_finished(),
            "B's start must block until A's finalization completes"
        );

        // Release the barrier so A's finish() returns and finalization
        // completes; B's gate then clears and B succeeds.
        release.store(true, StdOrdering::Release);
        let session_b = start_b
            .await
            .expect("B's start task joined")
            .expect("B start succeeds after A finalizes");
        assert_ne!(
            session_b.id, session_a.id,
            "B is a distinct session that opened after A finalized"
        );

        mgr.stop_manual().await.unwrap();
        // Drain B's finalization so the test doesn't leak a blocked task.
        let finalization = mgr.finalizing.lock().unwrap().take();
        if let Some(handle) = finalization {
            let _ = handle.await;
        }
    }

    #[tokio::test]
    async fn append_if_active_returns_false_when_no_session() {
        let mgr = fresh_manager().await;
        let appended = mgr.append_if_active("hello", 1_000).await.unwrap();
        assert!(!appended, "no session = no append, no error");
    }

    #[tokio::test]
    async fn append_if_active_persists_utterance_with_wall_clock_timestamps() {
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("Zoom".into()),
                None,
                Default::default(),
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

        // Wall-clock arithmetic (#818): timestamps reflect when in the
        // session the utterance ended (started_at.elapsed()) rather than
        // the cumulative sum of prior durations. In a unit test both calls
        // happen within a few ms of session start, so end_ms is small and
        // start_ms is clamped to 0 (duration > elapsed session age).
        //
        // What we assert:
        //   • both utterances were persisted
        //   • start_ms >= 0 (never negative)
        //   • end_ms >= start_ms (non-negative duration)
        //   • ended_at_ms - started_at_ms <= duration_ms (may be clamped when
        //     duration exceeds elapsed session age, as happens in this test)
        //   • second utterance's end_ms >= first utterance's end_ms (monotone)
        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 2);
        assert!(utterances[0].started_at_ms >= 0);
        assert!(utterances[0].ended_at_ms >= utterances[0].started_at_ms);
        assert!(utterances[0].ended_at_ms - utterances[0].started_at_ms <= 2_000);
        assert!(utterances[1].started_at_ms >= 0);
        assert!(utterances[1].ended_at_ms >= utterances[1].started_at_ms);
        assert!(utterances[1].ended_at_ms - utterances[1].started_at_ms <= 3_000);
        assert!(utterances[1].ended_at_ms >= utterances[0].ended_at_ms);
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

    #[tokio::test]
    async fn current_partials_for_returns_empty_for_new_session() {
        // Pin: a session with no in-flight inference yet has no
        // partials. The IPC poll path relies on this to return an
        // empty Vec rather than None / errors.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("revising tail", 1_500, 3_000, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("hello", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;
        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("hello world", 0, 1_500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("you side", 0, 1_000, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;
        dispatch_utterances(
            session.id,
            "system",
            vec![make_partial("remote side", 0, 1_000, "system")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("about to firm up", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;
        assert_eq!(mgr.current_partials_for(session.id).len(), 1);

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("about to firm up", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "system",
            vec![make_partial("remote still talking", 0, 2_000, "system")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;
        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("you finished a sentence", 0, 1_500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_final("   ", 0, 1_000, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        let mut u = make_final("hello", 0, 1_000, "");
        u.speaker_label = None;

        dispatch_utterances(
            session.id,
            "system",
            vec![u],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;

        let utterances = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].speaker_label.as_deref(), Some("system"));
        mgr.stop_manual().await.unwrap();
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            &crate::events::NoopEventEmitter,
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

        diarize_and_dispatch_merged(
            0,
            vec![],
            &diarize,
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;
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
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            &crate::events::NoopEventEmitter,
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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
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
            &crate::events::NoopEventEmitter,
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
    async fn diarize_and_dispatch_merged_skips_partials_for_diarizer() {
        // #800: partials should not be fed to label_utterances. Only
        // finals earn a diarizer inference; partials get the source-
        // derived fall-through label. Mixing partials in bloated
        // cluster history with near-duplicate embeddings and wasted
        // ~50–100 ms per partial inference.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone(), AudioSource::SystemAudio],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
            seen_audio_lens: Mutex::new(Vec::new()),
        });
        let recorder_dyn: Arc<dyn crate::diarization::Diarize> = recorder.clone();

        let mic_bucket = TickBucket {
            source_label: "mic".to_owned(),
            // One final + one partial.
            utterances: vec![
                make_final("hello", 100, 200, "mic"),
                make_partial("partial revision", 300, 400, "mic"),
            ],
            audio: vec![vec![0.0; 100], vec![0.0; 100]],
        };
        let sys_bucket = TickBucket {
            source_label: "system".to_owned(),
            utterances: vec![make_final("world", 150, 250, "system")],
            audio: vec![vec![0.0; 100]],
        };

        diarize_and_dispatch_merged(
            session.id,
            vec![mic_bucket, sys_bucket],
            &recorder_dyn,
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
        )
        .await;

        // Diarizer only saw the two finals' start times, not the partial.
        let seen = recorder.seen_starts.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![100, 150],
            "diarizer must only see finals, not partials; saw {:?}",
            seen
        );

        // Only finals landed in the DB.
        let persisted = mgr.repo.list_utterances(session.id).await.unwrap();
        assert_eq!(persisted.len(), 2, "only 2 finals should be persisted");

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
            .start_manual(
                vec![AudioSource::default_microphone()],
                None,
                None,
                Default::default(),
            )
            .await
            .unwrap();

        dispatch_utterances(
            session.id,
            "mic",
            vec![make_partial("incomplete", 0, 500, "mic")],
            &mgr.partials,
            &mgr.repo,
            &crate::events::NoopEventEmitter,
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
        let payload = MeetingSourceFailedPayload {
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
        let payload = MeetingSourceFailedPayload {
            session_id: 7,
            source_kind: "system-audio",
            reason: "transcription task panicked",
            device_lost: false,
        };
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["deviceLost"], false);
    }
}
