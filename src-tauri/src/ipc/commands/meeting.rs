//! Meeting Mode IPC commands (Phase C; refs #33 / #109).
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
use tauri::{AppHandle, State};

use crate::audio::AudioSource;
use crate::ipc::AppState;

use super::{IpcError, IpcResult};

/// List all meeting sessions, newest-first. Returns whatever the
/// streaming pump (#122 Phase 2 / #141) has persisted — empty for
/// a fresh install, populated after the user has run a meeting.
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
    let sessions = state
        .data
        .meetings
        .list()
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session get: {e:#}")))?;
    let session = sessions
        .into_iter()
        .find(|s| s.id == id)
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
#[tauri::command]
pub async fn meeting_session_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .meetings
        .delete(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session delete: {e:#}")))
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
/// attributed to. Frontend captures this from the foreground app
/// via `active-win-pos-rs` at the moment of click; if the user
/// declines or the call fails, `None` falls through to a "manual"
/// label.
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
    let session = state
        .meeting_manager
        .start_manual(sources, app_name)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("start_manual: {e:#}")))?;
    // Show the recording HUD so the user has the same at-a-glance
    // "audio is being captured" cue meeting mode that the dictation
    // hot path already provides — best-effort, a HUD-show failure
    // shouldn't fail the start of an otherwise-running session.
    if let Err(e) = crate::hud::show(&app) {
        tracing::error!(error = ?e, "failed to show recording HUD on meeting start");
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
#[tauri::command]
pub async fn meeting_stop_manual(app: AppHandle, state: State<'_, AppState>) -> IpcResult<()> {
    // Hide the HUD up front — the user clicked Stop and expects the
    // overlay gone now, not after the pump's final-chunk drain
    // (which can take several seconds while whisper finishes the
    // tail of the session). Hiding before the await also matches
    // the dictation hot path's order of effects.
    if let Err(e) = crate::hud::hide(&app) {
        tracing::error!(error = ?e, "failed to hide recording HUD on meeting stop");
    }
    state
        .meeting_manager
        .stop_manual()
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("stop_manual: {e:#}")))
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
}
