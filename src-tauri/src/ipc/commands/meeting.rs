//! Meeting Mode IPC commands (refs #33 / #109).
//!
//! Long-running multi-source capture sessions with You/Remote-tagged
//! transcripts. Backed by:
//!
//! - `meetings` repository — persists sessions + utterances rows.
//! - `meeting_manager` (`crate::meeting::SessionManager`) — owns the
//!   active session's pump task, in-memory partials store, and source
//!   handle lifecycle.
//!
//! The IPC surface is split across two flavours:
//!
//! - **Read / browse**: `meeting_sessions_list`, `meeting_session_get`,
//!   `meeting_session_delete`, `meeting_session_set_notes`,
//!   `meeting_active_session`.
//! - **Lifecycle**: `meeting_start_manual`, `meeting_stop_manual`. Each
//!   also drives the recording HUD show/hide.
//!
//! Extracted from `commands/mod.rs` under #82 to give the meeting
//! domain its own seam — by far the largest cohesive group in the
//! IPC layer (7 commands + types + a sanitiser).

use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, State};

use crate::audio::AudioSource;
use crate::ipc::AppState;
use crate::meeting::export::{
    meeting_session_csv, meeting_session_json, meeting_session_text, MeetingExportFormat,
};

use super::{poisoned, validate_export_path, IpcError, IpcResult};

/// List all meeting sessions, newest-first. Returns whatever the
/// `SessionManager` pump has persisted — empty for a fresh
/// install, populated after the user has run a meeting.
#[tauri::command]
pub async fn meeting_sessions_list(
    state: State<'_, AppState>,
) -> IpcResult<Vec<crate::meeting::MeetingSession>> {
    state
        .data
        .meetings
        .list()
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("sessions list: {e:#}")))
}

/// Cross-stream meeting search (#357 phase 2). Matches `query`
/// against the FTS5 index over `utterances.text` and returns the
/// distinct sessions whose utterances hit. The unified History
/// surface calls this in parallel with `history_search` so the
/// search box queries dictation + meetings in lockstep.
///
/// An empty / whitespace-only `query` is treated as "no filter"
/// and returns the full list — same shape as `history_search`'s
/// fallback. Avoids handing FTS5 a malformed phrase that would
/// throw a "no such column" error for trivially-empty input.
#[tauri::command]
pub async fn meeting_sessions_search(
    state: State<'_, AppState>,
    query: String,
) -> IpcResult<Vec<crate::meeting::MeetingSession>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return state
            .data
            .meetings
            .list()
            .await
            .map_err(|e| IpcError::MeetingSessions(format!("sessions list: {e:#}")));
    }
    state
        .data
        .meetings
        .search_sessions(trimmed)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("sessions search: {e:#}")))
}

/// Full detail for one session: the row plus all its persisted
/// utterances ordered by `started_at_ms ASC`, plus any in-flight
/// partials the streaming pump has produced for the still-active
/// session (post-#108 PR3).
///
/// `current_partials` is **never persisted** — it lives in the
/// `SessionManager`'s in-memory partials store and gets merged into
/// this response on each poll. The frontend is expected to render
/// these with reduced opacity / italic to distinguish from the
/// settled `utterances` list (#108 PR4). For closed sessions
/// (`session.endedAt` is non-null), `current_partials` is always
/// empty — the pump cleared its store on stop.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSessionDetail {
    pub session: crate::meeting::MeetingSession,
    pub utterances: Vec<crate::meeting::PersistedUtterance>,
    /// In-flight partials, one per active source. Sorted alphabet-
    /// ically by `speakerLabel` so the frontend's render order is
    /// stable across polls.
    pub current_partials: Vec<crate::transcription::Utterance>,
}

/// Detail view for one session — used by the panel's
/// session-detail route.
///
/// Errors `Settings` if the session id doesn't exist; the panel
/// surfaces a "this session was deleted" empty state.
#[tauri::command]
pub async fn meeting_session_get(
    state: State<'_, AppState>,
    id: i64,
) -> IpcResult<MeetingSessionDetail> {
    // Single-row PK lookup (#253) — pre-fix this loaded every
    // session row with `list()` and ran a linear `find` on the
    // result, scaling O(N) over the user's entire meeting
    // history per detail-panel open.
    let session = state
        .data
        .meetings
        .get_by_id(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session get: {e:#}")))?
        .ok_or_else(|| IpcError::MeetingSessions(format!("session {id} not found")))?;
    let utterances = state
        .data
        .meetings
        .list_utterances(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session utterances: {e:#}")))?;
    // Read in-flight partials from the manager's in-memory store.
    // The poll path is hot (every ~1 s while a session is active);
    // `current_partials_for` uses an `RwLock::read` and clones a
    // small Vec, so the cost is negligible.
    let current_partials = state.meeting_manager.current_partials_for(id);
    Ok(MeetingSessionDetail {
        session,
        utterances,
        current_partials,
    })
}

/// Delete a session and its utterances (FK cascade).
///
/// Rejects deletion of the active session — deleting the parent row
/// while the pump is writing would cause silent append failures and
/// leave `close_session` updating zero rows on stop (#833).
#[tauri::command]
pub async fn meeting_session_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    if state.meeting_manager.active_session_id() == Some(id) {
        return Err(IpcError::MeetingSessions(
            "cannot delete the active recording session".to_owned(),
        ));
    }
    state
        .data
        .meetings
        .delete(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session delete: {e:#}")))
}

/// Export a single meeting session to a file the user just picked
/// via `tauri-plugin-dialog`'s `save()` (#357 phase 3b). Same
/// trust model as `history_export_row_csv`: the dialog plugin
/// resolved the path, this IPC writes the bytes — no
/// `tauri-plugin-fs` needed.
///
/// Speaker labels in the output match what the History meeting
/// row renders: `You` for `mic`, `Remote` for `system`, the
/// existing label for model-derived `Speaker N`, and `Unknown`
/// for null. Audio is never persisted (PRD §5b) — and never
/// exported either; this command only emits transcript text.
#[tauri::command]
pub async fn meeting_session_export(
    state: State<'_, AppState>,
    id: i64,
    format: MeetingExportFormat,
    path: String,
) -> IpcResult<()> {
    validate_export_path(&path)?;
    let session = state
        .data
        .meetings
        .get_by_id(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session get: {e:#}")))?
        .ok_or_else(|| IpcError::MeetingSessions(format!("session {id} not found")))?;
    let utterances = state
        .data
        .meetings
        .list_utterances(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session utterances: {e:#}")))?;

    let body = match format {
        MeetingExportFormat::Text => meeting_session_text(&session, &utterances),
        MeetingExportFormat::Csv => meeting_session_csv(&session, &utterances)
            .map_err(|e| IpcError::Internal(format!("CSV write: {e:#}")))?,
        MeetingExportFormat::Json => meeting_session_json(&session, &utterances)
            .map_err(|e| IpcError::Internal(format!("JSON write: {e:#}")))?,
    };

    super::atomic_write(std::path::Path::new(&path), body.as_bytes()).await
}

/// Update a session's freeform notes. The panel calls this on blur
/// of the notes textarea.
#[tauri::command]
pub async fn meeting_session_set_notes(
    state: State<'_, AppState>,
    id: i64,
    notes: Option<String>,
) -> IpcResult<()> {
    state
        .data
        .meetings
        .set_notes(id, notes)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session set_notes: {e:#}")))
}

// -- Per-app classifier overrides (Phase E, #112) ----------------------
//
// User-supplied overrides for the meeting-app classifier. The
// SessionManager reads these at every session start so an edit in
// the Settings panel takes effect on the next start without an
// app restart.

/// All overrides, ordered by `app_name`.
#[tauri::command]
pub async fn meeting_app_override_list(
    state: State<'_, AppState>,
) -> IpcResult<Vec<crate::meeting::MeetingAppOverride>> {
    state
        .data
        .meeting_app_overrides
        .list()
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("app overrides list: {e:#}")))
}

/// Insert or update the override for `app_name`. The Settings panel
/// uses this both for the "add new" form and for in-place kind
/// changes on existing rows.
#[tauri::command]
pub async fn meeting_app_override_upsert(
    state: State<'_, AppState>,
    app_name: String,
    kind: crate::meeting::MeetingAppKind,
) -> IpcResult<crate::meeting::MeetingAppOverride> {
    let trimmed = app_name.trim();
    if trimmed.is_empty() {
        return Err(IpcError::MeetingSessions(
            "app_name must not be empty".into(),
        ));
    }
    state
        .data
        .meeting_app_overrides
        .upsert(crate::meeting::NewMeetingAppOverride {
            app_name: trimmed.to_owned(),
            kind,
        })
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("app overrides upsert: {e:#}")))
}

/// One entry in the built-in app classification table — a single
/// `(app_name, kind)` row from `AppClassifier::default_table()`.
/// The Settings panel renders these read-only so users can see
/// what's already covered before adding a redundant override
/// (#320).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinAppEntry {
    /// The exact string `active-win-pos-rs` returns for this app
    /// on whichever platform — bundle ID on macOS, exe basename
    /// on Windows, process name on Linux. The classifier uses
    /// exact-string matching with no normalisation, so each
    /// platform variant is its own row.
    pub app_name: String,
    pub kind: crate::meeting::MeetingAppKind,
}

/// Read the built-in classification table. Stable for a given
/// build of Hush; the panel reads it once on mount + caches.
/// Order matches `default_table()` (curated by app) so the panel
/// can render meaningful groupings without re-sorting.
#[tauri::command]
pub fn meeting_app_classifier_defaults() -> IpcResult<Vec<BuiltinAppEntry>> {
    let classifier = crate::meeting::AppClassifier::default_table();
    Ok(classifier
        .default_entries()
        .iter()
        .map(|(name, kind)| BuiltinAppEntry {
            app_name: name.clone(),
            kind: *kind,
        })
        .collect())
}

/// Set or clear the per-app audio profile fields (#427 Item 5).
/// Pass `None` (frontend `null`) to reset a field to "use the
/// global default", or `Some(value)` to pin a value. Both fields
/// are written every call so the panel sends the full intended
/// state — no merge.
///
/// The row must exist (typically via a prior
/// `meeting_app_override_upsert`); a missing row surfaces as an
/// error so the panel can re-list and re-render. Returns the
/// updated row so the frontend can patch its local list without a
/// follow-up `list` round-trip.
#[tauri::command]
pub async fn meeting_app_override_set_profile(
    state: State<'_, AppState>,
    app_name: String,
    preferred_audio_source: Option<String>,
    preferred_model_id: Option<String>,
) -> IpcResult<crate::meeting::MeetingAppOverride> {
    state
        .data
        .meeting_app_overrides
        .set_profile(
            &app_name,
            preferred_audio_source.as_deref(),
            preferred_model_id.as_deref(),
        )
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("app overrides set_profile: {e:#}")))
}

/// Delete the override for the given app. No-op if no row exists.
#[tauri::command]
pub async fn meeting_app_override_delete(
    state: State<'_, AppState>,
    app_name: String,
) -> IpcResult<()> {
    state
        .data
        .meeting_app_overrides
        .delete(&app_name)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("app overrides delete: {e:#}")))
}

/// Snapshot of the active meeting session. Empty (`active: None`)
/// means no session is in flight; the panel renders the start
/// button. `Some(id)` means a session is active; the panel renders
/// the stop button + a live status line.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMeetingSession {
    pub active: Option<i64>,
}

/// Read the active session id (if any). The panel polls this on
/// mount + after every state change so it can render the right
/// affordances.
#[tauri::command]
pub fn meeting_active_session(state: State<'_, AppState>) -> ActiveMeetingSession {
    ActiveMeetingSession {
        active: state.meeting_manager.active_session_id(),
    }
}

/// Open a meeting session manually (button-driven).
///
/// `sources` is the list of audio sources the meeting pump will
/// capture from in parallel. Required (a meeting needs at least one
/// source); the frontend's panel picker normally sends a single mic
/// source, evolving to mic + system-audio under Phase 3 of #122.
///
/// `app_name` is the bundle id / process name the session should be
/// attributed to. Frontend doesn't have direct access to
/// `active-win-pos-rs` so it usually passes `None`; the backend
/// queries the foreground window itself and uses that for both
/// the `app_name` (when unsupplied) and the `app_title` metadata
/// (#242 follow-up — captures the active window's title so a
/// browser-hosted session reads as "Vivaldi — <video title>"
/// rather than just "Vivaldi").
///
/// Errors with `IpcError::MeetingSessions` if a session is already
/// active — the user must close the existing one first.
#[tauri::command]
pub async fn meeting_start_manual(
    app: AppHandle,
    state: State<'_, AppState>,
    sources: Vec<AudioSource>,
    app_name: Option<String>,
) -> IpcResult<crate::meeting::MeetingSession> {
    let sources = sanitise_meeting_sources(sources)
        .map_err(|e| IpcError::MeetingSessions(format!("start_manual: {e:#}")))?;

    // Pre-flight: fail fast if no transcription model is loaded (#898).
    // Mirrors the dictation path (dictation/pipeline.rs:54) — same guard,
    // same error variant. Runs before opening any audio handles so the user
    // sees the error before any recording is started.
    {
        let guard = state.transcribe_meeting.lock().map_err(poisoned)?;
        if guard.is_none() {
            return Err(IpcError::TranscriptionUnavailable);
        }
    }

    // Snapshot the foreground app once at click time. The
    // `active-win-pos-rs` call is single-millisecond synchronous,
    // so we do it here rather than complicate the frontend with a
    // second IPC. Failure is non-fatal — manual sessions still
    // work without metadata, they just render as "manual / Other"
    // until the user adds notes.
    let (probed_app_name, app_title) = capture_foreground_app();
    // Normalize blank/whitespace-only strings so they don't win over the
    // probed foreground app name — frontend could pass stale empty state (#921).
    let resolved_app_name = app_name
        .and_then(|s| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        })
        .or(probed_app_name);

    let session = state
        .meeting_manager
        .start_manual(sources, resolved_app_name, app_title)
        .await
        .map_err(|e| {
            // Promote permission-shaped chains to the typed
            // `PermissionDenied` variant (#386). The frontend then
            // matches on `kind === "permission-denied"` instead of
            // substring-scraping the message — the substring path
            // stays as a fallback for unrecognised chains.
            if let Some(perm) = super::classify_permission_error(&e) {
                IpcError::PermissionDenied(perm.to_owned())
            } else {
                IpcError::MeetingSessions(format!("start_manual: {e:#}"))
            }
        })?;
    // Show the recording HUD so the user has the same at-a-glance
    // "audio is being captured" cue meeting mode that the dictation
    // hot path already provides. Suppressed entirely when the user
    // has flipped HUD off in Settings → General.
    //
    // `hud::show_async` dispatches onto the main thread because
    // this command is `async fn` and runs on a tokio worker; the
    // underlying `orderFront:` AppKit call is main-thread-only and
    // macOS 26 enforces that strictly (#476). Sync `start_dictation`
    // can call `hud::show` directly because Tauri lands its sync
    // handlers on the main thread.
    if state
        .runtime_flags
        .hud_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        crate::hud::show_async(&app);
        // Force the HUD into Recording with a fresh `started_at_ms`
        // so the persistent HUD page resets its elapsed-time counter
        // (#481). Pre-fix the meeting flow only called `show` and
        // relied on the HUD page's default `hudState = "recording"`
        // — which kept the previous session's `recordingStartedAt`
        // alive across back-to-back meetings.
        if let Err(e) = crate::hud::set_state(
            &app,
            crate::hud::HudState::Recording {
                started_at_ms: crate::hud::now_unix_ms(),
            },
        ) {
            tracing::warn!(error = ?e, "emit hud:state(recording) failed for meeting");
        }
    }
    Ok(session)
}

/// Maximum number of capture sources a single meeting may declare.
/// Today the canonical config is 1 mic + 1 system-audio = 2; the
/// cap is set with headroom for a future per-app SystemAudio (#33)
/// without inviting unbounded per-call resource expansion. Each
/// source spawns an OS-level capture handle; the cost grows
/// linearly.
const MAX_MEETING_SOURCES: usize = 4;

/// Validate + dedup the `sources` list `meeting_start_manual`
/// receives from the frontend. The IPC trusts the caller for
/// well-formed JSON (serde rejects bad shapes already), but
/// nothing today bounds the list's size or rejects duplicates —
/// a buggy frontend could send `[SystemAudio, SystemAudio, …]`
/// and have each entry open an independent SCStream / cpal
/// stream, doubling memory + CPU per duplicate. Cap + dedup at
/// the IPC boundary so the manager never has to consider that
/// case.
///
/// Microphone duplicates dedup by device id — `[Mic("a"), Mic("a"),
/// Mic("b")]` collapses to `[Mic("a"), Mic("b")]`. SystemAudio is
/// keyed by its variant alone (there's only one system-audio
/// stream per host).
/// Single-shot snapshot of the active window's app name + title at
/// IPC-entry time, for #242's browser-title surfacing. Returns
/// `(None, None)` when `active-win-pos-rs` errors (lock screen,
/// fullscreen game, no permission to introspect) — manual sessions
/// continue to work without metadata in that case.
///
/// Title is normalised to `None` when empty/whitespace so the panel
/// can render purely on truthiness — many platforms return empty
/// strings rather than absent titles for chrome-less windows
/// (browser pop-ups, PIP video players, etc.).
fn capture_foreground_app() -> (Option<String>, Option<String>) {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => {
            let title = w.title.trim();
            let title_opt = if title.is_empty() {
                None
            } else {
                Some(title.to_owned())
            };
            (Some(w.app_name), title_opt)
        }
        Err(_) => (None, None),
    }
}

fn sanitise_meeting_sources(sources: Vec<AudioSource>) -> Result<Vec<AudioSource>, String> {
    if sources.is_empty() {
        return Err("at least one audio source is required".to_owned());
    }
    if sources.len() > MAX_MEETING_SOURCES {
        return Err(format!(
            "too many audio sources ({}): max is {}",
            sources.len(),
            MAX_MEETING_SOURCES
        ));
    }
    let mut seen_system = false;
    let mut seen_mics: Vec<&str> = Vec::new();
    let mut deduped: Vec<AudioSource> = Vec::with_capacity(sources.len());
    for source in &sources {
        match source {
            AudioSource::Microphone(device_id) => {
                let key = device_id.as_deref().unwrap_or("__default_mic__");
                if seen_mics.contains(&key) {
                    continue;
                }
                seen_mics.push(key);
                deduped.push(source.clone());
            }
            AudioSource::SystemAudio => {
                if seen_system {
                    continue;
                }
                seen_system = true;
                deduped.push(source.clone());
            }
        }
    }
    Ok(deduped)
}

/// Close the active meeting session.
///
/// Errors with `IpcError::MeetingSessions` if no session is active.
/// The panel disables the Stop button when nothing's running, but a
/// stale double-click reaches here as a recoverable error rather
/// than a panic.
///
/// After a successful stop the transcribers AND the diarizer are
/// rebuilt in the background (#636). Old `WhisperContext` compute
/// buffers (KV cache, mel scratch, beam scratch) and the old ORT
/// `Session` stay live in their slots until the new contexts are
/// ready (~1 s for transcribers, ~80 ms for the diarizer); the
/// background task then atomically installs each new context,
/// dropping the old `Arc`s so the C++ destructors can release the
/// buffers.
///
/// Keeping old contexts live during the rebuild window means a
/// rapid stop-then-start sequence always finds a valid transcriber
/// AND a valid diarizer in their slots. The new `OnnxDiarizer`
/// instance has its own fresh `SessionClusterState` so speaker
/// labels do not bleed across meeting boundaries — the reset
/// happens at swap-time, not at null-time.
///
/// Extracted from the IPC command body so the CoreAudio auto-stop
/// path in `run_meeting_detection_task` can share the same logic.
pub(crate) async fn stop_meeting_and_rebuild_transcriber(
    app: &AppHandle,
    state: &AppState,
) -> IpcResult<()> {
    // Hide the HUD up front — the user (or auto-stop) expects the
    // overlay gone now, not after the pump's final-chunk drain
    // (which can take several seconds while whisper finishes the
    // tail of the session). `hide_async` dispatches onto the main
    // thread; same rationale as the start path (#476).
    crate::hud::hide_async(app);

    // Check *before* calling stop_manual so we know whether the pump
    // was involved. We need this to distinguish two error cases:
    //
    //   (a) "no meeting session active" — no pump was ever running,
    //       transcribe slots are still live for dictation → skip cleanup.
    //   (b) DB close failed after pump was already joined — pump is
    //       gone, contexts are safe to drop → run cleanup anyway.
    let had_active = state.meeting_manager.has_active_session();

    let stop_result = state
        .meeting_manager
        .stop_manual()
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("stop_manual: {e:#}")));

    if had_active {
        // The pump task has been joined (or this is a DB-close retry
        // where it was already joined previously). All Arc<dyn Transcribe>
        // clones held inside WhisperStreamingSession have been dropped.
        //
        // Rebuild both the transcribers AND the diarizer in the background.
        // The old Arcs stay live in their slots while the rebuild runs
        // (~1 s transcribers, ~80 ms diarizer), so a rapid stop-then-start
        // always finds a valid transcriber + diarizer in their slots and
        // doesn't run idle / fall back to source labels for the new
        // session. Only overwrite a slot when the rebuild returns Some
        // (transcriber) or Ok (diarizer) — a failure is logged at error!
        // and the existing (possibly high-watermarked) context stays live
        // rather than leaving dictation/meetings broken until restart.
        //
        // SessionClusterState reset: a fresh `OnnxDiarizer` instance
        // built by `swap_diarizer_after_download` has its own empty
        // `SessionClusterState`, so installing it via the atomic write
        // implicitly resets the speaker-label namespace — no bleed
        // across meeting boundaries. The reset happens at swap-time,
        // not at null-time, which is why we no longer need the
        // synchronous `*slot = NoopDiarizer` step that earlier
        // iterations of this PR included.
        //
        // Old WhisperContext compute buffers (KV cache, mel scratch, beam
        // scratch) are freed when the new Arcs replace the old ones and
        // their refcounts hit zero (#636).
        //
        // TODO(#636): a concurrent model_select that completes *during*
        // this reload can be overwritten when the reload task finishes.
        // Low-risk today (user rarely changes model right after a meeting
        // stop); address with a generation counter if it surfaces.
        let settings_bg = Arc::clone(&state.settings);
        let models_dir_bg = state.models_dir.clone();
        let inference_threads_bg = Arc::clone(&state.runtime_flags.inference_threads);
        let mic_gain_db_bg = Arc::clone(&state.runtime_flags.mic_gain_db);
        let transcribe_slot = Arc::clone(&state.transcribe);
        let transcribe_meeting_slot = Arc::clone(&state.transcribe_meeting);
        #[cfg(feature = "diarization-onnx")]
        let diarize_slot_bg = Arc::clone(&state.diarize_slot);

        tauri::async_runtime::spawn(async move {
            let (dictation, meeting) = tokio::join!(
                crate::ipc::pipeline::build_transcriber(
                    &settings_bg,
                    &models_dir_bg,
                    &inference_threads_bg,
                    &mic_gain_db_bg,
                ),
                crate::ipc::pipeline::build_transcriber(
                    &settings_bg,
                    &models_dir_bg,
                    &inference_threads_bg,
                    &mic_gain_db_bg,
                ),
            );
            // Only install a new context when the rebuild succeeded.
            // Writing None would leave dictation broken until the next
            // model selection; keeping the high-watermarked context live
            // is preferable to a silent outage.
            if dictation.is_none() {
                tracing::error!(
                    "meeting stop: dictation transcriber rebuild returned None; \
                     dictation will keep using the previous context until the \
                     next model selection"
                );
            } else if let Ok(mut g) = transcribe_slot.lock() {
                *g = dictation;
            }
            if meeting.is_none() {
                tracing::error!(
                    "meeting stop: meeting transcriber rebuild returned None; \
                     next meeting will keep using the previous context until the \
                     next model selection"
                );
            } else if let Ok(mut g) = transcribe_meeting_slot.lock() {
                *g = meeting;
            }

            #[cfg(feature = "diarization-onnx")]
            {
                use crate::diarization::catalog::WESPEAKER_RESNET34_LM_FILENAME;
                let model_path = models_dir_bg.join(WESPEAKER_RESNET34_LM_FILENAME);
                if model_path.exists() {
                    // `diarize_slot` holds the inner diarizer directly;
                    // FlagGatedDiarizer wraps the slot in AppState and reads
                    // through it on every call (not a snapshot at session start).
                    let slot = Arc::clone(&diarize_slot_bg);
                    match tokio::task::spawn_blocking(move || {
                        crate::ipc::commands::diarizer::swap_diarizer_after_download(
                            &slot,
                            &model_path,
                        )
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            tracing::info!("diarizer reloaded after meeting stop");
                        }
                        Ok(Err(e)) => {
                            tracing::error!(
                                error = ?e,
                                "meeting stop: diarizer reload failed; \
                                 speaker identification unavailable until next model selection"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                error = ?e,
                                "meeting stop: diarizer reload task panicked; \
                                 speaker identification unavailable until next model selection"
                            );
                        }
                    }
                }
            }
        });
    }

    stop_result
}

#[tauri::command]
pub async fn meeting_stop_manual(app: AppHandle, state: State<'_, AppState>) -> IpcResult<()> {
    stop_meeting_and_rebuild_transcriber(&app, &state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The IPC boundary trusts the frontend for well-formed JSON
    // (serde catches bad shapes) but nothing else bounds the list
    // size or rejects duplicates. These tests pin the validation
    // behaviour so a buggy frontend can't open N independent
    // SCStream instances by sending the same source N times.

    #[test]
    fn sanitise_rejects_empty_source_list() {
        let err = sanitise_meeting_sources(Vec::new()).expect_err("empty list must error");
        assert!(
            err.contains("at least one"),
            "error must explain the precondition; got: {err}"
        );
    }

    #[test]
    fn sanitise_rejects_oversize_source_list() {
        // MAX_MEETING_SOURCES + 1 entries — should error.
        let too_many: Vec<AudioSource> = (0..=MAX_MEETING_SOURCES)
            .map(|i| AudioSource::Microphone(Some(format!("mic-{i}"))))
            .collect();
        let err = sanitise_meeting_sources(too_many).expect_err("oversize list must error");
        assert!(
            err.contains("too many audio sources"),
            "error must name the precondition; got: {err}"
        );
    }

    #[test]
    fn sanitise_dedups_microphone_by_device_id() {
        let dupes = vec![
            AudioSource::Microphone(Some("Built-in".into())),
            AudioSource::Microphone(Some("Built-in".into())),
            AudioSource::Microphone(Some("USB-C".into())),
        ];
        let cleaned = sanitise_meeting_sources(dupes).unwrap();
        assert_eq!(cleaned.len(), 2);
        assert!(matches!(
            &cleaned[0],
            AudioSource::Microphone(Some(s)) if s == "Built-in"
        ));
        assert!(matches!(
            &cleaned[1],
            AudioSource::Microphone(Some(s)) if s == "USB-C"
        ));
    }

    #[test]
    fn sanitise_dedups_default_mic_against_itself() {
        // `Microphone(None)` means "host default mic" — two of them
        // are the same source.
        let dupes = vec![AudioSource::Microphone(None), AudioSource::Microphone(None)];
        let cleaned = sanitise_meeting_sources(dupes).unwrap();
        assert_eq!(cleaned.len(), 1);
    }

    #[test]
    fn sanitise_dedups_system_audio_against_itself() {
        let dupes = vec![AudioSource::SystemAudio, AudioSource::SystemAudio];
        let cleaned = sanitise_meeting_sources(dupes).unwrap();
        assert_eq!(cleaned.len(), 1);
        assert!(matches!(cleaned[0], AudioSource::SystemAudio));
    }

    #[test]
    fn sanitise_keeps_distinct_kinds_in_input_order() {
        // Mic + system audio is the canonical meeting config — the
        // dedup must preserve both, in the order the frontend sent
        // them, so the pump's source-tagging is deterministic.
        let mixed = vec![
            AudioSource::Microphone(Some("Built-in".into())),
            AudioSource::SystemAudio,
        ];
        let cleaned = sanitise_meeting_sources(mixed.clone()).unwrap();
        assert_eq!(cleaned, mixed);
    }

    // -- meeting export (#357 phase 3b) ------------------------------
    // Tests live in `meeting::export` (the module that owns the code).
}
