//! Meeting Mode session manager — owns the "is a session active?"
//! state, the chunking pump that drives auto-recording, and the
//! policy for opening / closing sessions.
//!
//! ## Lifecycle
//!
//! Manual-start: the user clicks "Start a session" in the panel.
//! The manager opens audio capture handles for each chosen source
//! (mic + optional system audio), opens one streaming inference
//! session per source via [`crate::transcription::Transcribe::start_stream`],
//! creates the session row, and spawns a pump task. The pump
//! drains every audio handle on a `PUMP_TICK` cadence (without
//! stopping the handles), feeds the drained samples into the
//! corresponding streaming inference session, and dispatches
//! returned utterances: finals to the database, partials to the
//! in-memory partials store. When the user clicks Stop,
//! `stop_manual` cancels the pump, awaits its `finish()`-driven
//! tail-flush, clears partials, and writes `ended_at` on the
//! session row.
//!
//! Auto-detect from foreground app is the next phase ([#112]) —
//! the [`AppClassifier`] table is wired up but not yet driving
//! the start lifecycle.
//!
//! ## Speaker labels
//!
//! Each persisted utterance carries a `speaker_label`. The pump
//! runs every batch of finals through the configured `Diarize`
//! impl (production: `NoopDiarizer` since #243 — D1
//! `EnergyDiarizer` collapsed cross-source utterances into a
//! single "Speaker A"; reverted until D2 model-based diarization
//! lands in #111). With `NoopDiarizer` every diarized label is
//! `None`, so `dispatch_utterances` stamps the source-derived
//! `"mic"` / `"system"` tag from `AudioSource::speaker_tag()`
//! (the single source of truth for the persistence-layer label
//! shape); the panel maps that to "You" / "Remote" when
//! rendering.
//!
//! ## Streaming (post-#108)
//!
//! The pump opens one [`StreamingTranscribeSession`] per audio
//! source at session start and feeds samples into it on a tight
//! 500 ms tick (via [`AudioSession::drain_into`]). The streaming
//! session runs whisper.cpp on a rolling 30 s window every ~3 s of
//! new audio (see `transcription::streaming` for the policy
//! state-machine) and emits **partials** for the trailing tail and
//! **finals** for segments aged past the commit threshold. The pump
//! routes finals to the database (via the existing
//! [`MeetingSessionRepository::append_utterance`] path) and stores
//! partials in an in-memory `partials` map keyed by session id +
//! speaker label. The panel polls [`meeting_session_get`] which
//! merges the in-memory partials into the response — partials
//! never touch the database, so a session's persisted history
//! stays clean.
//!
//! The pre-#108 chunk-and-restart cycle (10 s chunks, stop-drain-
//! transcribe-restart) is gone. The new shape needs the audio
//! backend to support [`AudioSession::drain_into`] (PR2) and the
//! transcribe backend to support
//! [`crate::transcription::Transcribe::start_stream`] (PR1). When
//! either is absent (test mocks, or the rare backend that opted
//! out), the pump degrades to a no-op cycle that keeps the session
//! row open but emits no utterances — same end-state as
//! "transcriber not loaded" pre-#108, just via a different code
//! path.
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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

#[cfg(test)]
use crate::audio::CapturedAudio;
use crate::audio::{AudioCapture, AudioSession, AudioSource, CaptureFormat};
#[cfg(test)]
use crate::transcription::Transcribe;
use crate::transcription::{StreamingTranscribeSession, Utterance};

use super::{
    MeetingAppKind, MeetingSession, MeetingSessionRepository, NewMeetingSession,
    NewPersistedUtterance,
};

/// Canonical capture format passed to the diarizer (#111). The pump
/// no longer has the per-source audio at the dispatch boundary (the
/// streaming session has already consumed it), so D1's
/// [`crate::diarization::EnergyDiarizer`] — which only needs
/// utterance timestamps — can ignore this. D2's model-based path
/// will need real audio threaded through; that's a follow-up
/// refactor.
const CANONICAL_FORMAT: CaptureFormat = CaptureFormat {
    sample_rate: 16_000,
    channels: 1,
};

/// Pump tick interval — how often the streaming pump pulls samples
/// from each audio handle and feeds them into the per-source
/// streaming inference session. Inference itself happens internally
/// to the streaming session at its own cadence (the `infer_interval_ms`
/// config in `transcription::streaming`); this is just the rate at
/// which fresh audio reaches the streaming session's buffer.
///
/// 500 ms is a balance: short enough to keep the streaming session's
/// rolling window fresh (~6 ticks per inference at the default 3 s
/// inference interval), long enough to amortize the per-tick
/// `drain_into` round-trip + lock overhead. Tighter ticks would
/// raise CPU baseline noticeably; looser ticks would make the
/// streaming session's "I have new samples to consider" gate land
/// late and add jitter to the partial-update cadence.
const PUMP_TICK: Duration = Duration::from_millis(500);

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
/// Notifies the frontend when the meeting pump's per-source state
/// changes mid-session — specifically when a previously-running
/// source fails (TCC revoke, device unplug, inference panic) and is
/// dropped from the rest of the session. Without this signal the
/// panel keeps showing "recording from mic + system audio" while
/// one of those sources has silently gone dead.
///
/// Trait rather than a direct `tauri::AppHandle` dep so the
/// `SessionManager` stays unit-testable without spinning up a
/// Tauri runtime. The production impl in `crate::ipc::commands`
/// wraps an `AppHandle::emit`; tests pass a no-op or a recording
/// stub that captures emit-call args.
pub trait MeetingEventEmitter: Send + Sync {
    /// Fires when a per-source capture or inference path failed and
    /// the pump dropped that source for the rest of the session.
    /// `source_kind` is the same `kind_label` (`"microphone"` /
    /// `"system-audio"`) the pump uses elsewhere.
    fn source_failed(&self, session_id: i64, source_kind: &str, reason: &str);
}

/// No-op emitter for unit tests + the `new_for_test` constructor.
/// Production code constructs an `AppHandle`-backed emitter; the
/// `Noop` variant means "I don't care about pump events here".
pub struct NoopMeetingEventEmitter;

impl MeetingEventEmitter for NoopMeetingEventEmitter {
    fn source_failed(&self, _session_id: i64, _source_kind: &str, _reason: &str) {}
}

pub struct SessionManager {
    repo: Arc<dyn MeetingSessionRepository>,
    /// User-overrides repo (#112). Read at every session start so
    /// edits in the Settings panel take effect without an app
    /// restart. The cached `classifier` field below is rebuilt from
    /// a fresh override snapshot inside `start_manual`.
    app_overrides: Arc<dyn super::MeetingAppOverrideRepository>,
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
    partials: Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    /// Surface pump-side events (per-source failure mid-session) to
    /// the frontend. Production wires this to a `tauri::AppHandle`
    /// emitter; tests use [`NoopMeetingEventEmitter`].
    event_emitter: Arc<dyn MeetingEventEmitter>,
    /// Speaker diarization. Production wires
    /// [`crate::diarization::NoopDiarizer`] as of #243 — the
    /// `EnergyDiarizer` D1 silence-gap heuristic collapsed
    /// cross-source utterances into a single "Speaker A" when
    /// mic + system audio were both captured (the common Meeting
    /// Mode shape). `dispatch_utterances` then falls back to the
    /// source-derived `"mic"` / `"system"` tag from
    /// `AudioSource::speaker_tag()`. D2 (#111) is the upgrade
    /// path that can distinguish voices across sources.
    diarize: Arc<dyn crate::diarization::Diarize>,
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
        event_emitter: Arc<dyn MeetingEventEmitter>,
        diarize: Arc<dyn crate::diarization::Diarize>,
        app_overrides: Arc<dyn super::MeetingAppOverrideRepository>,
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
        let emitter: Arc<dyn MeetingEventEmitter> = Arc::new(NoopMeetingEventEmitter);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let app_overrides: Arc<dyn super::MeetingAppOverrideRepository> =
            Arc::new(NoOpAppOverrides);
        Self::new(repo, audio, transcribe, emitter, diarize, app_overrides)
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

        let cancel = Arc::new(AtomicBool::new(false));
        let started_at = Instant::now();
        let pump_handle = tokio::spawn(run_pump(PumpContext {
            session_id: session.id,
            session_started_at: started_at,
            repo: Arc::clone(&self.repo),
            sources: sources.clone(),
            handles,
            streaming_sessions,
            partials: Arc::clone(&self.partials),
            cancel: Arc::clone(&cancel),
            event_emitter: Arc::clone(&self.event_emitter),
            diarize: Arc::clone(&self.diarize),
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

        // The pump's finish() path already flushed any tail finals
        // to the database and cleared the per-source partials. Belt-
        // and-braces: clear our partials map for this session id so
        // a stale partial can't leak into a subsequent IPC poll
        // between this point and the pump's last write.
        if let Ok(mut guard) = self.partials.write() {
            guard.remove(&active.id);
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

/// Owned context handed to the pump task at spawn time. Bundles the
/// per-session state plus shared handles so the task signature stays
/// readable. Indices into `sources`, `handles`, and
/// `streaming_sessions` correspond to the same source.
struct PumpContext {
    session_id: i64,
    /// Wall-clock start of the session. Not currently read by the
    /// streaming pump — utterance offsets come from the streaming
    /// session's internal clock — but kept for parity with the
    /// pre-#108 path and for any future "session age" diagnostics.
    #[allow(dead_code)]
    session_started_at: Instant,
    repo: Arc<dyn MeetingSessionRepository>,
    sources: Vec<AudioSource>,
    handles: Vec<Box<dyn AudioSession>>,
    /// One streaming inference session per source, parallel to
    /// `sources` and `handles`. `None` means streaming was not
    /// available for that source at start time (no transcriber, or
    /// the backend's `start_stream` errored). The pump treats those
    /// sources as audio-only — drains them so the buffer doesn't
    /// grow unbounded, but feeds nothing to inference.
    streaming_sessions: Vec<Option<Box<dyn StreamingTranscribeSession>>>,
    /// Shared in-memory partials store (the manager's field). The
    /// pump's per-tick dispatch updates entries keyed by speaker
    /// label as inference returns partials, and removes them when
    /// inference returns the matching final.
    partials: Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    cancel: Arc<AtomicBool>,
    /// Notify the frontend when a per-source path drops out
    /// mid-session. The pump fires this on the inference panic
    /// path and the streaming-feed/drain failure path that today
    /// only emit `tracing::warn!` lines the user never sees.
    event_emitter: Arc<dyn MeetingEventEmitter>,
    /// Diarization seam (#111). The pump runs every batch of finals
    /// through this before stamping the source-derived label, so a
    /// non-Noop impl can override `"mic"` / `"system"` with
    /// per-speaker labels.
    diarize: Arc<dyn crate::diarization::Diarize>,
}

/// Pump task body. Loops on a `PUMP_TICK` cadence: drain each audio
/// handle into its per-source buffer, feed the buffer into the
/// streaming inference session, dispatch returned utterances
/// (partials → in-memory map, finals → DB). On cancel, calls
/// `finish()` on each streaming session to flush the tail and
/// persists those finals.
///
/// All errors are logged and swallowed — the pump is fire-and-forget
/// from the spawn point's perspective, and a transient drain or
/// inference failure shouldn't tear down the user's session.
async fn run_pump(mut ctx: PumpContext) {
    // Per-source scratch buffer reused across ticks. Sized at first
    // drain; subsequent drains amortize the capacity. Indexed
    // parallel to `handles` / `sources`.
    let mut drain_buffers: Vec<Vec<f32>> = (0..ctx.handles.len()).map(|_| Vec::new()).collect();

    // Per-tick scratch for the merge-sort-label-split pattern (#206).
    // Accumulates `(source_label, utterances)` pairs from each
    // source's inference, then `diarize_and_dispatch_merged` runs the
    // diarizer once over the chronologically-merged batch before
    // splitting back per source for dispatch. Pre-#206 this lived
    // inside the per-source loop, which meant the diarizer never saw
    // mic + system audio interleaved — its alternating-talker
    // heuristic produced "Speaker A/B" inside each source's stream
    // without coordination, so "Speaker A" meant different people
    // depending on which source the chunk came from.
    let mut tick_buckets: Vec<TickBucket> = Vec::new();

    loop {
        // Sleep with periodic cancel polls. The pump tick is shorter
        // than the previous chunk-and-restart cycle (500 ms vs 10 s),
        // so the per-poll cancel-flag check happens on every tick
        // boundary directly.
        if ctx.cancel.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(PUMP_TICK).await;
        if ctx.cancel.load(Ordering::Acquire) {
            break;
        }

        // Drain audio for every source first (cheap, no inference),
        // then run inference per source. The drain step takes
        // microseconds; the inference step takes milliseconds-to-
        // seconds inside the streaming session's `drain` if a new
        // inference window has matured. Splitting the loop bounds
        // each source's audio buffer to the tick window plus the
        // few-ms drain.
        for (i, handle) in ctx.handles.iter().enumerate() {
            let buf = &mut drain_buffers[i];
            buf.clear();
            if let Err(e) = handle.drain_into(buf) {
                tracing::warn!(
                    error = ?e,
                    source_kind = ctx.sources[i].kind_label(),
                    session_id = ctx.session_id,
                    "meeting pump: drain_into failed for tick"
                );
            }
        }

        // For each source with a streaming session, feed the drained
        // samples and run an inference tick. Move the session into
        // `spawn_blocking` so whisper inference doesn't block the
        // tokio worker; the helper returns the session along with
        // its drained utterances so we can put it back.
        //
        // Index loop rather than `iter().enumerate()` because we
        // mutate three parallel `Vec`s — `streaming_sessions`,
        // `drain_buffers`, and `sources` — and need split-borrow
        // semantics on each. Restructuring to a single iterator
        // would either require interior mutability on each slot
        // or unsafe pointer arithmetic; the indexed loop is the
        // clearest shape for this pattern.
        #[allow(clippy::needless_range_loop)]
        for i in 0..ctx.sources.len() {
            // Skip sources without a streaming session — drained
            // samples are discarded. Logging only on the first
            // skipped tick per source to avoid flooding the
            // tracing layer (every 500 ms for the whole session).
            if ctx.streaming_sessions[i].is_none() {
                continue;
            }
            // Take the session out so we can move it into
            // spawn_blocking. The `Option` slot stays None until we
            // put it back at the bottom of this iteration.
            // Defensive take: pre-#246 this was `.unwrap()`, but
            // a future refactor that drains in a different order
            // would panic the pump task. Skip the source for this
            // tick if the slot was already taken.
            let Some(session) = ctx.streaming_sessions[i].take() else {
                tracing::warn!(
                    source_kind = ctx.sources[i].speaker_tag(),
                    "meeting pump: streaming session slot already empty; skipping tick"
                );
                continue;
            };
            let samples = std::mem::take(&mut drain_buffers[i]);
            let source_label = ctx.sources[i].speaker_tag().to_owned();
            let session_id = ctx.session_id;

            // Spawn-blocking: returns (session, samples_buf,
            // Result<Vec<Utterance>>). The buffer round-trips so we
            // can put it back into `drain_buffers[i]` to keep its
            // capacity warm for the next tick.
            let join =
                tokio::task::spawn_blocking(
                    move || -> (
                        Box<dyn StreamingTranscribeSession>,
                        Vec<f32>,
                        Result<Vec<Utterance>>,
                    ) {
                        let mut session = session;
                        if !samples.is_empty() {
                            if let Err(e) = session.feed(&samples) {
                                return (session, samples, Err(e));
                            }
                        }
                        let result = session.drain();
                        (session, samples, result)
                    },
                )
                .await;

            let (returned_session, returned_buf, drain_result) = match join {
                Ok(triple) => triple,
                Err(join_err) => {
                    tracing::error!(
                        error = ?join_err,
                        session_id,
                        source_kind = source_label,
                        "meeting pump: streaming inference task panicked; \
                         leaving streaming disabled for this source for the rest of the session"
                    );
                    // Session is gone (panicked closure dropped it).
                    // Leave the slot None so subsequent ticks skip
                    // this source. Notify the frontend so the panel
                    // can surface "this source dropped" rather than
                    // silently rendering "still recording".
                    ctx.event_emitter.source_failed(
                        session_id,
                        &source_label,
                        "transcription task panicked",
                    );
                    continue;
                }
            };

            // Restore the session + buffer for the next tick.
            ctx.streaming_sessions[i] = Some(returned_session);
            drain_buffers[i] = returned_buf;

            let utterances = match drain_result {
                Ok(u) => u,
                Err(e) => {
                    let reason = format!("{e}");
                    tracing::warn!(
                        error = ?e,
                        session_id,
                        source_kind = source_label,
                        "meeting pump: streaming feed/drain failed for tick"
                    );
                    // Drop the session so subsequent ticks skip this
                    // source — keeping a wedged session in the slot
                    // would loop the same warning every 500 ms for
                    // the rest of the meeting.
                    ctx.streaming_sessions[i] = None;
                    ctx.event_emitter
                        .source_failed(session_id, &source_label, &reason);
                    continue;
                }
            };

            // Accumulate this source's utterances into the tick
            // bucket. The per-tick `diarize_and_dispatch_merged`
            // call below runs the diarizer once over the merged +
            // chronologically-sorted batch, then splits the labelled
            // result back per source for dispatch (#206).
            tick_buckets.push(TickBucket {
                source_label,
                utterances,
            });
        }

        if !tick_buckets.is_empty() {
            diarize_and_dispatch_merged(
                ctx.session_id,
                std::mem::take(&mut tick_buckets),
                &ctx.diarize,
                &ctx.partials,
                &ctx.repo,
            )
            .await;
        }
    }

    // Cancel — flush each streaming session. `finish` drains
    // anything still in the rolling window as finals; we persist
    // those before returning so `stop_manual` sees the
    // tail-of-conversation utterances. Same merge-sort-label-split
    // shape as the per-tick path (#206) so the tail flush can't
    // re-introduce the per-source independent-A/B regression.
    let mut tail_buckets: Vec<TickBucket> = Vec::new();
    #[allow(clippy::needless_range_loop)] // see explanation in the tick loop above
    for i in 0..ctx.sources.len() {
        let Some(session) = ctx.streaming_sessions[i].take() else {
            continue;
        };
        let source_label = ctx.sources[i].speaker_tag().to_owned();
        let session_id = ctx.session_id;
        let join = tokio::task::spawn_blocking(move || session.finish()).await;
        let finals = match join {
            Ok(Ok(u)) => u,
            Ok(Err(e)) => {
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: streaming finish failed; tail dropped"
                );
                continue;
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    session_id,
                    "meeting pump: streaming finish task panicked"
                );
                continue;
            }
        };
        tail_buckets.push(TickBucket {
            source_label,
            utterances: finals,
        });
    }

    if !tail_buckets.is_empty() {
        diarize_and_dispatch_merged(
            ctx.session_id,
            tail_buckets,
            &ctx.diarize,
            &ctx.partials,
            &ctx.repo,
        )
        .await;
    }

    // Belt-and-braces: clear partials for this session id. The
    // dispatch loop above removes per-source entries on each final
    // commit; this drops the (now-empty) per-session HashMap so the
    // partials store doesn't grow unbounded across many sessions.
    if let Ok(mut guard) = ctx.partials.write() {
        guard.remove(&ctx.session_id);
    }
}

/// One source's worth of utterances for the merge-sort-label-split
/// pump dispatch (#206). The pump accumulates these per tick (and
/// once at tail flush), then `diarize_and_dispatch_merged` runs the
/// diarizer over the chronologically-merged batch and dispatches
/// each source's labelled slice through `dispatch_utterances`.
struct TickBucket {
    source_label: String,
    utterances: Vec<Utterance>,
}

/// Diarize + dispatch a tick's worth of utterances across all
/// sources, in chronological order (#206).
///
/// Pre-#206 the dispatch was per-source: the pump called
/// `diarize.label_utterances` once per source bucket and dispatched
/// each separately. The diarizer never saw mic + system audio
/// interleaved, so its alternating-talker heuristic produced
/// `"Speaker A" / "Speaker B"` independently inside each source
/// stream — meaning "Speaker A" referred to a different actual
/// speaker on a mic+system meeting depending on which source the
/// utterance came from.
///
/// The fix here is purely structural: tag each utterance with its
/// source-bucket index, sort the merged list by `started_at_ms`,
/// run the diarizer once, then split the labelled result back into
/// per-source slices (preserving original source order) for the
/// existing `dispatch_utterances` path. The trait surface is
/// unchanged; the wiring carries the cross-source coordination.
async fn diarize_and_dispatch_merged(
    session_id: i64,
    buckets: Vec<TickBucket>,
    diarize: &Arc<dyn crate::diarization::Diarize>,
    partials: &Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    repo: &Arc<dyn MeetingSessionRepository>,
) {
    if buckets.is_empty() {
        return;
    }

    // Hold the source labels in original order — the dispatch loop
    // at the bottom needs them, but the merge step consumes the
    // bucket vec.
    let source_labels: Vec<String> = buckets.iter().map(|b| b.source_label.clone()).collect();

    // Tag each utterance with its source bucket index, then move
    // into a flat `(idx, utterance)` vec. Owning move avoids the
    // double-clone shape the naive version had.
    let mut tagged: Vec<(usize, Utterance)> = Vec::new();
    for (idx, bucket) in buckets.into_iter().enumerate() {
        for u in bucket.utterances {
            tagged.push((idx, u));
        }
    }

    if tagged.is_empty() {
        return;
    }

    // Sort by start time. `sort_by_key` is stable, so utterances
    // sharing a `started_at_ms` keep their original per-source
    // arrival order — important when mic + system happen to
    // produce simultaneous finals and we don't want a race-y
    // re-ordering on every tick.
    tagged.sort_by_key(|(_, u)| u.started_at_ms);

    // Split tags from utterances (move out, no clones). Diarizer
    // takes `&mut [Utterance]` so it sees the chronological
    // sequence and labels accordingly.
    let mut bucket_indices: Vec<usize> = Vec::with_capacity(tagged.len());
    let mut chronological: Vec<Utterance> = Vec::with_capacity(tagged.len());
    for (idx, u) in tagged {
        bucket_indices.push(idx);
        chronological.push(u);
    }
    diarize.label_utterances(&mut chronological, &[], CANONICAL_FORMAT);

    // Re-split the labelled vec back into per-source buckets,
    // preserving original source order so the dispatch order
    // matches the pre-#206 behaviour.
    let mut split: Vec<Vec<Utterance>> = (0..source_labels.len()).map(|_| Vec::new()).collect();
    for (idx, u) in bucket_indices.into_iter().zip(chronological) {
        split[idx].push(u);
    }

    for (label, utts) in source_labels.into_iter().zip(split) {
        dispatch_utterances(session_id, &label, utts, partials, repo).await;
    }
}

/// Route streaming-session output: finals land in the database,
/// partials land in the in-memory map. Falls back to the source-
/// derived `speaker_label` (`"mic"` / `"system"`) when the
/// diarizer hasn't already set one — so the panel always has a
/// label to render with.
///
/// Errors are logged + swallowed — a single bad utterance shouldn't
/// abort the session.
async fn dispatch_utterances(
    session_id: i64,
    source_label: &str,
    utterances: Vec<Utterance>,
    partials: &Arc<RwLock<HashMap<i64, HashMap<String, Utterance>>>>,
    repo: &Arc<dyn MeetingSessionRepository>,
) {
    for mut u in utterances {
        // Source-derived speaker label is the fallback for any
        // utterance whose diarizer abstained (`NoopDiarizer`, or
        // a future impl that emits None for low-confidence cases).
        // Production wires `EnergyDiarizer` (#201) which always
        // produces a per-speaker tag; this branch is the
        // swap-back-to-Noop / D2-abstain path.
        if u.speaker_label.is_none() {
            u.speaker_label = Some(source_label.to_owned());
        }

        if u.is_final {
            // Skip empty finals — the streaming session usually
            // filters them, but defence in depth (whitespace-only
            // text from a non-speech segment) keeps the panel
            // clean.
            let trimmed = u.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Clear the in-flight partial for this source first —
            // the user just saw the partial firm up into a final, so
            // the partial slot for this source belongs to whatever
            // segment comes next. Doing this BEFORE the DB append
            // means a concurrent IPC poll between the partial-clear
            // and the DB-append sees neither (better than seeing
            // both, which would briefly show the same text twice).
            if let Ok(mut guard) = partials.write() {
                if let Some(per_session) = guard.get_mut(&session_id) {
                    per_session.remove(source_label);
                }
            }

            if let Err(e) = repo
                .append_utterance(NewPersistedUtterance {
                    session_id,
                    started_at_ms: u.started_at_ms as i64,
                    ended_at_ms: u.ended_at_ms as i64,
                    speaker_label: u.speaker_label.clone(),
                    text: trimmed.to_owned(),
                })
                .await
            {
                tracing::warn!(
                    error = ?e,
                    session_id,
                    source_kind = source_label,
                    "meeting pump: utterance append failed; final dropped"
                );
            }
        } else {
            // Partial — replace the in-flight slot for this source.
            // The map is keyed by source label so mic + system don't
            // overwrite each other.
            if let Ok(mut guard) = partials.write() {
                guard
                    .entry(session_id)
                    .or_insert_with(HashMap::new)
                    .insert(source_label.to_owned(), u);
            }
        }
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
    /// User-supplied overrides loaded from
    /// [`super::MeetingAppOverrideRepository`] (#112). Consulted
    /// before the static `entries` table — an override row with the
    /// same `app_name` as a default wins.
    ///
    /// Snapshot at construction time. Edits to the override table
    /// from the Settings panel don't propagate live; the next
    /// session start reads a fresh snapshot. Live propagation would
    /// need an event-driven invalidation, which the manual-start
    /// session lifecycle doesn't justify yet.
    overrides: Vec<(String, MeetingAppKind)>,
}

impl AppClassifier {
    /// Hardcoded defaults. Each entry matches what
    /// `active-win-pos-rs::get_active_window().app_name` returns on
    /// the corresponding platform — macOS prefers reverse-DNS bundle
    /// ids, Linux returns the process / app name, and Windows
    /// returns the executable basename (with `.exe`). To cover all
    /// three OSes the table lists every variant of an app
    /// explicitly: matching is exact-string, no normalisation, so
    /// "Zoom" on Linux and "Zoom.exe" on Windows must each be its
    /// own entry. Locale variants (e.g. "Microsoft Teams (work or
    /// school)") only land here if active-win actually returns them
    /// in shipped builds — covering every translation is unbounded.
    pub fn default_table() -> Self {
        Self {
            entries: vec![
                // ---- Meeting / video-call apps ----
                // Auto-start (when that policy lands) defaults to
                // "ask" for these.
                //
                // Zoom
                ("zoom.us", MeetingAppKind::Meeting),
                ("us.zoom.xos", MeetingAppKind::Meeting), // macOS bundle
                ("Zoom", MeetingAppKind::Meeting),        // Linux / display name
                ("Zoom Meetings", MeetingAppKind::Meeting),
                ("zoom", MeetingAppKind::Meeting), // Linux process
                ("Zoom.exe", MeetingAppKind::Meeting), // Windows
                ("zoom.exe", MeetingAppKind::Meeting),
                // Microsoft Teams
                ("Microsoft Teams", MeetingAppKind::Meeting),
                ("com.microsoft.teams2", MeetingAppKind::Meeting), // macOS bundle
                ("Microsoft Teams (work or school)", MeetingAppKind::Meeting),
                ("ms-teams", MeetingAppKind::Meeting),
                ("ms-teams.exe", MeetingAppKind::Meeting), // Windows
                ("Teams.exe", MeetingAppKind::Meeting),
                ("teams-for-linux", MeetingAppKind::Meeting), // unofficial Linux client
                // Google Meet (largely browser-based, but a few
                // PWAs / wrappers exist).
                ("Google Meet", MeetingAppKind::Meeting),
                ("Meet", MeetingAppKind::Meeting),
                // Discord
                ("Discord", MeetingAppKind::Meeting),
                ("com.hnc.Discord", MeetingAppKind::Meeting), // macOS bundle
                ("discord", MeetingAppKind::Meeting),         // Linux process
                ("Discord.exe", MeetingAppKind::Meeting),     // Windows
                // Slack
                ("Slack", MeetingAppKind::Meeting),
                ("com.tinyspeck.slackmacgap", MeetingAppKind::Meeting), // macOS bundle
                ("slack", MeetingAppKind::Meeting),                     // Linux process
                ("slack.exe", MeetingAppKind::Meeting),                 // Windows
                ("Slack.exe", MeetingAppKind::Meeting),
                // Webex
                ("Webex", MeetingAppKind::Meeting),
                ("Cisco Webex Meetings", MeetingAppKind::Meeting),
                ("webex", MeetingAppKind::Meeting),
                ("Webex.exe", MeetingAppKind::Meeting),
                ("CiscoCollabHost.exe", MeetingAppKind::Meeting),
                // Skype (legacy but still in active use, especially
                // for international calls).
                ("Skype", MeetingAppKind::Meeting),
                ("skype", MeetingAppKind::Meeting),
                ("Skype.exe", MeetingAppKind::Meeting),
                // GoTo / GoToMeeting
                ("GoToMeeting", MeetingAppKind::Meeting),
                ("GoToMeeting.exe", MeetingAppKind::Meeting),
                ("GoTo", MeetingAppKind::Meeting),
                // BlueJeans (Verizon)
                ("BlueJeans", MeetingAppKind::Meeting),
                ("BlueJeans.exe", MeetingAppKind::Meeting),
                // Loom (async video — not a live call but the
                // recording surface is the same)
                ("Loom", MeetingAppKind::Meeting),
                ("Loom.exe", MeetingAppKind::Meeting),
                // ---- Media apps ----
                // Auto-start (when shipped) defaults to "no" for
                // these — most users don't want a YouTube watch-
                // party transcribed by accident.
                //
                // YouTube (typically a browser tab; PWA / wrappers
                // included for completeness)
                ("YouTube", MeetingAppKind::Media),
                // Spotify
                ("Spotify", MeetingAppKind::Media),
                ("com.spotify.client", MeetingAppKind::Media), // macOS bundle
                ("spotify", MeetingAppKind::Media),            // Linux process
                ("Spotify.exe", MeetingAppKind::Media),
                // Apple Music / iTunes (macOS) and the legacy iTunes
                // on Windows.
                ("Apple Music", MeetingAppKind::Media),
                ("Music", MeetingAppKind::Media),
                ("iTunes", MeetingAppKind::Media),
                ("iTunes.exe", MeetingAppKind::Media),
                ("Podcasts", MeetingAppKind::Media),
                // Apple TV desktop on macOS — sound is system
                // audio, the surfaced app is "TV".
                ("TV", MeetingAppKind::Media),
                // VLC — cross-platform default media player
                ("VLC", MeetingAppKind::Media),
                ("VLC media player", MeetingAppKind::Media),
                ("vlc", MeetingAppKind::Media),
                ("vlc.exe", MeetingAppKind::Media),
                // Plex / Plexamp
                ("Plex", MeetingAppKind::Media),
                ("Plex.exe", MeetingAppKind::Media),
                ("plexamp", MeetingAppKind::Media),
                ("Plexamp", MeetingAppKind::Media),
            ],
            overrides: Vec::new(),
        }
    }

    /// Construct with a user-override snapshot loaded from the
    /// repository. The override list is checked before the static
    /// defaults, so a row with the same `app_name` as a default
    /// wins.
    pub fn with_overrides(overrides: Vec<(String, MeetingAppKind)>) -> Self {
        let mut classifier = Self::default_table();
        classifier.overrides = overrides;
        classifier
    }

    pub fn classify(&self, app_name: &str) -> MeetingAppKind {
        // User overrides win over defaults — even when an override
        // explicitly maps an app the table classifies as Meeting to
        // Other (the way to ignore an app the defaults catch).
        for (key, kind) in &self.overrides {
            if key == app_name {
                return *kind;
            }
        }
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
        let emitter: Arc<dyn MeetingEventEmitter> = Arc::new(NoopMeetingEventEmitter);
        let diarize: Arc<dyn crate::diarization::Diarize> =
            Arc::new(crate::diarization::NoopDiarizer);
        let app_overrides: Arc<dyn crate::meeting::MeetingAppOverrideRepository> =
            Arc::new(NoOpAppOverrides);
        SessionManager::new(repo, audio, transcribe, emitter, diarize, app_overrides)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], Some("Zoom".into()))
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
            .start_manual(vec![AudioSource::default_microphone()], None)
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
    /// receives, then writes deterministic `"Speaker A"` labels so
    /// the test can assert order without relying on the real
    /// `EnergyDiarizer` heuristic.
    struct RecordingDiarizer {
        seen_starts: Mutex<Vec<u64>>,
    }

    impl crate::diarization::Diarize for RecordingDiarizer {
        fn label_utterances(
            &self,
            utterances: &mut [crate::transcription::Utterance],
            _audio: &[Vec<f32>],
            _format: crate::audio::CaptureFormat,
        ) {
            let mut seen = self.seen_starts.lock().unwrap();
            for u in utterances.iter() {
                seen.push(u.started_at_ms);
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
            .start_manual(vec![AudioSource::default_microphone()], None)
            .await
            .unwrap();

        let recorder = Arc::new(RecordingDiarizer {
            seen_starts: Mutex::new(Vec::new()),
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
        };
        let sys_bucket = TickBucket {
            source_label: "system".to_owned(),
            utterances: vec![
                make_final("sys-100", 100, 180, "system"),
                make_final("sys-300", 300, 380, "system"),
            ],
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
    async fn stop_manual_clears_partials_for_the_session() {
        // Defence in depth: stop_manual clears any partials still
        // in the store for the closing session. Without this, a
        // subsequent IPC poll between stop_manual returning and the
        // pump's last dispatch could expose a stale partial.
        let mgr = fresh_manager().await;
        let session = mgr
            .start_manual(vec![AudioSource::default_microphone()], None)
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
}
