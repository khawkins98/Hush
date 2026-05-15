//! History-browse + dictation-stats IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the history surface
//! lives in a peer file the way `meeting.rs`, `models.rs`,
//! `dictionary.rs`, and `macos.rs` already do. No behaviour
//! change — a pure code-move so each domain's commands sit
//! alongside its trait + repository.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::history::history_list`, etc.). `pub use`
//! re-exports do not carry the macro's hidden `__cmd__<name>`
//! symbol — see `learnings.md` 2026-04-25.

use tauri::State;

use crate::history::HistoryEntry;

use super::super::AppState;
use super::{validate_export_path, IpcError, IpcResult};

/// Paginated list of history rows, newest first.
///
/// `limit` is hard-capped by the repository to a few hundred rows so a
/// misbehaving frontend cannot pull the entire table at once. `offset`
/// is clamped at 0.
#[tauri::command]
pub async fn history_list(
    state: State<'_, AppState>,
    limit: i64,
    offset: i64,
) -> IpcResult<Vec<HistoryEntry>> {
    state
        .data
        .history
        .list(limit, offset)
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// FTS5 search over transcript text. Empty / whitespace-only `query`
/// falls through to the full list, mirroring the UI's "type to filter"
/// pattern.
#[tauri::command]
pub async fn history_search(
    state: State<'_, AppState>,
    query: String,
    limit: i64,
    offset: i64,
) -> IpcResult<Vec<HistoryEntry>> {
    state
        .data
        .history
        .search(&query, limit, offset)
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// Export a single dictation history row as RFC-4180 CSV.
///
/// Two-arg shape: the user picks `path` via
/// `tauri-plugin-dialog`'s `save()` (which is a path picker only —
/// it doesn't write bytes for us), and Rust writes the CSV body
/// directly to that path. The capability for the main window grants
/// `dialog:allow-save` only; the backend handles the actual write
/// so we don't have to wire `tauri-plugin-fs` and broaden the
/// filesystem surface.
///
/// Schema: `id,created_at,duration_ms,app_name,model,transcript`.
/// Omitted: `window_title` (private; not in the export contract
/// from #357 phase 3a).
///
/// Per-row export uses `id` to look up the entry; bulk export
/// (pending) will reuse the same `history_csv_for_entries` helper.
#[tauri::command]
pub async fn history_export_row_csv(
    state: State<'_, AppState>,
    id: i64,
    path: String,
) -> IpcResult<()> {
    validate_export_path(&path)?;
    let entry = state
        .data
        .history
        .get_by_id(id)
        .await
        .map_err(|e| IpcError::History(format!("history get: {e:#}")))?
        .ok_or_else(|| IpcError::History(format!("history row {id} not found")))?;
    let body = history_csv_for_entries(std::slice::from_ref(&entry))
        .map_err(|e| IpcError::Internal(format!("CSV write: {e:#}")))?;
    super::atomic_write(std::path::Path::new(&path), body.as_bytes()).await
}

/// Pure CSV-emit helper. Held outside the IPC entry point so unit
/// tests can call it without an `AppState` around the corner. RFC
/// 4180 escaping handled by the `csv` crate — embedded quotes /
/// newlines / commas are quote-wrapped + double-quoted as needed.
pub(super) fn history_csv_for_entries(entries: &[HistoryEntry]) -> anyhow::Result<String> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "id",
        "created_at",
        "duration_ms",
        "app_name",
        "model",
        "transcript",
    ])?;
    for e in entries {
        wtr.write_record(&[
            e.id.to_string(),
            e.created_at.clone(),
            e.duration_ms.map(|n| n.to_string()).unwrap_or_default(),
            e.app_name.clone().unwrap_or_default(),
            e.model.clone(),
            e.transcript.clone(),
        ])?;
    }
    let bytes = wtr.into_inner()?;
    Ok(String::from_utf8(bytes)?)
}

/// Delete a single history row. No-op (returns Ok) if `id` does not
/// exist — mirrors the trait contract.
#[tauri::command]
pub async fn history_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .history
        .delete(id)
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// Set (or clear) the user-defined short label for a history entry.
/// Passing `null` / `None` removes the label. Blank strings are
/// treated as `None` by the repository.
#[tauri::command]
pub async fn history_set_name(
    state: State<'_, AppState>,
    id: i64,
    name: Option<String>,
) -> IpcResult<()> {
    state
        .data
        .history
        .set_name(id, name)
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// Total row count, for paginators that need "page X of Y".
#[tauri::command]
pub async fn history_count(state: State<'_, AppState>) -> IpcResult<i64> {
    state
        .data
        .history
        .count()
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// Delete every history row. The frontend gates this behind a
/// confirmation prompt — there is no recovery once it lands.
/// Returns the number of rows that were removed so the UI can
/// surface "Cleared N transcripts" feedback. Calling against an
/// empty history is safe and returns `0`.
#[tauri::command]
pub async fn history_clear(state: State<'_, AppState>) -> IpcResult<i64> {
    state
        .data
        .history
        .clear()
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

/// Aggregate stats for the History stats bar (#293). Returns
/// session count, total words, total recording time, and total
/// transcript characters. Empty-history case returns all zeros so
/// the frontend can render a consistent shape.
#[tauri::command]
pub async fn get_dictation_stats(
    state: State<'_, AppState>,
) -> IpcResult<crate::history::DictationStats> {
    state
        .data
        .history
        .get_stats()
        .await
        .map_err(|e| IpcError::History(format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_entry(
        id: i64,
        transcript: &str,
        app: Option<&str>,
        duration: Option<i64>,
    ) -> HistoryEntry {
        HistoryEntry {
            id,
            transcript: transcript.to_owned(),
            app_name: app.map(str::to_owned),
            window_title: None,
            model: "ggml-base.bin".to_owned(),
            duration_ms: duration,
            created_at: "2026-05-01T10:00:00Z".to_owned(),
            ignored: false,
            name: None,
        }
    }

    #[test]
    fn history_csv_for_entries_emits_header_and_one_row_per_entry() {
        let entries = vec![
            history_entry(1, "Hello world", Some("iTerm2"), Some(2_500)),
            history_entry(2, "Second line", None, None),
        ];
        let csv = history_csv_for_entries(&entries).expect("csv ok");
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows; got: {csv:?}");
        assert_eq!(
            lines[0],
            "id,created_at,duration_ms,app_name,model,transcript"
        );
        assert!(lines[1].starts_with("1,2026-05-01T10:00:00Z,2500,iTerm2"));
        // None app / duration render as empty fields, not "null" or "None".
        assert_eq!(
            lines[2], "2,2026-05-01T10:00:00Z,,,ggml-base.bin,Second line",
            "got: {:?}",
            lines[2]
        );
    }

    #[test]
    fn history_csv_escapes_quotes_commas_and_newlines() {
        // RFC-4180 escape rules — embedded quotes get doubled,
        // commas / newlines force quote-wrapping. The csv crate
        // does the heavy lifting; this test pins that we route
        // through it correctly and don't accidentally hand-roll
        // an `escape` somewhere that would double-encode.
        let entries = vec![history_entry(
            7,
            "She said \"hi\", then\nleft.",
            Some("Notes,Inc"),
            None,
        )];
        let csv = history_csv_for_entries(&entries).expect("csv ok");
        // Transcript field: leading + trailing quote, embedded
        // quote doubled, newline preserved inside the quoted field.
        assert!(
            csv.contains("\"She said \"\"hi\"\", then\nleft.\""),
            "transcript should be quote-wrapped with doubled quotes\n{csv}"
        );
        // App-name field: comma triggers quoting too.
        assert!(
            csv.contains("\"Notes,Inc\""),
            "comma in app name should force quoting\n{csv}"
        );
    }
}
