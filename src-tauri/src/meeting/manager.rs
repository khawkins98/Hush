//! Meeting Mode session manager — owns the "is a session active?"
//! state, the chunking pump that drives auto-recording, and the
//! policy for opening / closing sessions.
//!
//! ## Lifecycle
//!
//! Manual-start: the user clicks "Start a session" in the panel.
//! The manager opens audio capture handles for each chosen source
//! (mic + optional system audio), creates the session row, and
//! spawns a pump task. The pump drains every handle on a
//! `CHUNK_DURATION` cadence, transcribes each chunk via whisper,
//! and appends utterances tagged with the originating source.
//! When the user clicks Stop, `stop_manual` cancels the pump,
//! awaits its final-chunk drain, and writes `ended_at` on the
//! session row.
//!
//! Auto-detect from foreground app is the next phase ([#112]) —
//! the [`AppClassifier`] table is wired up but not yet driving
//! the start lifecycle.
//!
//! ## Speaker labels
//!
//! Each persisted utterance carries a `speaker_label` derived
//! from its capture source: `"mic"` (you) or `"system"` (remote
//! participants on a typical Zoom / Meet call). Real per-speaker
//! diarization is upstream of this module ([#111]); when it
//! ships, the pump will pass through the model's speaker id
//! instead of the source-derived hint.
//!
//! ## Streaming
//!
//! The pump uses one-shot whisper inference per chunk. Streaming
//! whisper ([#108]) replaces the chunk-and-restart cycle with a
//! continuous capture whose utterances arrive as they finalise;
//! the panel just polls the same `meeting_session_get` IPC and
//! sees them sooner.
//!
//! [#108]: https://github.com/khawkins98/Hush/issues/108
//! [#111]: https://github.com/khawkins98/Hush/issues/111
//! [#112]: https://github.com/khawkins98/Hush/issues/112
//!
//! ## Privacy invariant (load-bearing)
//!
//! The manager only ever sees `Utterance`s from the transcription
//! layer — never raw audio bytes that survive the transcribe call.
//! `CapturedAudio.samples` is owned by the transcription closure
//! and dropped when it returns; the persistence layer only sees
//! text + timestamps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use crate::audio::{AudioCapture, AudioSession, AudioSource, CapturedAudio};
#[cfg(test)]
use crate::transcription::Transcribe;

use super::{
    MeetingAppKind, MeetingSession, MeetingSessionRepository, NewMeetingSession,
    NewPersistedUtterance,
};

/// Pump cadence: each chunk of captured audio runs ~this long before
/// the pump stops + restarts the underlying [`AudioSession`]s and
/// hands the drained samples to whisper.
///
/// 10 seconds is a deliberate trade-off pre-#108 (one-shot whisper
/// inference). Whisper's transcription cost is roughly real-time on
/// Apple Silicon with the `base` model, so a 10 s chunk takes ~10 s
/// to transcribe. Smaller chunks raise overhead (whisper has a
/// fixed ~1 s setup cost per call) and clip more words at chunk
/// boundaries; larger chunks delay the moment utterances appear in
/// the panel. The streaming-Whisper backend (#108) replaces the
/// chunk-and-restart cycle with a single long-running session whose
/// utterances arrive continuously, so this constant is only
/// load-bearing pre-#108.
const CHUNK_DURATION: Duration = Duration::from_secs(10);

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
    /// Audio backend the pump uses to open per-source capture
    /// sessions. Cloned from `AppState::audio` at construction.
    audio: Arc<dyn AudioCapture>,
    /// Live transcribe handle. Same `Arc<Mutex<...>>` `AppState`
    /// holds so model hot-swap reaches in-flight pumps on the
    /// next chunk automatically.
    transcribe: crate::ipc::TranscribeSlot,
    /// Session state, see [`SessionState`]. The `Opening` sentinel
    /// is what makes concurrent `start_manual` calls safe: the
    /// first call flips Idle → Opening under the lock, drops the
    /// lock for the async DB / handle work, and only then commits
    /// to Active. A second concurrent call sees `Opening` and
    /// rejects, instead of slipping past the precondition check
    /// and creating an orphan session.
    state: Mutex<SessionState>,
}

/// Lifecycle state for the manager's session slot. Three-valued
/// rather than `Option<ActiveSession>` because the start path needs
/// an intermediate "I have claimed the slot, but the DB row /
/// capture handles aren't open yet" state. Without it, two
/// concurrent `start_manual` IPC calls could both observe `None`
/// before either commits, and end up creating two database rows /
/// pump tasks for what the user expects to be one session.
enum SessionState {
    Idle,
    Opening,
    Active(ActiveSession),
}

/// In-memory state for an open meeting session. Held inside the
/// manager's `active` mutex; `None` means no session in flight.
struct ActiveSession {
    id: i64,
    /// Wall-clock start. Used by the pump to compute per-utterance
    /// `started_at_ms` / `ended_at_ms` offsets that don't drift
    /// across out-of-order chunk completions (chunk N+1 transcribes
    /// faster than chunk N).
    started_at: Instant,
    /// Cancellation flag the pump task polls between sleeps. Set on
    /// `stop_manual`; the pump completes its in-flight chunk, drains
    /// + transcribes one final time, then exits.
    cancel: Arc<AtomicBool>,
    /// Pump task. Joined on `stop_manual` so the final chunk's
    /// transcription + append are observed before the session row
    /// is closed. Wrapped in `Mutex<Option<...>>` so `stop_manual`
    /// can take it out without the borrow checker complaining.
    pump_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SessionManager {
    pub fn new(
        repo: Arc<dyn MeetingSessionRepository>,
        audio: Arc<dyn AudioCapture>,
        transcribe: crate::ipc::TranscribeSlot,
    ) -> Self {
        Self {
            repo,
            classifier: AppClassifier::default_table(),
            audio,
            transcribe,
            state: Mutex::new(SessionState::Idle),
        }
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
        Self::new(repo, audio, transcribe)
    }

    /// Start a meeting session manually (button-driven).
    ///
    /// `sources` is the list of audio sources the pump should
    /// capture from in parallel. The default in production is
    /// `[selected_source]` until Phase 3 of #122 promotes mic + SCK
    /// as the meeting default; passing multiple sources today
    /// already works because [`AudioCapture::start_session`] supports
    /// parallel handles (#124).
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
        let app_kind = self.classifier.classify(&app_name);

        let session = match self
            .repo
            .create(NewMeetingSession {
                app_name: app_name.clone(),
                app_kind,
            })
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let _ = revert_to_idle(handles);
                return Err(e);
            }
        };

        // Spawn the pump on the current tokio runtime. Captures
        // are already in flight via `handles`; the pump's first
        // chunk drains them after `CHUNK_DURATION`.
        let cancel = Arc::new(AtomicBool::new(false));
        let started_at = Instant::now();
        let pump_handle = tokio::spawn(run_pump(PumpContext {
            session_id: session.id,
            session_started_at: started_at,
            audio: Arc::clone(&self.audio),
            transcribe: Arc::clone(&self.transcribe),
            repo: Arc::clone(&self.repo),
            sources: sources.clone(),
            handles,
            cancel: Arc::clone(&cancel),
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
        });
        drop(guard);

        Ok(session)
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

        // Tell the pump to wind down, then wait for it to drain its
        // final chunk + append the resulting utterance. Awaiting the
        // join here matters: if we close the session row before the
        // pump's last append, the panel briefly shows "ended" with
        // a missing tail-of-conversation utterance.
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

        match self.repo.close_session(active.id).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Restore the active id so the caller can retry —
                // a transient SQLite failure shouldn't leave the
                // user without a way to close the session. The
                // pump is gone at this point so we restore an
                // ActiveSession with a no-op pump handle and a
                // fresh cancel flag (the old one already fired).
                if let Ok(mut guard) = self.state.lock() {
                    *guard = SessionState::Active(ActiveSession {
                        id: active.id,
                        started_at: active.started_at,
                        cancel: Arc::new(AtomicBool::new(false)),
                        pump_handle: Mutex::new(None),
                    });
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
        // legacy behavior). The pump path uses absolute offsets
        // computed from `session_started_at`; this hotkey-dictation
        // path doesn't have access to a comparable wall-clock so it
        // anchors at the previous utterance's end.
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
        }
    }
}

/// Owned context handed to the pump task at spawn time. Bundles the
/// per-session state plus shared handles so the task signature stays
/// readable.
struct PumpContext {
    session_id: i64,
    session_started_at: Instant,
    audio: Arc<dyn AudioCapture>,
    transcribe: crate::ipc::TranscribeSlot,
    repo: Arc<dyn MeetingSessionRepository>,
    sources: Vec<AudioSource>,
    handles: Vec<Box<dyn AudioSession>>,
    cancel: Arc<AtomicBool>,
}

/// Pump task body. Loops: sleep `CHUNK_DURATION`, drain each handle,
/// transcribe + append, restart capture; until the cancel flag flips.
/// On cancel, drains one final chunk, appends, exits.
///
/// All errors are logged and swallowed — the pump is fire-and-forget
/// from the spawn point's perspective, and a transient transcription
/// or append failure shouldn't tear down the user's session. The
/// audio capture is restarted across the failure so subsequent
/// chunks recover automatically.
async fn run_pump(mut ctx: PumpContext) {
    loop {
        // Sleep with periodic cancel polls. tokio::select! over the
        // full sleep + a cancellation channel would be tighter, but
        // a periodic poll keeps the cancel signalling synchronous
        // (AtomicBool, no Tokio channel) which makes the test mocks
        // simpler.
        let poll_interval = Duration::from_millis(100);
        let mut elapsed = Duration::ZERO;
        while elapsed < CHUNK_DURATION {
            if ctx.cancel.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(poll_interval).await;
            elapsed += poll_interval;
        }

        let cancelled = ctx.cancel.load(Ordering::Acquire);

        // Drain ALL handles BEFORE any transcription. Doing them
        // serially-with-transcription-in-between (the previous
        // shape) meant source B kept accumulating audio for the
        // duration of source A's transcription — a 30 s whisper
        // call against source A would have B holding 40 s of
        // samples by the time we reached its stop. Stopping every
        // handle first bounds each chunk's buffer to the original
        // wall-clock window plus the few-ms drain delay.
        let drained: Vec<Box<dyn AudioSession>> = ctx.handles.drain(..).collect();
        let chunk_end_offset_ms = ctx.session_started_at.elapsed().as_millis() as i64;
        let chunk_start_offset_ms = chunk_end_offset_ms.saturating_sub(elapsed.as_millis() as i64);

        let captured: Vec<(AudioSource, Result<CapturedAudio>)> = drained
            .into_iter()
            .map(|handle| {
                let source = handle.source().clone();
                let result = handle.stop();
                (source, result)
            })
            .collect();

        // Cancel-after-drain check: if Stop fired while we were
        // draining, exit before kicking off transcription. The
        // session's final-chunk transcription still happens below
        // (we treat captured as the "last chunk" data); we just
        // skip the restart that would kick off another cycle.
        let cancelled_during_drain = ctx.cancel.load(Ordering::Acquire);

        for (source, captured_result) in captured {
            let captured = match captured_result {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        source_kind = ?source.kind_label(),
                        session_id = ctx.session_id,
                        "meeting pump: stop of capture session failed"
                    );
                    continue;
                }
            };
            transcribe_and_append(
                ctx.session_id,
                source,
                captured,
                chunk_start_offset_ms,
                chunk_end_offset_ms,
                Arc::clone(&ctx.transcribe),
                Arc::clone(&ctx.repo),
            )
            .await;
        }

        if cancelled || cancelled_during_drain {
            return;
        }

        // Not cancelled — open fresh handles for the next chunk.
        // If a restart fails (TCC permission revoked mid-session,
        // device unplugged), log and drop that source from
        // `ctx.sources` so we don't churn the OS asking for a
        // permission we already lost. Keeping the failed source in
        // the loop produced a warning every 10 s for the rest of
        // the session — a 60-min meeting with a denied SCK turned
        // into ~360 redundant warnings.
        let sources_at_loop_start = ctx.sources.clone();
        ctx.sources.clear();
        for source in sources_at_loop_start {
            match ctx.audio.start_session(source.clone()) {
                Ok(h) => {
                    ctx.handles.push(h);
                    ctx.sources.push(source);
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        source_kind = ?source.kind_label(),
                        session_id = ctx.session_id,
                        "meeting pump: restart of capture session failed; dropping that source for the rest of the session"
                    );
                }
            }
        }

        // Every source failed to restart — no point looping further.
        if ctx.handles.is_empty() {
            tracing::error!(
                session_id = ctx.session_id,
                "meeting pump: no capture sessions could be restarted; pump exiting"
            );
            return;
        }
    }
}

/// Transcribe one chunk's captured audio and append the resulting
/// utterance under `session_id`. Tagged with the source kind in the
/// `speaker_label` slot ("mic" / "system") as a primitive form of
/// diarization ahead of #111. Errors are logged + swallowed so a
/// single bad chunk doesn't abort the whole session.
async fn transcribe_and_append(
    session_id: i64,
    source: AudioSource,
    captured: CapturedAudio,
    started_at_ms: i64,
    ended_at_ms: i64,
    transcribe: crate::ipc::TranscribeSlot,
    repo: Arc<dyn MeetingSessionRepository>,
) {
    let speaker_label = match &source {
        AudioSource::Microphone(_) => Some("mic".to_owned()),
        AudioSource::SystemAudio => Some("system".to_owned()),
    };

    // Snapshot the transcriber Arc out of the shared mutex so the
    // (potentially long) inference doesn't hold the lock.
    let transcriber = match transcribe.lock() {
        Ok(g) => g.clone(),
        Err(_) => {
            tracing::error!(session_id, "transcribe mutex poisoned in pump");
            return;
        }
    };
    let transcriber = match transcriber {
        Some(t) => t,
        None => {
            tracing::warn!(
                session_id,
                "meeting pump: no transcriber loaded; chunk dropped (model picker hasn't been used yet)"
            );
            return;
        }
    };

    // Whisper-rs is sync + blocking. Run on a blocking thread so
    // the tokio scheduler keeps the pump's other awaits (the cancel
    // poll on the next tick, parallel chunks from a sibling source)
    // responsive.
    let format = captured.format;
    let samples = captured.samples;
    let utterances = match tokio::task::spawn_blocking(move || {
        transcriber.transcribe_chunks(&[samples], format, "")
    })
    .await
    {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            tracing::warn!(
                error = ?e,
                source_kind = source.kind_label(),
                session_id,
                "meeting pump: transcription failed; chunk dropped"
            );
            return;
        }
        Err(e) => {
            tracing::error!(
                error = ?e,
                session_id,
                "meeting pump: transcribe blocking task panicked"
            );
            return;
        }
    };

    let text: String = utterances
        .iter()
        .filter(|u| u.is_final)
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned();

    if text.is_empty() {
        // Silent or sub-threshold chunk — don't pollute the panel
        // with empty rows. The user's eye for "did anything happen"
        // is the utterance count, and an empty row would inflate
        // it without telling them anything.
        return;
    }

    if let Err(e) = repo
        .append_utterance(NewPersistedUtterance {
            session_id,
            started_at_ms,
            ended_at_ms,
            speaker_label,
            text,
        })
        .await
    {
        tracing::warn!(
            error = ?e,
            session_id,
            "meeting pump: utterance append failed; chunk dropped"
        );
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
    use crate::audio::{AudioDevice, CaptureFormat, CapturedAudio};
    use crate::db::SqliteDatabase;
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
        let audio: Arc<dyn AudioCapture> = Arc::new(StubParallelAudio);
        let transcribe: Arc<Mutex<Option<Arc<dyn Transcribe>>>> = Arc::new(Mutex::new(None));
        SessionManager::new(repo, audio, transcribe)
    }

    #[tokio::test]
    async fn start_manual_opens_a_session_and_records_active_id() {
        let mgr = fresh_manager().await;
        assert!(mgr.active_session_id().is_none(), "no session at boot");

        let session = mgr
            .start_manual(
                vec![AudioSource::default_microphone()],
                Some("us.zoom.xos".into()),
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
        mgr.start_manual(vec![AudioSource::default_microphone()], None)
            .await
            .unwrap();
        let err = mgr
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(Vec::new(), None)
            .await
            .expect_err("empty source list must error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("at least one audio source"),
            "error must name the precondition; got: {msg}"
        );

        // The slot is back to Idle — a valid start now succeeds.
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None)
            .await
            .expect("post-rollback start must succeed");
        assert_eq!(mgr.active_session_id(), Some(session.id));
        mgr.stop_manual().await.unwrap();
    }

    #[tokio::test]
    async fn stop_manual_closes_the_session_and_clears_active_id() {
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None)
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
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], Some("Zoom".into()))
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
