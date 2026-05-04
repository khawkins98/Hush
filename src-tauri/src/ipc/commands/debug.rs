//! IPC commands for the in-app debug log console (#532).

use tauri::State;

use super::IpcResult;
use crate::ipc::AppState;

/// Return the current snapshot of the debug log ring buffer.
///
/// The frontend should subscribe to the `"log:event"` Tauri event
/// *before* calling this command, then use the `seq` field to discard
/// any entries already received via the live stream. This prevents the
/// gap between "subscribe" and "get snapshot" from losing events.
#[tauri::command]
pub fn get_log_entries(state: State<'_, AppState>) -> IpcResult<Vec<crate::debug_log::LogEntry>> {
    Ok(state.debug_log.snapshot())
}
