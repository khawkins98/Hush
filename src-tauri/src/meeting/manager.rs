//! Meeting Mode session manager — owns the "is a session active?"
//! state and the policy for opening / closing them.
//!
//! ## Phase C runtime — manual-start MVP
//!
//! This is the **manual-start** slice of [#110]. The user clicks
//! "Start meeting" in the panel; the manager opens a session. They
//! talk; each `stop_dictation` lands a transcript that the IPC layer
//! also appends to the active session as a final utterance (in
//! addition to the existing history insert). The user clicks "Stop
//! meeting"; the manager closes the session.
//!
//! What's deliberately **not** here yet:
//!
//! - **Auto-detect from foreground app.** The classifier enum
//!   ([`AppClassifier`]) is wired up but not yet driving the
//!   session lifecycle. Auto-start-on-Zoom-detection is a
//!   follow-up; manual-start is the safer first step because it
//!   never records a meeting the user didn't intend to record.
//! - **"Start a session?" prompt.** No prompt UX yet — the only
//!   trigger is the panel button.
//! - **Streaming utterances.** Each session captures one final
//!   utterance per `stop_dictation` call, not per VAD-segmented
//!   speech turn. Streaming partials wait on [#108]; the panel
//!   will start showing per-utterance timeline rendering the
//!   moment a streaming backend lands.
//! - **System audio.** Without [#105] / [#106] / [#107] shipped,
//!   meeting mode captures via mic only — a single-speaker
//!   "personal meeting transcript" experience. Useful for note-
//!   taking yourself; a partial experience for capturing the
//!   other side of a Zoom call. The picker now includes a
//!   "System audio" entry but it's disabled until those PRs
//!   land.
//!
//! [#105]: https://github.com/khawkins98/Hush/issues/105
//! [#106]: https://github.com/khawkins98/Hush/issues/106
//! [#107]: https://github.com/khawkins98/Hush/issues/107
//! [#108]: https://github.com/khawkins98/Hush/issues/108
//! [#110]: https://github.com/khawkins98/Hush/issues/110
//!
//! ## Privacy invariant (load-bearing)
//!
//! The manager only ever sees `Utterance`s from the transcription
//! layer — never raw audio. The trait shape here can't be subverted
//! to persist `Vec<f32>` even if a future caller tried. Pinned by
//! the test that asserts the manager's API surface accepts only
//! transcripts + timestamps.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use super::{
    MeetingAppKind, MeetingSession, MeetingSessionRepository, NewMeetingSession,
    NewPersistedUtterance,
};

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
    repo: Arc<dyn MeetingSessionRepository>,
    classifier: AppClassifier,
    /// Active session id, or `None` if no session is in flight.
    /// Mutex (not `RwLock`): the contention surface is one IPC
    /// command per user click — never a hot path. Read by every
    /// `stop_dictation` to decide whether to append; written only
    /// by start_manual / stop_manual.
    active: Mutex<Option<i64>>,
}

impl SessionManager {
    pub fn new(repo: Arc<dyn MeetingSessionRepository>) -> Self {
        Self {
            repo,
            classifier: AppClassifier::default_table(),
            active: Mutex::new(None),
        }
    }

    /// Start a meeting session manually (button-driven).
    ///
    /// `app_name` is what the user wants the session attributed to —
    /// typically the foreground app's bundle id at the moment of click.
    /// If `None`, the manager labels the session as "manual" with
    /// `app_kind = Other`. The session row is opened with
    /// `started_at = NOW`, `ended_at = NULL`.
    ///
    /// Errors if a session is already active — the user must close
    /// the existing one first. Surfaces as `IpcError::MeetingSessions`
    /// at the IPC layer.
    pub async fn start_manual(&self, app_name: Option<String>) -> Result<MeetingSession> {
        // Lock first so a concurrent start can't slip through. The
        // lock is released before the async DB call to avoid holding
        // it across `await` (which would block other start/stop
        // calls in flight, even though there shouldn't be any).
        {
            let guard = self
                .active
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            if guard.is_some() {
                return Err(anyhow!(
                    "meeting session already active; stop the current one first"
                ));
            }
        }

        let app_name = app_name.unwrap_or_else(|| "manual".to_owned());
        let app_kind = self.classifier.classify(&app_name);

        let session = self
            .repo
            .create(NewMeetingSession {
                app_name: app_name.clone(),
                app_kind,
            })
            .await?;

        // Commit the active-session id only after the DB write
        // succeeds — we never want the in-memory state to claim a
        // session that doesn't exist on disk.
        *self
            .active
            .lock()
            .map_err(|_| anyhow!("session manager mutex poisoned"))? = Some(session.id);

        Ok(session)
    }

    /// Close the active session.
    ///
    /// Writes `ended_at = NOW`. No-op-with-error if no session is
    /// active — the panel is expected to disable the Stop button
    /// when nothing's running, but a stale double-click shouldn't
    /// crash anything either.
    pub async fn stop_manual(&self) -> Result<()> {
        let id = {
            let mut guard = self
                .active
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            // Take the id out so a concurrent append_utterance can't
            // race past us writing into a session we're about to
            // close. The dropped-on-error case below restores it.
            guard.take()
        };

        let id = match id {
            Some(id) => id,
            None => return Err(anyhow!("no meeting session active")),
        };

        match self.repo.close_session(id).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Restore the active id so the caller can retry —
                // a transient SQLite failure shouldn't leave the
                // user without a way to close the session.
                if let Ok(mut guard) = self.active.lock() {
                    *guard = Some(id);
                }
                Err(e)
            }
        }
    }

    /// Append a final utterance to the active session, if any.
    ///
    /// Called from the IPC layer's `stop_dictation` handler after
    /// each successful transcription. Returns `Ok(false)` if no
    /// session is active (the common case — no behaviour change
    /// from pre-meeting-mode dictation), `Ok(true)` if a session is
    /// active and the utterance was persisted.
    ///
    /// This is the **only** path utterances enter the data layer
    /// today. Phase B's streaming pump will start calling it
    /// per-final-utterance once a streaming backend lands (#108).
    pub async fn append_if_active(&self, text: &str, duration_ms: i64) -> Result<bool> {
        let id = {
            let guard = self
                .active
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            *guard
        };

        let id = match id {
            Some(id) => id,
            None => return Ok(false),
        };

        // Compute timestamps relative to session start. We don't
        // have the actual recording-start wall-clock here without
        // threading it through stop_dictation, so v1 uses the
        // simplest workable scheme: each utterance covers the
        // duration_ms range [previous_session_end, previous_session_end +
        // duration_ms]. The panel renders these as cumulative
        // offsets which is good enough for "show me what was said
        // and roughly when." A future PR threading the actual
        // start_dictation timestamp through can sharpen this.
        //
        // For the manual-start MVP the simpler approach is to just
        // anchor each utterance at "now relative to session open"
        // which is approximately what we want.
        let utterances = self.repo.list_utterances(id).await?;
        let next_start = utterances.last().map(|u| u.ended_at_ms).unwrap_or(0);

        self.repo
            .append_utterance(NewPersistedUtterance {
                session_id: id,
                started_at_ms: next_start,
                ended_at_ms: next_start + duration_ms,
                speaker_label: None,
                text: text.to_owned(),
            })
            .await?;

        Ok(true)
    }

    /// Read-only snapshot of the active session id, if any. The
    /// frontend polls this on mount + after every state change so
    /// the panel can render "session in progress" affordances.
    pub fn active_session_id(&self) -> Option<i64> {
        self.active.lock().ok().and_then(|guard| *guard)
    }
}

/// Bundle-id → [`MeetingAppKind`] lookup.
///
/// Hardcoded defaults for the apps Hush expects to encounter most
/// frequently. v1 only uses this for the `app_kind` row stamped on
/// new sessions (informational, drives the panel's coloured tag);
/// the actual auto-start-on-meeting policy that this would also
/// drive is deferred (still manual-start-only for the MVP).
///
/// Per-user overrides (Phase E, [#112]) will write entries into the
/// settings table that this struct reads on construction. Today the
/// table is empty; the defaults are the only signal.
///
/// [#112]: https://github.com/khawkins98/Hush/issues/112
pub struct AppClassifier {
    /// Future: replace with `HashMap` once the entry list grows
    /// past ~20. v1 stays linear because the default table is small
    /// and the per-classify cost is irrelevant.
    entries: Vec<(&'static str, MeetingAppKind)>,
}

impl AppClassifier {
    /// Hardcoded defaults. Bundle ids match what
    /// `active-win-pos-rs::get_active_window().app_name` returns
    /// on each platform — process / app names rather than reverse-
    /// DNS bundle ids on Linux/Windows where the latter doesn't
    /// exist.
    pub fn default_table() -> Self {
        Self {
            entries: vec![
                // Meeting / video-call apps. Auto-start (when that
                // policy lands) defaults to "ask" for these.
                ("zoom.us", MeetingAppKind::Meeting),
                ("us.zoom.xos", MeetingAppKind::Meeting),
                ("Microsoft Teams", MeetingAppKind::Meeting),
                ("com.microsoft.teams2", MeetingAppKind::Meeting),
                ("Microsoft Teams (work or school)", MeetingAppKind::Meeting),
                ("Google Meet", MeetingAppKind::Meeting),
                ("Discord", MeetingAppKind::Meeting),
                ("com.hnc.Discord", MeetingAppKind::Meeting),
                ("Slack", MeetingAppKind::Meeting),
                ("com.tinyspeck.slackmacgap", MeetingAppKind::Meeting),
                ("Webex", MeetingAppKind::Meeting),
                // Media apps. Auto-start (when shipped) defaults
                // to "no" for these — most users don't want a
                // YouTube watch-party transcribed by accident.
                ("YouTube", MeetingAppKind::Media),
                ("Spotify", MeetingAppKind::Media),
                ("com.spotify.client", MeetingAppKind::Media),
                ("Apple Music", MeetingAppKind::Media),
                ("Music", MeetingAppKind::Media),
                ("Podcasts", MeetingAppKind::Media),
            ],
        }
    }

    pub fn classify(&self, app_name: &str) -> MeetingAppKind {
        for (key, kind) in &self.entries {
            if *key == app_name {
                return *kind;
            }
        }
        MeetingAppKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SqliteDatabase;
    use crate::meeting::SqliteMeetingSessionRepository;

    async fn fresh_manager() -> SessionManager {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let repo: Arc<dyn MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
        SessionManager::new(repo)
    }

    #[tokio::test]
    async fn start_manual_opens_a_session_and_records_active_id() {
        let mgr = fresh_manager().await;
        assert!(mgr.active_session_id().is_none(), "no session at boot");

        let session = mgr.start_manual(Some("us.zoom.xos".into())).await.unwrap();
        assert_eq!(session.app_name, "us.zoom.xos");
        assert_eq!(session.app_kind, MeetingAppKind::Meeting); // classifier lookup
        assert!(session.ended_at.is_none(), "new session is open");

        assert_eq!(mgr.active_session_id(), Some(session.id));
    }

    #[tokio::test]
    async fn start_manual_rejects_concurrent_starts() {
        let mgr = fresh_manager().await;
        mgr.start_manual(None).await.unwrap();
        let err = mgr
            .start_manual(None)
            .await
            .expect_err("second start must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already active"),
            "error must name the precondition; got: {msg}"
        );
    }

    #[tokio::test]
    async fn stop_manual_closes_the_session_and_clears_active_id() {
        let mgr = fresh_manager().await;
        let session = mgr.start_manual(None).await.unwrap();

        mgr.stop_manual().await.unwrap();
        assert!(mgr.active_session_id().is_none(), "active cleared on stop");

        // No regression test that ended_at is non-null because the
        // SqliteMeetingSessionRepository::close_session test pins
        // that already; the manager's job is just to call it.
        let _ = session;
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

    #[tokio::test]
    async fn append_if_active_returns_false_when_no_session() {
        let mgr = fresh_manager().await;
        let appended = mgr.append_if_active("hello", 1_000).await.unwrap();
        assert!(!appended, "no session = no append, no error");
    }

    #[tokio::test]
    async fn append_if_active_persists_utterance_with_cumulative_timestamps() {
        let mgr = fresh_manager().await;
        let session = mgr.start_manual(Some("Zoom".into())).await.unwrap();

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
    }

    #[test]
    fn classifier_recognises_default_meeting_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("us.zoom.xos"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Microsoft Teams"), MeetingAppKind::Meeting);
        assert_eq!(c.classify("Discord"), MeetingAppKind::Meeting);
    }

    #[test]
    fn classifier_recognises_default_media_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("Spotify"), MeetingAppKind::Media);
        assert_eq!(c.classify("YouTube"), MeetingAppKind::Media);
    }

    #[test]
    fn classifier_returns_other_for_unknown_apps() {
        let c = AppClassifier::default_table();
        assert_eq!(c.classify("RandomEditor.app"), MeetingAppKind::Other);
        assert_eq!(c.classify(""), MeetingAppKind::Other);
    }
}
