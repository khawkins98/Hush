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
use tauri::{AppHandle, State};

use crate::audio::AudioSource;
use crate::ipc::AppState;

use super::{IpcError, IpcResult};

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
#[tauri::command]
pub async fn meeting_session_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .meetings
        .delete(id)
        .await
        .map_err(|e| IpcError::MeetingSessions(format!("session delete: {e:#}")))
}

/// Export format for a single meeting session (#357 phase 3b).
/// `serde(rename_all = "lowercase")` so the IPC accepts the
/// frontend's lowercase format strings (`"text"` / `"csv"` /
/// `"json"`) without an explicit converter.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingExportFormat {
    /// Human-readable, the "send notes to a colleague" format.
    /// Header line + relative-time prefixed utterances.
    Text,
    /// One row per utterance, RFC-4180 escaped via the `csv` crate.
    /// Schema: utterance_id, session_id, started_at_ms, ended_at_ms,
    /// speaker_label, text. Speaker label is the rendered string
    /// (`You` / `Remote` / `Speaker N`) — no raw `mic` / `system`
    /// leakage per the #357 phase 3 acceptance.
    Csv,
    /// Full session metadata + utterance array. The shape mirrors
    /// `MeetingSessionDetail` but with the utterance speaker labels
    /// substituted for their rendered copy, again per the
    /// no-raw-`mic`/`system` rule.
    Json,
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

    tokio::fs::write(&path, body)
        .await
        .map_err(|e| IpcError::Internal(format!("write {path}: {e}")))?;
    Ok(())
}

/// Render the per-row "rendered" speaker label — the same mapping
/// `HistoryMeetingRow.svelte::speakerCopy` uses, kept in lockstep
/// here so exports never leak the raw source-derived `mic` /
/// `system` tokens (#357 phase 3 acceptance).
pub(super) fn rendered_speaker_label(raw: Option<&str>) -> &str {
    match raw {
        Some("mic") => "You",
        Some("system") => "Remote",
        Some(other) => other,
        None => "Unknown",
    }
}

/// Format `started_at_ms` (relative to session start) as
/// `[hh:mm:ss]`. Relative time is what makes the plain-text export
/// readable as "session-internal timeline" rather than a wall-clock
/// log — a meeting at 14:32 doesn't need every line to repeat 14:32.
fn format_relative_timestamp(ms: i64) -> String {
    let total_secs = ms.max(0) / 1000;
    let hours = total_secs / 3_600;
    let minutes = (total_secs % 3_600) / 60;
    let seconds = total_secs % 60;
    format!("[{hours:02}:{minutes:02}:{seconds:02}]")
}

/// Plain-text "send notes to a colleague" format. Header line with
/// session metadata, blank line, utterances one per line with a
/// relative-time prefix and the rendered speaker label. Trailing
/// newline so the file ends cleanly when concatenated.
pub(super) fn meeting_session_text(
    session: &crate::meeting::MeetingSession,
    utterances: &[crate::meeting::PersistedUtterance],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} · started {} · {} utterance{}",
        session.app_name,
        session.started_at,
        session.utterance_count,
        if session.utterance_count == 1 {
            ""
        } else {
            "s"
        }
    );
    if let Some(sources) = &session.sources {
        if !sources.is_empty() {
            let _ = writeln!(out, "Sources: {}", sources.join(" + "));
        }
    }
    if let Some(notes) = &session.notes {
        if !notes.is_empty() {
            let _ = writeln!(out, "Notes: {}", notes);
        }
    }
    out.push('\n');

    for u in utterances {
        let _ = writeln!(
            out,
            "{} {}: {}",
            format_relative_timestamp(u.started_at_ms),
            rendered_speaker_label(u.speaker_label.as_deref()),
            u.text
        );
    }
    out
}

/// CSV with one row per utterance. `csv` crate does the RFC-4180
/// escape (quotes, commas, newlines in transcript text). Speaker
/// label is the rendered copy, not the raw token.
pub(super) fn meeting_session_csv(
    session: &crate::meeting::MeetingSession,
    utterances: &[crate::meeting::PersistedUtterance],
) -> anyhow::Result<String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "utterance_id",
        "session_id",
        "started_at_ms",
        "ended_at_ms",
        "speaker_label",
        "text",
    ])?;
    for u in utterances {
        wtr.write_record(&[
            u.id.to_string(),
            session.id.to_string(),
            u.started_at_ms.to_string(),
            u.ended_at_ms.to_string(),
            rendered_speaker_label(u.speaker_label.as_deref()).to_owned(),
            u.text.clone(),
        ])?;
    }
    let bytes = wtr.into_inner()?;
    Ok(String::from_utf8(bytes)?)
}

/// JSON with the full session metadata + utterance array. Pretty-
/// printed for readability — the file is meant to be human-
/// inspectable, and for programmatic re-import the indentation
/// doesn't change semantics.
pub(super) fn meeting_session_json(
    session: &crate::meeting::MeetingSession,
    utterances: &[crate::meeting::PersistedUtterance],
) -> anyhow::Result<String> {
    // Build a serde_json::Value rather than serializing the raw
    // `MeetingSession` / `PersistedUtterance` types directly, so we
    // can substitute the rendered speaker label without leaking
    // the raw `mic`/`system` token. Keeps the export schema
    // independent of the wire shape — re-arranging the wire shape
    // shouldn't silently re-shape every meeting JSON file the user
    // has on disk.
    let utterances_json: Vec<serde_json::Value> = utterances
        .iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "started_at_ms": u.started_at_ms,
                "ended_at_ms": u.ended_at_ms,
                "speaker_label": rendered_speaker_label(u.speaker_label.as_deref()),
                "text": u.text,
            })
        })
        .collect();

    let envelope = serde_json::json!({
        "session": {
            "id": session.id,
            "app_name": session.app_name,
            "app_kind": session.app_kind,
            "started_at": session.started_at,
            "ended_at": session.ended_at,
            "speaker_count": session.speaker_count,
            "utterance_count": session.utterance_count,
            "notes": session.notes,
            "sources": session.sources,
            "app_title": session.app_title,
        },
        "utterances": utterances_json,
    });
    Ok(serde_json::to_string_pretty(&envelope)?)
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

    // Snapshot the foreground app once at click time. The
    // `active-win-pos-rs` call is single-millisecond synchronous,
    // so we do it here rather than complicate the frontend with a
    // second IPC. Failure is non-fatal — manual sessions still
    // work without metadata, they just render as "manual / Other"
    // until the user adds notes.
    let (probed_app_name, app_title) = capture_foreground_app();
    let resolved_app_name = app_name.or(probed_app_name);

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
#[tauri::command]
pub async fn meeting_stop_manual(app: AppHandle, state: State<'_, AppState>) -> IpcResult<()> {
    // Hide the HUD up front — the user clicked Stop and expects the
    // overlay gone now, not after the pump's final-chunk drain
    // (which can take several seconds while whisper finishes the
    // tail of the session). `hide_async` dispatches onto the main
    // thread; same rationale as the start path (#476).
    crate::hud::hide_async(&app);
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

    // -- meeting export (#357 phase 3b) ------------------------------

    fn sample_session() -> crate::meeting::MeetingSession {
        crate::meeting::MeetingSession {
            id: 7,
            app_name: "Microsoft Teams".to_owned(),
            app_kind: crate::meeting::MeetingAppKind::Meeting,
            started_at: "2026-04-30T12:39:00Z".to_owned(),
            ended_at: Some("2026-04-30T13:39:00Z".to_owned()),
            speaker_count: Some(2),
            utterance_count: 3,
            notes: Some("Action: send recap".to_owned()),
            sources: Some(vec!["mic".to_owned(), "system".to_owned()]),
            app_title: Some("Q3 sync".to_owned()),
        }
    }

    fn sample_utterances() -> Vec<crate::meeting::PersistedUtterance> {
        vec![
            crate::meeting::PersistedUtterance {
                id: 100,
                session_id: 7,
                started_at_ms: 3_000,
                ended_at_ms: 4_500,
                speaker_label: Some("mic".to_owned()),
                text: "Hello everyone, thanks for joining.".to_owned(),
                is_final: true,
            },
            crate::meeting::PersistedUtterance {
                id: 101,
                session_id: 7,
                started_at_ms: 9_200,
                ended_at_ms: 11_000,
                speaker_label: Some("system".to_owned()),
                text: "Hi! Can you share your screen?".to_owned(),
                is_final: true,
            },
            crate::meeting::PersistedUtterance {
                id: 102,
                session_id: 7,
                started_at_ms: 65 * 1000 + 500,
                ended_at_ms: 67 * 1000,
                speaker_label: None,
                text: "Sure, one second.".to_owned(),
                is_final: true,
            },
        ]
    }

    #[test]
    fn rendered_speaker_label_maps_source_tokens_and_passes_others_through() {
        assert_eq!(rendered_speaker_label(Some("mic")), "You");
        assert_eq!(rendered_speaker_label(Some("system")), "Remote");
        assert_eq!(rendered_speaker_label(Some("Speaker 1")), "Speaker 1");
        assert_eq!(rendered_speaker_label(None), "Unknown");
    }

    #[test]
    fn format_relative_timestamp_pads_each_field_to_two_digits() {
        assert_eq!(format_relative_timestamp(0), "[00:00:00]");
        assert_eq!(format_relative_timestamp(3_500), "[00:00:03]");
        assert_eq!(format_relative_timestamp(65 * 1000), "[00:01:05]");
        // Hour rollover — the meetings that need this exist; the
        // 7h-44m example in the issue body is the load-bearing one.
        assert_eq!(
            format_relative_timestamp((7 * 3600 + 44 * 60) * 1000),
            "[07:44:00]"
        );
    }

    #[test]
    fn meeting_session_text_renders_header_and_utterances() {
        let body = meeting_session_text(&sample_session(), &sample_utterances());
        // Header line + sources line + notes line + blank + 3
        // utterance lines + trailing newline. Easier to assert
        // line-by-line than pin exact whitespace.
        let lines: Vec<&str> = body.lines().collect();
        assert!(
            lines[0].starts_with("Microsoft Teams · started 2026-04-30T12:39:00Z"),
            "first line: {:?}",
            lines[0]
        );
        assert!(lines[0].ends_with("3 utterances"));
        assert!(
            lines.contains(&"Sources: mic + system"),
            "missing sources line: {body}"
        );
        assert!(
            lines.contains(&"Notes: Action: send recap"),
            "missing notes line: {body}"
        );
        // Speaker labels rendered, no raw `mic`/`system`.
        assert!(
            lines.iter().any(|l| l.contains("[00:00:03] You: Hello")),
            "expected `You` for mic-source utterance: {body}"
        );
        assert!(
            lines.iter().any(|l| l.contains("[00:00:09] Remote: Hi!")),
            "expected `Remote` for system-source utterance: {body}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("[00:01:05] Unknown: Sure, one second.")),
            "expected `Unknown` for null-speaker utterance: {body}"
        );
    }

    #[test]
    fn meeting_session_csv_renders_one_row_per_utterance_with_rendered_labels() {
        let body = meeting_session_csv(&sample_session(), &sample_utterances()).expect("csv ok");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 4, "header + 3 utterances; got: {body}");
        assert_eq!(
            lines[0],
            "utterance_id,session_id,started_at_ms,ended_at_ms,speaker_label,text"
        );
        // Speaker labels in the CSV match the rendered copy, no
        // raw `mic`/`system`.
        assert!(
            lines[1].contains(",You,"),
            "row 1 should have `You`: {:?}",
            lines[1]
        );
        assert!(
            lines[2].contains(",Remote,"),
            "row 2 should have `Remote`: {:?}",
            lines[2]
        );
        assert!(
            lines[3].contains(",Unknown,"),
            "row 3 should have `Unknown`: {:?}",
            lines[3]
        );
    }

    #[test]
    fn meeting_session_json_substitutes_rendered_labels_in_envelope() {
        let body = meeting_session_json(&sample_session(), &sample_utterances()).expect("json ok");
        let value: serde_json::Value = serde_json::from_str(&body).expect("output parses as JSON");
        let session = value.get("session").expect("session field");
        assert_eq!(session["id"], 7);
        let uts = value.get("utterances").expect("utterances field");
        let arr = uts.as_array().expect("utterances is array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["speaker_label"], "You");
        assert_eq!(arr[1]["speaker_label"], "Remote");
        assert_eq!(arr[2]["speaker_label"], "Unknown");
        // The session metadata's `sources` field is allowed to keep
        // the raw `mic` / `system` labels — that's what was
        // captured. The acceptance criterion is just that
        // utterance speaker labels carry the rendered copy. Verify
        // that explicitly by walking each utterance row.
        for (i, u) in arr.iter().enumerate() {
            let label = u["speaker_label"]
                .as_str()
                .expect("speaker_label is string");
            assert_ne!(
                label, "mic",
                "utterance {i} leaked raw `mic` into speaker_label"
            );
            assert_ne!(
                label, "system",
                "utterance {i} leaked raw `system` into speaker_label"
            );
        }
    }
}
