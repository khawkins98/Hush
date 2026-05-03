//! Diarizer model status / install / remove IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the wespeaker
//! download lifecycle (#301 / #351) sits in a peer file the way
//! `meeting.rs`, `models.rs`, and `dictionary.rs` already do. No
//! behaviour change — pure code-move.
//!
//! Three Tauri events fan out the lifecycle, namespaced under the
//! existing `model:` prefix the Whisper picker uses:
//! - `model:download-progress` — `{ id, bytesReceived, bytesTotal }`
//! - `model:download-done` — `{ id, message: null }`
//! - `model:download-failed` — `{ id, message }`
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::diarizer::download_diarizer_model`, etc.).
//! `pub use` re-exports do not carry the macro's hidden
//! `__cmd__<name>` symbol — see `learnings.md` 2026-04-25.

use tauri::State;

use super::super::AppState;
use super::{poisoned, IpcError, IpcResult};

/// Status of the diarizer model file (#301). The Settings →
/// Speakers panel reads this on mount + after every download
/// progress event so the UI can render "model not installed",
/// "downloading", or "ready" states accurately.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizeModelStatus {
    /// Whether the wespeaker `.onnx` file is present in
    /// `models_dir`. Frontend uses this to grey out the toggle and
    /// show the download affordance when `false`.
    pub downloaded: bool,
    /// Catalog display name ("wespeaker ResNet34-LM"). Lifted into
    /// the status (#351) so the panel can show *which* model is
    /// installed without duplicating the catalog on the frontend.
    pub display_name: String,
    /// Catalog-declared on-disk size (~26 MB). Surfaced in the UI
    /// so the user knows what they're committing to before
    /// clicking Download.
    pub size_mb: u32,
    /// Catalog-declared SHA-256 (hex). Returned alongside the
    /// status so the UI can show a "verified file" indicator
    /// post-download. Not user-facing per se, but useful for
    /// support / troubleshooting.
    pub sha256: String,
    /// Absolute path the user can copy-and-cd-into to drop the
    /// file manually if they prefer (or to verify the download
    /// landed where expected). Mirrors the same affordance as the
    /// Whisper picker.
    pub expected_path: String,
    /// Upstream URL the model was downloaded from. Linked from the
    /// Speakers panel so a user who wants to read the model card
    /// can click through (#351).
    pub source_url: String,
}

/// Read the diarizer model's status (#301). Cheap — single
/// filesystem stat. Called by Settings → Speakers on mount and
/// after each `model:download-done` / `model:download-failed`
/// Tauri event.
#[tauri::command]
pub fn get_diarizer_model_status(state: State<'_, AppState>) -> IpcResult<DiarizeModelStatus> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let path = state.models_dir.join(&model.filename);
    Ok(DiarizeModelStatus {
        downloaded: path.exists(),
        display_name: model.display_name,
        size_mb: model.size_mb,
        sha256: model.sha256,
        expected_path: path.to_string_lossy().into_owned(),
        source_url: model.download_url,
    })
}

/// Remove the installed wespeaker model and revert the diarizer
/// slot to NoopDiarizer (#351). The slot swap is the in-process
/// inverse of `download_diarizer_model`'s `swap_diarizer_after_download`
/// — the next meeting pump tick reads the new slot and stops
/// labelling utterances. Persists `diarization_enabled = false` so
/// the toggle in Settings reflects the new state and a future
/// re-install lands in a clean configuration.
///
/// No-op if the file isn't present (the user already removed it
/// out-of-band, or a parallel `remove` raced to completion). The
/// slot swap still runs so the in-memory state stays consistent
/// with the filesystem regardless of how the file disappeared.
#[tauri::command]
pub async fn remove_diarizer_model(state: State<'_, AppState>) -> IpcResult<()> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let path = state.models_dir.join(&model.filename);

    // Best-effort delete: a missing file is fine (idempotent), but
    // any other error (permission, IO failure) surfaces so the
    // user sees something rather than a silent partial state.
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(IpcError::Internal(format!(
                "remove diarizer model {}: {e}",
                path.display()
            )));
        }
    }

    // Revert the slot to a Noop. Mirror the recovery shape
    // `swap_diarizer_after_download` uses for write lock acquisition
    // — a transient panic shouldn't poison the slot for the rest
    // of the session. The guard is scoped to a block so it's
    // proven-dropped before the next await; otherwise the macro-
    // generated future fails to satisfy `Send`.
    {
        let mut slot = state
            .diarize_slot
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *slot = std::sync::Arc::new(crate::diarization::NoopDiarizer);
    }

    // Turn the toggle off in the persisted settings so the panel's
    // next read shows a consistent "no model + toggle off" state.
    // Errors here are non-fatal: the in-memory slot already swapped,
    // and a misaligned toggle setting is a UX papercut, not a
    // broken state.
    state
        .runtime_flags
        .diarization_enabled
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if let Err(e) = state
        .settings
        .set(crate::settings::keys::DIARIZATION_ENABLED, "false")
        .await
    {
        tracing::warn!(error = %e, "remove_diarizer_model: persist toggle=false failed");
    }

    Ok(())
}

/// Begin downloading the wespeaker speaker-embedding model (#301).
/// Mirrors the `model_download` shape: returns immediately, the
/// download runs on a tokio task, progress is reported via Tauri
/// events. After a successful download we hot-swap the diarizer
/// slot so the new `OnnxDiarizer` takes effect on the next meeting
/// tick — no app restart needed.
///
/// `id` is always `"wespeaker-resnet34-lm"` for the diarizer
/// (matches `catalog::WESPEAKER_RESNET34_LM_ID`).
///
/// Implementation delegates to [`download_diarizer_model_inner`] —
/// the inner takes an [`crate::events::EventEmitter`] trait
/// instead of an `AppHandle` so tests can drive both the rejection
/// path and the failure-cleanup path without spinning up a real
/// Tauri runtime (#315).
#[tauri::command]
pub async fn download_diarizer_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<()> {
    let model = crate::diarization::catalog::default_diarizer_model();
    let emitter: std::sync::Arc<dyn crate::events::EventEmitter> =
        std::sync::Arc::new(crate::ipc::events::TauriEventEmitter::new(app));
    download_diarizer_model_inner(
        DiarizerDownloadDeps {
            emitter,
            downloads: std::sync::Arc::clone(&state.downloads),
            http: state.http.clone(),
            diarize_slot: std::sync::Arc::clone(&state.diarize_slot),
            models_dir: state.models_dir.clone(),
        },
        model,
    )
    .await
}

/// Bundled dependencies the diarizer download needs at runtime.
/// Pulled out of [`AppState`] so [`download_diarizer_model_inner`]
/// can run from a `#[tokio::test]` without a real `AppHandle` /
/// `tauri::State` (#315).
pub(crate) struct DiarizerDownloadDeps {
    pub emitter: std::sync::Arc<dyn crate::events::EventEmitter>,
    pub downloads: std::sync::Arc<
        std::sync::Mutex<
            std::collections::HashMap<String, crate::transcription::download::CancelHandle>,
        >,
    >,
    pub http: reqwest::Client,
    pub diarize_slot: crate::diarization::DiarizeSlot,
    pub models_dir: std::path::PathBuf,
}

/// Inner implementation of [`download_diarizer_model`]. Same
/// behaviour as the `#[tauri::command]` wrapper, but takes the
/// dependencies as plain values so tests can drive both:
///
/// - the duplicate-rejection guard inside the
///   `state.downloads.lock()` critical section (audit-2 fix); and
/// - the cancel-handle cleanup on the spawned task's failure
///   branch (mirrors the Whisper-download cleanup pattern).
///
/// `model` is the catalog entry to download. Production passes
/// `crate::diarization::catalog::default_diarizer_model()`; tests
/// can pass a custom entry with a deliberately bad URL to drive
/// the failure path without standing up a fake server.
pub(crate) async fn download_diarizer_model_inner(
    deps: DiarizerDownloadDeps,
    model: crate::diarization::catalog::DiarizerModelMetadata,
) -> IpcResult<()> {
    let id = model.id.clone();
    let dest = deps.models_dir.join(&model.filename);

    // Register a cancel handle + re-check on-disk presence inside
    // the same critical section. Reuses the `downloads` store —
    // same map the Whisper download path uses, keyed by id, so
    // the existing `model_cancel_download` IPC works for the
    // diarizer model with no extra wiring.
    //
    // The exists-check sits inside the lock to close a TOCTOU
    // race (audit-2): two rapid clicks could both pass the
    // exists-check before either took the lock. Holding the lock
    // for the existence test means a concurrent download that
    // just finished is observable as either "file exists now" or
    // "cancel handle still in flight" — caller gets a clean error
    // either way and we never start a duplicate download on top
    // of a freshly-finalized file.
    let cancel = crate::transcription::download::CancelHandle::new();
    {
        let mut guard = deps.downloads.lock().map_err(poisoned)?;
        if dest.exists() {
            return Err(IpcError::Settings(format!(
                "{} is already downloaded",
                model.display_name
            )));
        }
        if guard.contains_key(&id) {
            return Err(IpcError::Settings(format!(
                "{} is already downloading",
                model.display_name
            )));
        }
        guard.insert(id.clone(), cancel.clone());
    }

    let emitter_for_task = std::sync::Arc::clone(&deps.emitter);
    let downloads_for_task = std::sync::Arc::clone(&deps.downloads);
    let id_for_task = id.clone();
    let url = model.download_url.clone();
    let sha = model.sha256.clone();
    let http = deps.http.clone();
    let dest_for_task = dest.clone();
    let diarize_slot = std::sync::Arc::clone(&deps.diarize_slot);

    tauri::async_runtime::spawn(async move {
        let emitter_for_progress = std::sync::Arc::clone(&emitter_for_task);
        let id_for_progress = id_for_task.clone();
        let progress: Box<crate::transcription::download::ProgressCallback> =
            Box::new(move |update| {
                emitter_for_progress.emit(
                    "model:download-progress",
                    &crate::ipc::commands::models::DownloadProgress {
                        id: id_for_progress.clone(),
                        bytes_received: update.bytes_received,
                        bytes_total: update.bytes_total,
                    },
                );
            });

        let result = crate::transcription::download::download_with_progress(
            &http,
            &url,
            &dest_for_task,
            &sha,
            &cancel,
            &progress,
        )
        .await;

        // Drop the cancel handle on the way out, success or
        // failure. Same pattern the Whisper download uses; the
        // shared `downloads` map is the rejection-guard so
        // forgetting to clean up here would silently block
        // subsequent download attempts (audit-2 R-2).
        if let Ok(mut guard) = downloads_for_task.lock() {
            guard.remove(&id_for_task);
        }

        match result {
            Ok(()) => {
                // Hot-swap the diarizer. If OnnxDiarizer::new
                // succeeds, write it into the slot — the next
                // pump tick that runs with diarization_enabled=on
                // will use it.
                //
                // If the load fails (corrupted ONNX, ort init
                // error, feature compiled out), the file is on
                // disk but useless. Pre-audit-2 we emitted
                // `model:download-done` regardless — the UI then
                // showed "installed and verified" while the
                // diarizer was still Noop, leaving the user with
                // a feature that quietly didn't work. Now we
                // delete the bad file (so retry isn't blocked by
                // the `dest.exists()` guard at the top of the
                // function) and emit `model:download-failed` with
                // the load error, so the UI surfaces it the same
                // way as a network or SHA-mismatch failure.
                match swap_diarizer_after_download(&diarize_slot, &dest_for_task) {
                    Ok(()) => {
                        emitter_for_task.emit(
                            "model:download-done",
                            &crate::ipc::commands::models::DownloadStatus {
                                id: id_for_task,
                                message: None,
                            },
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            path = %dest_for_task.display(),
                            "diarizer download succeeded but model load failed; \
                             deleting bad file and emitting download-failed so \
                             retry isn't blocked"
                        );
                        let _ = std::fs::remove_file(&dest_for_task);
                        emitter_for_task.emit(
                            "model:download-failed",
                            &crate::ipc::commands::models::DownloadStatus {
                                id: id_for_task,
                                message: Some(format!("model load failed: {e:#}")),
                            },
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = ?e,
                    model_id = %id_for_task,
                    "diarizer download failed"
                );
                emitter_for_task.emit(
                    "model:download-failed",
                    &crate::ipc::commands::models::DownloadStatus {
                        id: id_for_task,
                        message: Some(format!("{e:#}")),
                    },
                );
            }
        }
    });

    Ok(())
}

/// Build a fresh `OnnxDiarizer` from the just-downloaded file and
/// swap it into the slot. Pulled out as a helper so the inline
/// download closure stays readable + so the cfg-gating around the
/// `diarization-onnx` feature lives in one spot.
pub(crate) fn swap_diarizer_after_download(
    slot: &crate::diarization::DiarizeSlot,
    model_path: &std::path::Path,
) -> anyhow::Result<()> {
    #[cfg(feature = "diarization-onnx")]
    {
        let onnx = crate::diarization::onnx::OnnxDiarizer::new(model_path)?;
        let mut guard = slot
            .write()
            .map_err(|e| anyhow::anyhow!("slot poisoned: {e}"))?;
        *guard = std::sync::Arc::new(onnx);
        Ok(())
    }
    #[cfg(not(feature = "diarization-onnx"))]
    {
        let _ = slot;
        let _ = model_path;
        Err(anyhow::anyhow!(
            "diarization-onnx feature not enabled in this build"
        ))
    }
}
