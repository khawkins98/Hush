//! [`SessionManager`] lifecycle methods (#488).
//!
//! Lifted out of [`super::manager`] under #488 — the audit's primary
//! recommendation for #431 item 2. The state machine + struct
//! definitions stay in `manager.rs`; the methods that *drive* the
//! state machine (`start_manual`, `stop_manual`, `append_if_active`)
//! live here so each file has one job.
//!
//! ## Why fields are `pub(super)` not private
//!
//! Splitting the impl across two files means the lifecycle code
//! needs read+write access to every field of [`SessionManager`],
//! [`super::manager::ActiveSession`], and
//! [`super::manager::SessionState`]. Every relevant field gained
//! `pub(super)` visibility (visible to all of `crate::meeting::*`,
//! invisible outside). This widens the internal API surface but
//! doesn't expose anything to consumers of the meeting module.
//!
//! ## Locking discipline
//!
//! See `learnings.md` 2026-05-02. The state mutex is *never* held
//! across an `.await` — every method that touches it does so in a
//! scoped block, drops the guard, then performs async work, then
//! re-acquires for the commit step. The Idle → Opening → Active
//! sentinel flip is what makes concurrent `start_manual` calls
//! race-safe; a careless rewrite that holds the lock through the DB
//! `create()` call would deadlock against any concurrent
//! `stop_manual`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Result};
use zeroize::Zeroize;

use crate::audio::{AudioSession, AudioSource};
use crate::transcription::StreamingTranscribeSession;

use super::classifier::AppClassifier;
use super::events::{
    emit_meeting_session_ended, emit_meeting_session_started, emit_meeting_source_failed,
};
use super::manager::{ActiveSession, SessionManager, SessionState};
use super::pump;
use super::{MeetingSession, NewMeetingSession, NewPersistedUtterance};

impl SessionManager {
    /// Start a meeting session manually (button-driven).
    ///
    /// `sources` is the list of audio sources the pump should
    /// capture from in parallel. The default in production is
    /// `[selected_source]` until Phase 3 of #122 promotes mic + SCK
    /// as the meeting default; passing multiple sources today
    /// already works because [`crate::audio::AudioCapture::start_session`]
    /// supports parallel handles (#124).
    ///
    /// `app_name` is what the user wants the session attributed to —
    /// typically the foreground app's bundle id at the moment of click.
    /// If `None`, the manager labels the session as "manual" with
    /// `app_kind = Other`. The session row is opened with
    /// `started_at = NOW`, `ended_at = NULL`.
    ///
    /// On success: opens the session row, starts an
    /// [`AudioSession`] handle per source, spawns the chunking pump
    /// task. Each chunk is transcribed and appended as an
    /// [`super::PersistedUtterance`] under the active session.
    ///
    /// Errors if a session is already active — the user must close
    /// the existing one first. Surfaces as `IpcError::MeetingSessions`
    /// at the IPC layer.
    pub async fn start_manual(
        &self,
        sources: Vec<AudioSource>,
        app_name: Option<String>,
        app_title: Option<String>,
    ) -> Result<MeetingSession> {
        // Claim the slot via the Opening sentinel. A concurrent
        // start sees Opening and rejects rather than racing past
        // the precondition check. The lock is released before the
        // async DB / handle work — held across an .await would
        // block all other manager methods, including stop_manual,
        // for the duration of the open.
        {
            let mut guard = self
                .state
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            match *guard {
                SessionState::Idle => {
                    *guard = SessionState::Opening;
                }
                SessionState::Opening => {
                    return Err(anyhow!(
                        "another start is already in flight; wait for it to finish"
                    ));
                }
                SessionState::Active(_) => {
                    return Err(anyhow!(
                        "meeting session already active; stop the current one first"
                    ));
                }
                SessionState::Stopping => {
                    return Err(anyhow!(
                        "a meeting session is finishing; wait before starting a new one"
                    ));
                }
            }
        }

        // Anything below this line that returns Err MUST first
        // revert the slot to Idle and roll back any opened audio
        // handles. The `revert_to_idle` closure centralises the
        // recovery so each early-return arm is a single call.
        let revert_to_idle = |handles: Vec<Box<dyn AudioSession>>| -> Result<()> {
            for opened in handles {
                if let Err(roll_err) = opened.stop() {
                    tracing::warn!(
                        error = ?roll_err,
                        "rollback: stop of already-opened audio session failed"
                    );
                }
            }
            let mut guard = self
                .state
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            *guard = SessionState::Idle;
            Ok(())
        };

        if sources.is_empty() {
            let _ = revert_to_idle(Vec::new());
            return Err(anyhow!("meeting session needs at least one audio source"));
        }

        // Snapshot the transcriber Arc once at start time. We take the
        // snapshot here (before opening audio handles) so we can fail fast
        // when no model is loaded — opening mic/screen handles just to
        // discard them is wasteful and confusing (#898). If the user
        // hot-swaps models mid-session, the new model affects the *next*
        // session, not this one — the sliding-window state machine carries
        // inference history that wouldn't transfer cleanly across a model
        // change.
        let transcriber_snapshot = self.transcribe.lock().ok().and_then(|g| g.clone());
        if transcriber_snapshot.is_none() {
            let _ = revert_to_idle(Vec::new());
            return Err(anyhow!(
                "no transcription model loaded; load a model before starting a meeting session"
            ));
        }

        // Open all the capture handles BEFORE the DB write. If any
        // source fails (Screen Recording permission denied, mic
        // already in use), we want to fail loud now rather than
        // create an empty session row the user has to clean up.
        let mut handles: Vec<Box<dyn AudioSession>> = Vec::with_capacity(sources.len());
        for source in &sources {
            match self.audio.start_session(source.clone()) {
                Ok(h) => handles.push(h),
                Err(e) => {
                    let kind = source.kind_label();
                    let _ = revert_to_idle(handles);
                    return Err(e.context(format!("open audio session for {kind} source")));
                }
            }
        }

        let app_name = app_name.unwrap_or_else(|| "manual".to_owned());
        // Load a fresh override snapshot at every session start (#112).
        // The Settings panel writes here without notifying the manager,
        // so reading per-session is the simplest invalidation strategy
        // — the cost is one indexed lookup against a tiny table.
        // Failures degrade to "no overrides" so a corrupt or
        // unreachable database can't block session creation.
        let overrides = match self.app_overrides.list().await {
            Ok(rows) => rows
                .into_iter()
                .map(|r| (r.app_name, r.kind))
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "meeting: failed to load app overrides; falling back to defaults"
                );
                Vec::new()
            }
        };
        let classifier = if overrides.is_empty() {
            // Tiny fast-path: when there are no overrides, reuse the
            // cached defaults instead of allocating a fresh classifier
            // every time. Skips one Vec clone per session start.
            None
        } else {
            Some(AppClassifier::with_overrides(overrides))
        };
        let app_kind = classifier
            .as_ref()
            .unwrap_or(&self.classifier)
            .classify(&app_name);

        // Snapshot the source-kind tags for persistence (#242).
        // The panel reads these back to render "Mic + System audio"
        // metadata even when the app classification is "Other"
        // (browser tab, generic productivity app). Stored as a
        // separate Vec rather than shadowing `sources` because the
        // streaming-session loop below still iterates the original
        // `Vec<AudioSource>`.
        //
        // Uses `speaker_tag()` (the persistence-layer short form)
        // not `kind_label()` (the structured-logging long form) so
        // the CSV in `meeting_sessions.sources` agrees with the
        // per-utterance `speaker_label` set in the dispatch loop —
        // see `AudioSource::speaker_tag` for the invariant.
        let source_labels: Vec<String> = sources
            .iter()
            .map(|src| src.speaker_tag().to_owned())
            .collect();
        let session = match self
            .repo
            .create(NewMeetingSession {
                app_name: app_name.clone(),
                app_kind,
                sources: source_labels,
                app_title: app_title.clone(),
            })
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let _ = revert_to_idle(handles);
                return Err(e);
            }
        };

        // Open one streaming inference session per audio source.
        // The transcriber was snapshotted (and null-checked) above
        // before opening audio handles — we know it's Some here.
        // Sources that fail `start_stream` are excluded from the pump's
        // per-tick loop; if ALL fail we abort below (#898).
        let mut streaming_sessions: Vec<Option<Box<dyn StreamingTranscribeSession>>> =
            Vec::with_capacity(sources.len());
        // Collect source-failure events to emit AFTER session-started (#881).
        // The frontend ignores source-failed events that arrive before
        // session-started (activeId is null until that event fires), so we
        // defer them here and drain the vec after emit_meeting_session_started.
        let mut deferred_source_failures: Vec<(String, String, bool)> =
            Vec::with_capacity(sources.len());
        // transcriber_snapshot is always Some at this point — the None case
        // was caught and returned early above. Unwrap is safe.
        let transcriber = transcriber_snapshot.as_ref().unwrap();
        // Source ordering matches `handles` and `sources`. The
        // pump's per-tick loop iterates by index into all three.
        for (i, source) in sources.iter().enumerate() {
            // Per-handle format read: each AudioSession knows
            // its capture format, but the trait surface today
            // exposes it only through `stop()` / `drain_into()`
            // returns. We pre-warm by issuing a drain into a
            // scratch buffer to discover the format and capture
            // any audio that accumulated between handle-open and
            // stream-start (#868). The streaming session replays
            // this buffer so the first inference window is not cold.
            //
            // If the pre-warm fails (ScreenCaptureKit denied
            // mid-start, mic device vanished), we skip opening
            // a streaming session for that source — the audio
            // handle is still valid for the legacy `stop()`
            // path, but the streaming pump won't process its
            // samples. Logged loudly so the user sees the
            // diagnostic in the panel.
            let mut scratch = Vec::new();
            let format = match handles[i].drain_into(&mut scratch) {
                Ok(f) => f,
                Err(e) => {
                    scratch.zeroize();
                    tracing::warn!(
                        error = ?e,
                        source_kind = source.kind_label(),
                        "meeting pump: drain_into pre-warm failed; streaming disabled for this source"
                    );
                    // Downcast for DeviceLost so the frontend can
                    // distinguish "device vanished between picker
                    // and start" from generic capture failures
                    // (#617). The mid-session pump path already
                    // does this; the pre-warm path was the
                    // asymmetric arm.
                    let device_lost = e.downcast_ref::<crate::audio::DeviceLost>().is_some();
                    let reason = if device_lost {
                        "audio device disconnected before session start"
                    } else {
                        "audio capture pre-warm failed at session start"
                    };
                    // Surface the failure to the frontend (#533, #881). Deferred
                    // until after session-started so the frontend's activeId is
                    // set before the event arrives; it would silently drop
                    // source-failed events received while activeId is null.
                    deferred_source_failures.push((
                        // Use speaker_tag() ("mic"/"system") to match the
                        // mid-session pump path and the frontend's "mic"
                        // branch in the MeetingSourceFailed listener (#810).
                        source.speaker_tag().to_owned(),
                        reason.to_owned(),
                        device_lost,
                    ));
                    streaming_sessions.push(None);
                    continue;
                }
            };
            match transcriber.start_stream(format, "") {
                Ok(mut sess) => {
                    // Replay pre-warm audio into the streaming session before
                    // zeroizing the buffer (#868). Without this, audio captured
                    // between handle-open and stream-start (the caller's first
                    // words before the first pump tick) is silently dropped.
                    // If feed fails, treat it as a stream-setup failure for
                    // this source rather than pushing a broken session.
                    if !scratch.is_empty() {
                        if let Err(e) = sess.feed(&scratch) {
                            tracing::warn!(
                                error = ?e,
                                source_kind = source.kind_label(),
                                "meeting pump: pre-warm replay failed; streaming disabled for this source"
                            );
                            scratch.zeroize(); // (#930) clear PCM from allocator memory
                            deferred_source_failures.push((
                                source.speaker_tag().to_owned(),
                                "pre-warm replay failed at session start".to_owned(),
                                false,
                            ));
                            streaming_sessions.push(None);
                            continue;
                        }
                    }
                    scratch.zeroize(); // (#930) clear PCM from allocator memory after feeding
                    streaming_sessions.push(Some(sess));
                }
                Err(e) => {
                    scratch.zeroize(); // (#930) clear PCM from allocator memory
                    tracing::warn!(
                        error = ?e,
                        source_kind = source.kind_label(),
                        "meeting pump: start_stream failed; streaming disabled for this source"
                    );
                    // Same surface-to-frontend pattern as the pre-warm
                    // failure arm above (#533, #881): a start_stream failure
                    // means this source will produce 0 utterances. Deferred
                    // until after session-started (see comment above).
                    deferred_source_failures.push((
                        source.speaker_tag().to_owned(),
                        "streaming session creation failed at session start".to_owned(),
                        false,
                    ));
                    streaming_sessions.push(None);
                }
            }
        }

        // If all sources failed to open a streaming session, abort
        // rather than starting a silent session that produces 0
        // utterances despite the user expecting transcription (#898).
        // Stop audio handles immediately, close the DB row (best-effort),
        // then surface an error through the IPC layer.
        let active_streaming = streaming_sessions.iter().filter(|s| s.is_some()).count();
        if active_streaming == 0 {
            tracing::error!(
                session_id = session.id,
                sources = sources.len(),
                "meeting pump: ALL streaming sessions failed at startup; \
                 aborting so the user sees an error rather than a silent recording"
            );
            // Stop audio capture before the async DB close so no PCM
            // is captured during the cleanup window.
            let _ = revert_to_idle(handles);
            if let Err(e) = self.repo.close_session(session.id).await {
                tracing::warn!(
                    error = ?e,
                    session_id = session.id,
                    "rollback: close_session failed after all-streams-fail; session row may be orphaned"
                );
            }
            return Err(anyhow!(
                "all audio sources failed to open transcription streams; \
                 check microphone and screen-recording permissions"
            ));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        // `started_at` here populates `ActiveSession.started_at`
        // (used by the pump to anchor utterance offsets and
        // prevent drift across out-of-order chunk completions).
        // This is *not* the same field that #253 removed from
        // `PumpContext` — that one was unused; this one is
        // load-bearing.
        let started_at = Instant::now();
        let pump_handle = tokio::spawn(pump::run_pump(pump::PumpContext {
            session_id: session.id,
            repo: Arc::clone(&self.repo),
            sources: sources.clone(),
            // Wrap each handle in Some so the pump can `.take()` it
            // before swapping on device-loss without losing the ability
            // to detect that a slot is occupied (#611).
            handles: handles.into_iter().map(Some).collect(),
            streaming_sessions,
            partials: Arc::clone(&self.partials),
            cancel: Arc::clone(&cancel),
            event_emitter: Arc::clone(&self.event_emitter),
            diarize: Arc::clone(&self.diarize),
            mic_gain_db: Arc::clone(&self.mic_gain_db),
            audio: Arc::clone(&self.audio),
            transcribe: transcriber_snapshot,
            session_start: started_at,
        }));

        // Commit Active. The slot has been Opening since the start
        // of this method, so no concurrent start_manual can have
        // raced through — the swap below is unconditional.
        let mut guard = self
            .state
            .lock()
            .map_err(|_| anyhow!("session manager mutex poisoned"))?;
        *guard = SessionState::Active(ActiveSession {
            id: session.id,
            started_at,
            cancel,
            pump_handle: Mutex::new(Some(pump_handle)),
            close_attempted: false,
        });
        drop(guard);

        // Notify the frontend so it immediately syncs its session state
        // (shows the Stop button, starts the live-transcript poll, etc.).
        // Emitted after the Active commit so that `meeting_active_session`
        // IPC always finds the session if the frontend calls it in response.
        // Covers both the manual button path (IPC command) and the HAL
        // auto-start path — the frontend listener only needs to call
        // `meeting.refresh()` once regardless of which path fired.
        emit_meeting_session_started(self.event_emitter.as_ref(), session.id);

        // Emit deferred source-failure events now that the frontend has a
        // non-null activeId — events emitted before session-started are
        // silently dropped by the listener (#881).
        for (source_kind, reason, device_lost) in &deferred_source_failures {
            emit_meeting_source_failed(
                self.event_emitter.as_ref(),
                session.id,
                source_kind,
                reason,
                *device_lost,
            );
        }

        Ok(session)
    }

    /// Returns `true` when a session is in the `Active` state — i.e. a
    /// pump is running or was running and needs a DB-close retry. Returns
    /// `false` for `Idle`, `Opening`, or a poisoned state mutex.
    ///
    /// Called by [`crate::ipc::commands::meeting::meeting_stop_manual`] to
    /// decide whether the WhisperContext + ORT Session cleanup should run:
    /// the cleanup must fire even when `stop_manual` returns a DB-close
    /// error (pump already joined), but must NOT fire for the "no meeting
    /// session active" early-return case (pump was never involved, so the
    /// transcribe slots are still in use for dictation).
    pub fn has_active_session(&self) -> bool {
        self.state
            .lock()
            .map(|guard| matches!(&*guard, SessionState::Active(_)))
            .unwrap_or(false)
    }

    /// Close the active session.
    ///
    /// Signals the pump to cancel, awaits its completion (the pump
    /// drains + transcribes one final chunk before exiting), then
    /// writes `ended_at = NOW` on the session row. No-op-with-error
    /// if no session is active — the panel disables the Stop button
    /// when nothing's running, but a stale double-click shouldn't
    /// crash anything either.
    pub async fn stop_manual(&self) -> Result<()> {
        // Take the active record out so a concurrent append_utterance
        // can't race past us writing into a session we're about to
        // close. The dropped-on-error case below restores it.
        let active = {
            let mut guard = self
                .state
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            match std::mem::replace(&mut *guard, SessionState::Stopping) {
                SessionState::Active(a) => Some(a),
                state @ (SessionState::Opening | SessionState::Idle | SessionState::Stopping) => {
                    // Restore the original state — we didn't have an
                    // Active to take.
                    *guard = state;
                    None
                }
            }
        };

        let active = match active {
            Some(a) => a,
            None => return Err(anyhow!("no meeting session active")),
        };

        // First-try path: signal the pump and join it. Subsequent
        // retries (`close_attempted == true`) skip this — the pump
        // is already gone, having drained on the original call —
        // and go straight to retrying the DB close (#249).
        if !active.close_attempted {
            // Tell the pump to wind down, then wait for it to drain
            // its final chunk + append the resulting utterance.
            // Awaiting the join here matters: if we close the
            // session row before the pump's last append, the panel
            // briefly shows "ended" with a missing
            // tail-of-conversation utterance.
            active.cancel.store(true, Ordering::Release);
            let pump_handle = active
                .pump_handle
                .lock()
                .map_err(|_| anyhow!("active session pump_handle mutex poisoned"))?
                .take();
            if let Some(handle) = pump_handle {
                // Best-effort: a panicked pump task shouldn't block
                // session cleanup. Log and continue.
                if let Err(e) = handle.await {
                    tracing::error!(error = ?e, "meeting pump task panicked or was cancelled");
                }
            }

            // The pump's finish() path already flushed any tail
            // finals to the database and cleared the per-source
            // partials. Belt-and-braces: clear our partials map
            // for this session id so a stale partial can't leak
            // into a subsequent IPC poll between this point and
            // the pump's last write.
            if let Ok(mut guard) = self.partials.write() {
                guard.remove(&active.id);
            }
        } else {
            tracing::info!(
                session_id = active.id,
                "meeting stop: retrying close_session after prior DB failure"
            );
        }

        let session_id = active.id;
        let close_result = match self.repo.close_session(active.id).await {
            Ok(()) => {
                // Transition Stopping → Idle now that the pump has joined
                // and the DB row is committed (#839).
                if let Ok(mut guard) = self.state.lock() {
                    if matches!(&*guard, SessionState::Stopping) {
                        *guard = SessionState::Idle;
                    }
                }
                Ok(())
            }
            Err(e) => {
                // Restore the active record with `close_attempted`
                // set so a retry skips the (already-completed)
                // pump cancellation work and goes straight to
                // re-attempting the DB write. The fresh AtomicBool
                // and empty pump_handle reflect that reality —
                // the original cancel/handle have already done
                // their job and aren't reusable.
                //
                // **Race-aware restore (#492).** While we awaited
                // `close_session`, a concurrent `start_manual` may
                // have claimed the slot (Idle → Opening → Active for
                // a new session). The pre-#492 code unconditionally
                // wrote `Active(<old id>)` here, silently clobbering
                // the new session — orphaning its pump task and
                // leaving its DB row stuck open. Now we only restore
                // when the slot is still Idle; if it's been claimed,
                // we log + drop the recovery and surface the close
                // error to the user. The orphan row from the failed
                // close gets cleaned up by `reconcile_orphan_sessions`
                // on next launch (#249).
                if let Ok(mut guard) = self.state.lock() {
                    match &*guard {
                        SessionState::Stopping => {
                            // We own the Stopping slot — restore to Active so
                            // the caller can retry the DB close (#839).
                            *guard = SessionState::Active(ActiveSession {
                                id: active.id,
                                started_at: active.started_at,
                                cancel: Arc::new(AtomicBool::new(false)),
                                pump_handle: Mutex::new(None),
                                close_attempted: true,
                            });
                        }
                        SessionState::Idle | SessionState::Opening | SessionState::Active(_) => {
                            // Should not happen — start_manual blocks on
                            // Stopping, so no concurrent start can have
                            // claimed the slot. Log and drop the recovery;
                            // the orphan row is handled by
                            // reconcile_orphan_sessions on next launch.
                            tracing::warn!(
                                session_id = active.id,
                                "stop_manual close_session failed and slot is \
                                 unexpectedly not Stopping — orphan row will be \
                                 closed by next-launch reconcile_orphan_sessions"
                            );
                        }
                    }
                }
                Err(e)
            }
        };
        // Notify the frontend only after close_session has been attempted
        // so a `meeting_session_get` or `meeting_sessions_list` called
        // immediately from the frontend event handler sees `ended_at` already
        // set in the database (#809). Emitted on both success and failure
        // paths: the pump is already gone either way, and the UI should clear
        // regardless of whether the DB write succeeded (a failed close will be
        // retried; the orphan row gets reconciled at next launch if needed).
        emit_meeting_session_ended(self.event_emitter.as_ref(), session_id);
        close_result
    }

    /// Append a final utterance to the active session, if any.
    ///
    /// Legacy path retained for the dictation hot path: when the
    /// user holds the dictation hotkey *while* a meeting session is
    /// active, the resulting transcript is also recorded as an
    /// utterance under that session. The pump captures continuous
    /// audio independently; the hotkey-driven dictation is a
    /// separate utterance the user explicitly chose to capture.
    ///
    /// Returns `Ok(false)` if no session is active, `Ok(true)` if
    /// the utterance was persisted.
    pub async fn append_if_active(&self, text: &str, duration_ms: i64) -> Result<bool> {
        let id_and_start = {
            let guard = self
                .state
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            match &*guard {
                SessionState::Active(a) => Some((a.id, a.started_at)),
                SessionState::Idle | SessionState::Opening | SessionState::Stopping => None,
            }
        };

        let (id, session_started_at) = match id_and_start {
            Some(pair) => pair,
            None => return Ok(false),
        };

        // Anchor the hotkey-dictation utterance at the wall-clock position
        // within this session rather than at "end of last DB utterance".
        //
        // The pump timestamps utterances relative to the same `started_at`
        // Instant. Using elapsed() here places hotkey-dictation in the same
        // timeline without a DB list read, eliminating the read-then-write
        // race where a pump append between the list query and the insert
        // would produce an overlapping or out-of-order timestamp (#818).
        //
        // `start_ms` is clamped to 0 in the pathological case where
        // `duration_ms` exceeds the session age (e.g. dictation started
        // before the meeting session opened).
        let end_ms = session_started_at.elapsed().as_millis() as i64;
        let start_ms = end_ms.saturating_sub(duration_ms).max(0);

        match self
            .repo
            .append_utterance(NewPersistedUtterance {
                session_id: id,
                started_at_ms: start_ms,
                ended_at_ms: end_ms,
                speaker_label: None,
                text: text.to_owned(),
            })
            .await?
        {
            Some(_) => Ok(true),
            // Session was closed between the in-memory check above and the DB
            // insert — stop_manual won the race (#917). Treat as "no active
            // session" so the caller doesn't surface a false error.
            None => {
                tracing::debug!(
                    session_id = id,
                    "append_if_active: session closed before insert"
                );
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use crate::db::SqliteDatabase;
    use crate::meeting::manager::{ActiveSession, SessionState};
    use crate::meeting::SqliteMeetingSessionRepository;

    async fn idle_manager() -> crate::meeting::SessionManager {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        let repo: Arc<dyn crate::meeting::MeetingSessionRepository> =
            Arc::new(SqliteMeetingSessionRepository::new(Arc::new(db)));
        crate::meeting::SessionManager::new_for_test(repo)
    }

    #[tokio::test]
    async fn has_active_session_false_when_idle() {
        assert!(!idle_manager().await.has_active_session());
    }

    #[tokio::test]
    async fn has_active_session_true_when_active() {
        let manager = idle_manager().await;
        {
            let mut guard = manager.state.lock().unwrap();
            *guard = SessionState::Active(ActiveSession {
                id: 1,
                started_at: std::time::Instant::now(),
                cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                pump_handle: Mutex::new(None),
                close_attempted: false,
            });
        }
        assert!(manager.has_active_session());
    }

    #[tokio::test]
    async fn has_active_session_false_when_opening() {
        let manager = idle_manager().await;
        {
            let mut guard = manager.state.lock().unwrap();
            *guard = SessionState::Opening;
        }
        assert!(!manager.has_active_session());
    }
}
