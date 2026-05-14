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

use crate::audio::{AudioSession, AudioSource};
use crate::transcription::StreamingTranscribeSession;

use super::classifier::AppClassifier;
use super::events::{emit_meeting_session_started, emit_meeting_source_failed};
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
        // The transcribe slot may be empty (no model loaded yet) or
        // may carry a backend that doesn't override `start_stream`
        // — in either case the pump degrades gracefully (sources
        // that fail to open a streaming session are dropped from the
        // pump's per-tick loop and the session row stays open with
        // no utterances, mirroring the pre-#108 "no transcriber"
        // path).
        //
        // We snapshot the transcriber Arc once at start time. If the
        // user hot-swaps models mid-session via the picker, the new
        // model affects the *next* session, not this one — the
        // sliding-window state machine carries inference history
        // that wouldn't transfer cleanly across a model change. A
        // future tightening could re-open streaming sessions on
        // hot-swap; not the day-one shape.
        let transcriber_snapshot = self.transcribe.lock().ok().and_then(|g| g.clone());
        let mut streaming_sessions: Vec<Option<Box<dyn StreamingTranscribeSession>>> =
            Vec::with_capacity(sources.len());
        if let Some(transcriber) = &transcriber_snapshot {
            // Source ordering matches `handles` and `sources`. The
            // pump's per-tick loop iterates by index into all three.
            for (i, source) in sources.iter().enumerate() {
                // Per-handle format read: each AudioSession knows
                // its capture format, but the trait surface today
                // exposes it only through `stop()` / `drain_into()`
                // returns. We pre-warm by issuing a no-op drain
                // into a scratch buffer to learn the format. The
                // drain itself is cheap (lock + mem::take of an
                // empty Vec) and the streaming session needs the
                // format to set up its internal resampler at
                // construction.
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
                        // Surface the failure to the frontend (#533). A
                        // pre-warm failure at startup means this source will
                        // produce 0 utterances for the entire session — not
                        // a transient blip like the mid-session path. Emit
                        // so the panel can show a warning banner rather than
                        // silently logging nothing.
                        emit_meeting_source_failed(
                            self.event_emitter.as_ref(),
                            session.id,
                            // Use speaker_tag() ("mic"/"system") to match the
                            // mid-session pump path and the frontend's "mic"
                            // branch in the MeetingSourceFailed listener (#810).
                            source.speaker_tag(),
                            reason,
                            device_lost,
                        );
                        streaming_sessions.push(None);
                        continue;
                    }
                };
                match transcriber.start_stream(format, "") {
                    Ok(sess) => streaming_sessions.push(Some(sess)),
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            source_kind = source.kind_label(),
                            "meeting pump: start_stream failed; streaming disabled for this source"
                        );
                        // Same surface-to-frontend pattern as the pre-warm
                        // failure arm above (#533): a start_stream failure
                        // means this source will produce 0 utterances.
                        emit_meeting_source_failed(
                            self.event_emitter.as_ref(),
                            session.id,
                            source.speaker_tag(),
                            "streaming session creation failed at session start",
                            false,
                        );
                        streaming_sessions.push(None);
                    }
                }
            }
        } else {
            // No transcriber loaded — streaming sessions stay None
            // for every source. The pump still runs (so cancellation
            // works) but emits no utterances. Same end-state as
            // pre-#108 with no model loaded.
            tracing::warn!(
                session_id = session.id,
                "meeting pump: no transcriber loaded; pump will run idle until model is picked"
            );
            streaming_sessions.resize_with(sources.len(), || None);
        }

        // If all sources failed to open a streaming session despite a
        // transcriber being loaded, the entire session will be silent.
        // Log at error level so it's immediately visible in any log
        // level — warn-only was the original oversight that made #533
        // hard to diagnose (#533 hardening).
        let active_streaming = streaming_sessions.iter().filter(|s| s.is_some()).count();
        if transcriber_snapshot.is_some() && active_streaming == 0 && !sources.is_empty() {
            tracing::error!(
                session_id = session.id,
                sources = sources.len(),
                "meeting pump: ALL streaming sessions failed at startup; \
                 session will produce 0 utterances despite transcriber being loaded"
            );
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
            match std::mem::replace(&mut *guard, SessionState::Idle) {
                SessionState::Active(a) => Some(a),
                state @ (SessionState::Opening | SessionState::Idle) => {
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

        match self.repo.close_session(active.id).await {
            Ok(()) => Ok(()),
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
                        SessionState::Idle => {
                            *guard = SessionState::Active(ActiveSession {
                                id: active.id,
                                started_at: active.started_at,
                                cancel: Arc::new(AtomicBool::new(false)),
                                pump_handle: Mutex::new(None),
                                close_attempted: true,
                            });
                        }
                        SessionState::Opening | SessionState::Active(_) => {
                            tracing::warn!(
                                session_id = active.id,
                                "stop_manual close_session failed but slot was \
                                 claimed by a concurrent start; not clobbering \
                                 the new session — orphan row will be closed by \
                                 next-launch reconcile_orphan_sessions"
                            );
                        }
                    }
                }
                Err(e)
            }
        }
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
        let id = {
            let guard = self
                .state
                .lock()
                .map_err(|_| anyhow!("session manager mutex poisoned"))?;
            match &*guard {
                SessionState::Active(a) => Some(a.id),
                SessionState::Idle | SessionState::Opening => None,
            }
        };

        let id = match id {
            Some(id) => id,
            None => return Ok(false),
        };

        // Cumulative-end-of-last-utterance scheme (the original
        // legacy behavior). The streaming pump uses offsets
        // produced by each session's internal clock; this
        // hotkey-dictation path doesn't have access to a
        // comparable per-session wall-clock so it anchors at the
        // previous utterance's end.
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
