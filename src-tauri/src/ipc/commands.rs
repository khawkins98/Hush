//! Tauri command handlers for the dictation pipeline.
//!
//! Kept thin: each command pulls long-lived services off [`AppState`],
//! delegates orchestration to [`super::run_pipeline`] (which is unit-tested
//! against mocks), and performs the OS side effects (clipboard write,
//! native notification, foreground-app capture) here. Putting the side
//! effects in the command body — rather than abstracting yet another trait
//! — keeps the command surface easy to read at the cost of moving these
//! bits onto the manual smoke-test checklist.

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::AudioDevice;

use super::{run_pipeline, AppState, ForegroundApp};

/// What the frontend gets back from `stop_dictation`.
///
/// The text is what was written to the clipboard. The foreground snapshot
/// is whatever was focused at `start_dictation`; once history persistence
/// lands (TODO(#7)) the frontend will send this through the history insert
/// command rather than displaying it directly.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationResult {
    pub text: String,
    pub foreground: Option<ForegroundApp>,
}

/// Errors returned across the IPC boundary.
///
/// Tauri serialises whatever the command returns; we use a tagged enum so
/// the frontend can switch on `kind` for user-facing copy and recovery
/// hints without parsing free-form `Display` strings.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "kebab-case")]
pub enum IpcError {
    #[error("audio: {0}")]
    Audio(String),

    #[error("transcription: {0}")]
    Transcription(String),

    /// Surfaced when no transcription backend is configured. The recovery
    /// path is "set `HUSH_MODEL_PATH` and rebuild with `--features whisper`"
    /// during the M1/M2 spike; once the model picker (M3) lands this
    /// becomes "open settings and pick a model."
    #[error("transcription not available — set HUSH_MODEL_PATH and build with --features whisper")]
    TranscriptionUnavailable,

    #[error("clipboard: {0}")]
    Clipboard(String),
}

type IpcResult<T> = std::result::Result<T, IpcError>;

/// Enumerate the host's input devices.
///
/// Tauri marshals errors via the `Serialize` impl on [`IpcError`].
#[tauri::command]
pub fn list_input_devices(state: State<'_, AppState>) -> IpcResult<Vec<AudioDevice>> {
    state
        .audio
        .list_input_devices()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Begin capturing from `device_id` (or the system default if `None`).
///
/// Captures the foreground app *before* opening the input stream so the
/// snapshot is taken while the user's intended target window still has
/// focus — by the time the stream is open they may have alt-tabbed back to
/// Hush. The snapshot is held on [`AppState::pending_foreground`] until the
/// matching `stop_dictation` call drains it.
#[tauri::command]
pub fn start_dictation(state: State<'_, AppState>, device_id: Option<String>) -> IpcResult<()> {
    let foreground = capture_foreground();
    *state
        .pending_foreground
        .lock()
        .expect("pending_foreground mutex") = foreground;

    state
        .audio
        .start(device_id.as_deref())
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Stop capturing, transcribe, write to clipboard, fire a notification,
/// and return the text to the frontend.
///
/// Clipboard write is the user's actual artefact; if it fails we surface
/// the error back to the frontend so the user knows the text wasn't
/// pasteable. The notification is courtesy and best-effort: if the
/// platform refuses to fire one (Linux without a notification daemon, for
/// example), we swallow the error and continue.
#[tauri::command]
pub fn stop_dictation(app: AppHandle, state: State<'_, AppState>) -> IpcResult<DictationResult> {
    let transcriber = state
        .transcribe
        .as_ref()
        .ok_or(IpcError::TranscriptionUnavailable)?
        .clone();

    let text = run_pipeline(state.audio.as_ref(), transcriber.as_ref())
        .map_err(|e| classify_pipeline_error(&e))?;

    app.clipboard()
        .write_text(text.clone())
        .map_err(|e| IpcError::Clipboard(e.to_string()))?;

    if let Err(e) = app
        .notification()
        .builder()
        .title("Hush")
        .body("Ready to paste")
        .show()
    {
        tracing::warn!(error = ?e, "failed to fire 'ready to paste' notification");
    }

    let foreground = state
        .pending_foreground
        .lock()
        .expect("pending_foreground mutex")
        .take();

    Ok(DictationResult { text, foreground })
}

/// Best-effort classifier for `run_pipeline` errors. The function returns a
/// boxed `anyhow::Error`; we inspect the chain to pick the right
/// [`IpcError`] variant rather than dumping everything as
/// `IpcError::Transcription`. If neither layer matches, fall back to the
/// transcription bucket — that's the more common origin in practice and
/// the message string still goes through to the frontend either way.
fn classify_pipeline_error(err: &anyhow::Error) -> IpcError {
    let msg = err.to_string();
    // `AudioCapture::stop` errors carry the prefix the cpal backend uses
    // ("no recording in progress", "audio buffer ..."); `Transcribe` errors
    // come from whisper-rs and tend to contain "whisper" or "model".
    if msg.contains("recording") || msg.contains("audio buffer") || msg.contains("device") {
        IpcError::Audio(msg)
    } else {
        IpcError::Transcription(msg)
    }
}

/// Snapshot the current foreground window via `active-win-pos-rs`.
///
/// `active-win-pos-rs` exposes a Result with the unit type as its error,
/// which is not particularly informative. We collapse the failure case to
/// `None` because losing the foreground snapshot is a graceful degradation
/// — the dictation still works, history just won't have the per-app
/// metadata for that row.
fn capture_foreground() -> Option<ForegroundApp> {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => Some(ForegroundApp {
            app_name: w.app_name,
            window_title: w.title,
        }),
        Err(_) => {
            tracing::debug!("active-win-pos-rs returned no active window");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_error_serialises_with_tag_and_message() {
        let json = serde_json::to_string(&IpcError::Audio("device gone".into())).unwrap();
        assert!(json.contains("\"kind\":\"audio\""), "got: {json}");
        assert!(json.contains("\"message\":\"device gone\""), "got: {json}");
    }

    #[test]
    fn ipc_error_unavailable_has_no_message_field() {
        // The unit variant has no payload, so the `content = "message"`
        // attribute should produce just the tag with no `message` key.
        let json = serde_json::to_string(&IpcError::TranscriptionUnavailable).unwrap();
        assert!(
            json.contains("\"kind\":\"transcription-unavailable\""),
            "got: {json}"
        );
        assert!(!json.contains("\"message\""), "got: {json}");
    }

    #[test]
    fn classify_pipeline_error_routes_audio_failures_to_audio_variant() {
        let err = anyhow::anyhow!("no recording in progress");
        match classify_pipeline_error(&err) {
            IpcError::Audio(_) => {}
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn classify_pipeline_error_routes_other_failures_to_transcription() {
        let err = anyhow::anyhow!("whisper model failed to decode");
        match classify_pipeline_error(&err) {
            IpcError::Transcription(_) => {}
            other => panic!("expected Transcription, got {other:?}"),
        }
    }
}
