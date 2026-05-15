//! IPC commands for cross-session speaker identity management (#667).
//!
//! All commands require `speaker_identity_enabled` to be wired but do
//! NOT require it to be true — the management UI (rename/delete/merge)
//! should work even when auto-identification is disabled so users can
//! clean up existing data.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in `src-tauri/src/lib.rs`
//! via its full path (`ipc::commands::speakers::speaker_list`, etc.).
//! `pub use` re-exports do not carry the `__cmd__<name>` symbol — see
//! `learnings.md` 2026-04-25.

use tauri::State;

use crate::speakers::SpeakerIdentity;

use super::super::AppState;
use super::{IpcError, IpcResult};

/// List all known speaker identities.
#[tauri::command]
pub async fn speaker_list(state: State<'_, AppState>) -> IpcResult<Vec<SpeakerIdentity>> {
    state
        .data
        .speakers
        .list()
        .await
        .map_err(|e| IpcError::Internal(format!("{e:#}")))
}

/// Rename a speaker identity. Pass `null` to clear the display name
/// (reverts to auto-assigned provisional label).
#[tauri::command]
pub async fn speaker_rename(
    state: State<'_, AppState>,
    id: i64,
    display_name: Option<String>,
) -> IpcResult<()> {
    state
        .data
        .speakers
        .rename(id, display_name)
        .await
        .map_err(|e| IpcError::Internal(format!("{e:#}")))
}

/// Delete a speaker identity and NULL-out all linked utterances.
#[tauri::command]
pub async fn speaker_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .speakers
        .delete(id)
        .await
        .map_err(|e| IpcError::Internal(format!("{e:#}")))
}

/// Merge two speaker identities: re-link absorb_id's utterances to
/// keep_id, update keep_id's centroid, delete absorb_id.
#[tauri::command]
pub async fn speaker_merge(
    state: State<'_, AppState>,
    keep_id: i64,
    absorb_id: i64,
) -> IpcResult<()> {
    state
        .data
        .speakers
        .merge(keep_id, absorb_id)
        .await
        .map_err(|e| IpcError::Internal(format!("{e:#}")))
}

/// Read the speaker-identity-enabled toggle.
#[tauri::command]
pub fn get_speaker_identity_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .speaker_identity_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the speaker-identity-enabled toggle. Does NOT delete
/// existing identity data — the user can re-enable and keep their
/// accumulated profiles.
#[tauri::command]
pub async fn set_speaker_identity_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> IpcResult<()> {
    state
        .runtime_flags
        .speaker_identity_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::SPEAKER_IDENTITY_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(format!("{e:#}")))
}
