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

#[cfg(test)]
mod tests {
    use crate::debug_log::DebugLogState;
    use crate::ipc::tests::mock_state;

    #[test]
    fn debug_log_snapshot_is_empty_on_fresh_state() {
        // `mock_state()` constructs `DebugLogState::new()` with no entries.
        // `get_log_entries` calls `state.debug_log.snapshot()` — this pins
        // the contract: a freshly-built AppState has no log entries.
        let state = mock_state();
        assert!(
            state.debug_log.snapshot().is_empty(),
            "fresh AppState must have an empty debug log"
        );
    }

    #[test]
    fn debug_log_state_snapshot_is_idempotent() {
        // Drive `DebugLogState` directly (no Tauri runtime needed).
        // Repeated snapshots on an unmodified state must be identical.
        let state = DebugLogState::new();
        let first = state.snapshot();
        let second = state.snapshot();
        assert_eq!(
            first.len(),
            second.len(),
            "snapshot must be deterministic for an unmodified state"
        );
        assert!(first.is_empty(), "new DebugLogState must have no entries");
    }
}
