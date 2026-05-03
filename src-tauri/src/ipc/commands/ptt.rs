//! Push-to-talk configuration IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the PTT config
//! surface lives in a peer file the way `meeting.rs`,
//! `models.rs`, and `dictionary.rs` already do. No behaviour
//! change — pure code-move.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::ptt::ptt_get_config`, etc.). `pub use`
//! re-exports do not carry the macro's hidden `__cmd__<name>`
//! symbol — see `learnings.md` 2026-04-25.

use tauri::{AppHandle, State};

use super::super::AppState;
use super::{IpcError, IpcResult};

/// Configuration the Settings UI reads + writes for push-to-talk.
///
/// `combo` is the canonical `+`-separated key list (`RightMeta`,
/// `RightMeta+RightShift`, etc.). `enabled` mirrors the persisted
/// `ptt_enabled` settings flag. `listenerRunning` is a runtime
/// signal: true when the rdev thread is alive and gated by the
/// `enabled` flag, false when it wasn't started at boot. The UI
/// uses it to show "Restart Hush for Enable to take effect" when
/// the user toggles ON in a session that started with PTT off.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PttConfig {
    pub combo: Vec<String>,
    pub enabled: bool,
    pub listener_running: bool,
}

#[tauri::command]
pub fn ptt_get_config(state: State<'_, AppState>) -> IpcResult<PttConfig> {
    let combo = state
        .ptt_combo
        .read()
        .map_err(|_| IpcError::Internal("ptt_combo lock poisoned".into()))?
        .keys()
        .iter()
        .map(|k| k.as_str().to_string())
        .collect();
    let enabled = state.ptt_active.load(std::sync::atomic::Ordering::SeqCst);
    let listener_running = state
        .ptt_listener_spawned
        .load(std::sync::atomic::Ordering::SeqCst);
    Ok(PttConfig {
        combo,
        enabled,
        // True once the rdev thread is actually up. `ptt_set_config`
        // spawns it on demand the first time the user enables PTT,
        // so this transitions from false → true on first opt-in
        // without an app restart.
        listener_running,
    })
}

/// Update the user's PTT configuration. Combo is hot-swapped via
/// the shared `RwLock` (next keystroke uses the new combo). Enabled
/// is persisted + flipped on the runtime atomic; if the listener
/// wasn't running at boot, a restart is required for the change to
/// take effect (the listener can't be started mid-session because
/// rdev::listen has no clean stop API and starting it now would
/// trigger the OS permission prompt at a surprising moment).
///
/// Validates the combo before persisting — an empty combo or
/// unparseable key name returns `IpcError::Settings` and the
/// existing config is unchanged.
///
/// First-time opt-in: when `enabled` flips from false to true and
/// the rdev listener wasn't spawned at boot, this command starts
/// it on demand via `register_ptt_listener`. On macOS that's the
/// moment the Input Monitoring permission prompt fires — the user
/// has clicked Enable, so the prompt is no longer a surprise.
#[tauri::command]
pub async fn ptt_set_config(
    app: AppHandle,
    state: State<'_, AppState>,
    combo: Vec<String>,
    enabled: bool,
) -> IpcResult<()> {
    // Build + validate the combo first, BEFORE touching state.
    // A bad input shouldn't half-apply (combo persisted, atomic
    // flipped) — validate up front and bail clean.
    let parsed_keys: Result<Vec<crate::hotkey::ptt::PttKey>, _> = combo
        .iter()
        .map(|s| crate::hotkey::ptt::parse_ptt_key(s))
        .collect();
    let parsed_keys =
        parsed_keys.map_err(|e| IpcError::Settings(format!("ptt_set_config: {e:#}")))?;
    let new_combo = crate::hotkey::ptt::PttCombo::try_from_keys(parsed_keys)
        .map_err(|e| IpcError::Settings(format!("ptt_set_config: {e:#}")))?;

    // Persist combo first so a crash between steps leaves the user
    // with their chosen combo on next launch even if the atomic /
    // enabled flip didn't reach the DB.
    state
        .settings
        .set(
            crate::settings::keys::PTT_COMBO,
            &new_combo.to_storage_string(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    state
        .settings
        .set(
            crate::settings::keys::PTT_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;

    // Hot-swap the in-memory state — listener picks both up on the
    // next OS event without restarting.
    {
        let mut guard = state
            .ptt_combo
            .write()
            .map_err(|_| IpcError::Internal("ptt_combo lock poisoned".into()))?;
        *guard = new_combo;
    }
    state
        .ptt_active
        .store(enabled, std::sync::atomic::Ordering::SeqCst);

    // Spawn the rdev listener on demand if this is the first time
    // PTT is being enabled this session. The call is idempotent —
    // a second invocation with the spawned latch already true
    // returns Ok without touching the thread. On macOS, this is
    // the line that triggers the Input Monitoring permission
    // prompt; on the success path the user clicks Enable, sees
    // the prompt, grants, and PTT works without a restart.
    if enabled {
        if let Err(e) = crate::hotkey::ptt::register_ptt_listener(
            &app,
            std::sync::Arc::clone(&state.ptt_combo),
            std::sync::Arc::clone(&state.ptt_active),
            std::sync::Arc::clone(&state.ptt_listener_spawned),
        ) {
            // Best-effort: spawn failure is logged but shouldn't
            // un-persist the user's preference. They can try again
            // (or restart) and the listener will spin up on next
            // launch via lib.rs::setup since `ptt_enabled=true` is
            // already in the DB.
            tracing::error!(error = ?e, "failed to spawn PTT listener on demand");
        }
    }
    Ok(())
}
