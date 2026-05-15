//! Bulk "Export filtered" IPC for the unified History feed (#357
//! phase 3c).
//!
//! Per-row export already lives next to its domain commands —
//! [`super::history::history_export_row_csv`] for dictation,
//! [`super::meeting::meeting_session_export`] for meetings. The
//! bulk path needs both, plus the same FTS query the panel is
//! showing, plus the user's chosen format/kind/dir options. It
//! writes one file per session/entry into a directory the user
//! picked via `tauri-plugin-dialog`'s `open({ directory: true })`
//! — same trust shape as the per-row path: dialog plugin
//! resolves the path, this IPC writes the bytes.
//!
//! The IPC is filter-scoped: it pulls dictation rows via
//! `history_search` and meeting sessions via
//! `meeting_sessions_search`, both passed the same `query` string
//! the panel had at click-time. So "Export filtered" honours the
//! current search + filter chip without the frontend having to
//! pass the full row list across the IPC boundary.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tauri::State;

use crate::ipc::AppState;
use crate::meeting::export::{
    meeting_session_csv, meeting_session_json, meeting_session_text, MeetingExportFormat,
};

use super::history::history_csv_for_entries;
use super::{IpcError, IpcResult};

/// Which kinds of rows the bulk export covers (#357 phase 3c).
/// Tagged lowercase to match the frontend literal tokens.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportKind {
    /// Dictation rows + meeting sessions interleaved.
    Both,
    /// Dictation rows only.
    Dictation,
    /// Meeting sessions only.
    Meetings,
}

/// Options carried from the export-options dialog.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportBundleOptions {
    /// Optional FTS query — same string the search box has at
    /// click-time. Empty / whitespace-only → no filter, full lists
    /// returned. Mirrors the fallback shape `history_search` and
    /// `meeting_sessions_search` use.
    pub query: Option<String>,
    /// Which streams to include.
    pub kind: ExportKind,
    /// Format for the meeting files. Dictation always exports as
    /// CSV (matches the per-row path) — meetings have three
    /// formats; the user picks once for the whole bundle.
    pub meeting_format: MeetingExportFormat,
}

/// Result returned to the frontend so it can render a "Wrote N
/// files to <dir>" toast without having to count the rows itself.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportBundleResult {
    /// Directory the bundle landed in — same path the user picked,
    /// echoed back so the frontend's toast can reference it
    /// without keeping its own state.
    pub directory: String,
    /// Number of files written. Zero is a legitimate result (an
    /// empty filter); the frontend renders "No rows matched the
    /// current filter" rather than an error in that case.
    pub written: i64,
}

/// Export the current filter+search slice of the History feed to
/// the user-picked directory (#357 phase 3c). One file per
/// session/dictation entry. Filenames follow:
///
/// - Dictation: `dictation-<id>.csv`
/// - Meeting: `meeting-<id>.txt|.csv|.json` (extension matches
///   `meeting_format`)
///
/// Collision safe: the row id is unique within its table, so two
/// dictation rows with overlapping created_at don't clash. The
/// `dictation-` / `meeting-` prefix disambiguates across tables.
///
/// Cancellation by the dialog plugin is handled frontend-side —
/// this IPC only fires when the user has already picked a
/// directory.
#[tauri::command]
pub async fn history_export_bundle(
    state: State<'_, AppState>,
    options: ExportBundleOptions,
    directory: String,
) -> IpcResult<ExportBundleResult> {
    let dir = PathBuf::from(&directory);
    if !dir.is_dir() {
        return Err(IpcError::Internal(format!(
            "{directory}: not a directory or not accessible"
        )));
    }

    let trimmed_query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut written: i64 = 0;

    if options.kind == ExportKind::Both || options.kind == ExportKind::Dictation {
        // Pull every matching dictation row via the export-specific
        // method that bypasses the pagination cap (#858).
        let entries = state
            .data
            .history
            .list_all_for_export(trimmed_query)
            .await
            .map_err(|e| IpcError::History(format!("history export: {e:#}")))?;

        for entry in &entries {
            let body = history_csv_for_entries(std::slice::from_ref(entry))
                .map_err(|e| IpcError::Internal(format!("CSV write: {e:#}")))?;
            let path = bundle_path(&dir, &format!("dictation-{}.csv", entry.id))?;
            tokio::fs::write(&path, body)
                .await
                .map_err(|e| IpcError::Internal(format!("write {}: {e}", path.display())))?;
            written += 1;
        }
    }

    if options.kind == ExportKind::Both || options.kind == ExportKind::Meetings {
        let sessions = match trimmed_query {
            Some(q) => state
                .data
                .meetings
                .search_sessions(q)
                .await
                .map_err(|e| IpcError::MeetingSessions(format!("sessions search: {e:#}")))?,
            None => state
                .data
                .meetings
                .list()
                .await
                .map_err(|e| IpcError::MeetingSessions(format!("sessions list: {e:#}")))?,
        };

        let ext = match options.meeting_format {
            MeetingExportFormat::Text => "txt",
            MeetingExportFormat::Csv => "csv",
            MeetingExportFormat::Json => "json",
        };

        for session in &sessions {
            let utterances = state
                .data
                .meetings
                .list_utterances(session.id)
                .await
                .map_err(|e| IpcError::MeetingSessions(format!("session utterances: {e:#}")))?;
            let body = match options.meeting_format {
                MeetingExportFormat::Text => meeting_session_text(session, &utterances),
                MeetingExportFormat::Csv => meeting_session_csv(session, &utterances)
                    .map_err(|e| IpcError::Internal(format!("CSV write: {e:#}")))?,
                MeetingExportFormat::Json => meeting_session_json(session, &utterances)
                    .map_err(|e| IpcError::Internal(format!("JSON write: {e:#}")))?,
            };
            let path = bundle_path(&dir, &format!("meeting-{}.{}", session.id, ext))?;
            tokio::fs::write(&path, body)
                .await
                .map_err(|e| IpcError::Internal(format!("write {}: {e}", path.display())))?;
            written += 1;
        }
    }

    Ok(ExportBundleResult { directory, written })
}

/// Compose a path inside `dir` from a leaf filename, sanity-
/// checking that the leaf doesn't try to escape (no `/`, no
/// `..`). The frontend builds these from row ids so we trust
/// the shape — but a release-build defence-in-depth check
/// keeps a future caller (or a malicious frontend) from writing
/// outside the chosen directory by sneaking `../` into the
/// filename. Pre-#499 this was a `debug_assert!`, which compiled
/// out in release; the comment claimed a guarantee the binary
/// didn't enforce.
fn bundle_path(dir: &Path, leaf: &str) -> IpcResult<PathBuf> {
    if leaf.contains('/') || leaf.contains('\\') || leaf.contains("..") {
        return Err(IpcError::Internal(format!(
            "unsafe bundle leaf rejected: {leaf:?}"
        )));
    }
    Ok(dir.join(leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_path_accepts_simple_filename_components() {
        let dir = Path::new("/tmp/x");
        // Production-shaped leaves used by the two callers.
        assert!(bundle_path(dir, "dictation-42.csv").is_ok());
        assert!(bundle_path(dir, "meeting-42.json").is_ok());
        assert!(bundle_path(dir, "anything-without-separators.txt").is_ok());
    }

    #[test]
    fn bundle_path_rejects_path_separators_and_traversal() {
        let dir = Path::new("/tmp/x");
        // Forward slash — POSIX path separator.
        assert!(bundle_path(dir, "../etc/passwd").is_err());
        assert!(bundle_path(dir, "subdir/leaf.csv").is_err());
        // Backslash — Windows path separator. Hush's hands-on
        // target is macOS but the export IPC compiles on Windows,
        // and a `\..\` leaf would equally escape there.
        assert!(bundle_path(dir, r"subdir\leaf.csv").is_err());
        // Plain `..` without a separator still escapes via
        // `dir.join("..")`. Reject it too — the frontend has no
        // legitimate use for it as a leaf.
        assert!(bundle_path(dir, "..").is_err());
        // Embedded `..` inside an otherwise innocent leaf.
        assert!(bundle_path(dir, "data..csv").is_err());
    }
}
