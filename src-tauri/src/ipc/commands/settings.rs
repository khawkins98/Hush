//! Settings get/set IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the per-domain
//! command surface lives in a peer file the way `meeting.rs`,
//! `models.rs`, and `dictionary.rs` already do. No behaviour
//! change — pure code-move so each `RuntimeFlags` field's
//! get/set pair sits in one place.
//!
//! Most setters follow the "optimistic atomic update + persist
//! to settings row" pattern: flip the in-memory `AtomicBool` so
//! the dictation / meeting hot paths see the new value
//! immediately, then write through to the settings table for
//! the next launch. The `*_inner` variants stay `pub(crate)`
//! so the rest of the crate (settings boot path, tests) can
//! call them without a Tauri runtime.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::settings::set_hud_enabled`, etc.). `pub use`
//! re-exports do not carry the macro's hidden `__cmd__<name>`
//! symbol — see `learnings.md` 2026-04-25.

use tauri::State;

use super::super::AppState;
use super::{IpcError, IpcResult};

/// Read the recording-HUD-enabled flag. The Settings → General
/// toggle reads this on mount so the checkbox renders the
/// persisted value rather than always-checked. Defaults to `true`
/// when the row is absent — first-time users benefit from the
/// floating pill that confirms the mic is hot.
#[tauri::command]
pub fn get_hud_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .hud_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the recording-HUD-enabled flag and update the in-memory
/// `AppState` flag the dictation / meeting start paths read.
/// Stored as the literal `"true"` / `"false"` under
/// [`crate::settings::keys::HUD_ENABLED`] so the next launch reads
/// the same value back.
#[tauri::command]
pub async fn set_hud_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_hud_enabled_inner(&state, enabled).await
}

/// Tauri-free orchestration for [`set_hud_enabled`]. Tests exercise
/// this against a `MemSettings`-backed `AppState` rather than a
/// real Tauri runtime.
pub(crate) async fn set_hud_enabled_inner(state: &AppState, enabled: bool) -> IpcResult<()> {
    state
        .runtime_flags
        .hud_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::HUD_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the audio-cues toggle (#292). Settings → General reads
/// this on mount. Default off — opt-in by design (intrusive in
/// shared spaces).
#[tauri::command]
pub fn get_sound_cues_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .sound_cues_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the audio-cues flag + update the AtomicBool. Same
/// shape as `set_hud_enabled`.
#[tauri::command]
pub async fn set_sound_cues_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_sound_cues_enabled_inner(&state, enabled).await
}

pub(crate) async fn set_sound_cues_enabled_inner(state: &AppState, enabled: bool) -> IpcResult<()> {
    state
        .runtime_flags
        .sound_cues_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::SOUND_CUES_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the per-event recording-start cue toggle (#463). Sub-
/// toggle beneath the master `sound_cues_enabled`; defaults to
/// `true` so first-time master-on users hear the start cue.
#[tauri::command]
pub fn get_sound_cue_start_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .sound_cue_start_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

#[tauri::command]
pub async fn set_sound_cue_start_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> IpcResult<()> {
    state
        .runtime_flags
        .sound_cue_start_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::SOUND_CUE_START_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the per-event transcription-complete cue toggle (#463).
#[tauri::command]
pub fn get_sound_cue_complete_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .sound_cue_complete_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

#[tauri::command]
pub async fn set_sound_cue_complete_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> IpcResult<()> {
    state
        .runtime_flags
        .sound_cue_complete_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::SOUND_CUE_COMPLETE_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the diarization-enabled flag (#111). Settings → Meeting reads
/// this on mount so the toggle renders the persisted value. Defaults
/// to `false` when the row is absent — diarization is opt-in until
/// the PR-B model-download path lands.
#[tauri::command]
pub fn get_diarization_enabled(state: State<'_, AppState>) -> IpcResult<bool> {
    Ok(state
        .runtime_flags
        .diarization_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the diarization-enabled flag + update the AtomicBool. Same
/// shape as `set_hud_enabled`. Foundation PR (this one) only flips
/// the flag; the meeting pump's dispatch path will read it once PR-B
/// wires the `OnnxDiarizer` impl.
#[tauri::command]
pub async fn set_diarization_enabled(state: State<'_, AppState>, enabled: bool) -> IpcResult<()> {
    set_diarization_enabled_inner(&state, enabled).await
}

pub(crate) async fn set_diarization_enabled_inner(
    state: &AppState,
    enabled: bool,
) -> IpcResult<()> {
    state
        .runtime_flags
        .diarization_enabled
        .store(enabled, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::DIARIZATION_ENABLED,
            crate::settings::codec::encode_bool(enabled),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the live inference thread count (#255). Settings →
/// General reads this on mount so the slider renders the
/// persisted value rather than the cross-platform default.
#[tauri::command]
pub fn get_inference_threads(state: State<'_, AppState>) -> IpcResult<i32> {
    Ok(state
        .runtime_flags
        .inference_threads
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Persist the inference thread count + update the in-memory
/// atomic the loaded `WhisperTranscription` reads on every
/// inference call. Same pattern as `set_hud_enabled` —
/// optimistically updates the atomic + persists to settings.
/// Clamped to `[MIN_INFERENCE_THREADS, MAX_INFERENCE_THREADS]`
/// (1–16) so a malformed input can't push past whisper.cpp's
/// happy band.
#[tauri::command]
pub async fn set_inference_threads(state: State<'_, AppState>, threads: i32) -> IpcResult<()> {
    set_inference_threads_inner(&state, threads).await
}

pub(crate) async fn set_inference_threads_inner(state: &AppState, threads: i32) -> IpcResult<()> {
    let clamped = threads.clamp(1, 16);
    state
        .runtime_flags
        .inference_threads
        .store(clamped, std::sync::atomic::Ordering::Relaxed);
    state
        .settings
        .set(
            crate::settings::keys::INFERENCE_THREADS,
            &clamped.to_string(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Read the current Meeting-Mode auto-start mode. The Settings
/// → Meeting tab calls this on mount so the dropdown renders the
/// persisted value.
#[tauri::command]
pub fn get_meeting_autostart_mode(
    state: State<'_, AppState>,
) -> IpcResult<crate::meeting::MeetingAutostartMode> {
    Ok(crate::ipc::decode_autostart_mode(
        state
            .runtime_flags
            .meeting_autostart_mode
            .load(std::sync::atomic::Ordering::Relaxed),
    ))
}

/// Persist the Meeting-Mode auto-start mode. Updates both the
/// in-memory atomic the foreground poller reads and the settings
/// row used at next-launch boot, so the value is observable to the
/// poller within the next 3 s tick without an app restart.
#[tauri::command]
pub async fn set_meeting_autostart_mode(
    state: State<'_, AppState>,
    mode: crate::meeting::MeetingAutostartMode,
) -> IpcResult<()> {
    state.runtime_flags.meeting_autostart_mode.store(
        crate::ipc::encode_autostart_mode(mode),
        std::sync::atomic::Ordering::Relaxed,
    );
    state
        .settings
        .set(
            crate::settings::keys::MEETING_AUTOSTART_MODE,
            mode.as_setting(),
        )
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}
