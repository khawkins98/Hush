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

        // Emit the completion or failure event first, then remove
        // the cancel handle from the map. Order matters: tests
        // (and any future callers) poll the map for handle removal
        // as a "task is fully done" signal; emitting before
        // clearing ensures the event is visible as soon as the
        // handle disappears (no TOCTOU window between "handle
        // gone" and "event posted").
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
                                id: id_for_task.clone(),
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
                                id: id_for_task.clone(),
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
                        id: id_for_task.clone(),
                        message: Some(format!("{e:#}")),
                    },
                );
            }
        }

        // Drop the cancel handle after all notifications are
        // sent. Removing here (not before the match) means any
        // poller that uses handle-absence as a "task fully done"
        // signal already sees the posted event — no observable
        // window between "handle gone" and "event emitted".
        if let Ok(mut guard) = downloads_for_task.lock() {
            guard.remove(&id_for_task);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::AppState;

    // ---- swap_diarizer_after_download (#315) --------------------------------

    /// Sentinel diarizer used by the swap-failure test below.
    /// Different type from the `RecordingDiarizer` in
    /// `diarization::tests` so we can use `Arc::ptr_eq` reliably
    /// to confirm the *exact same* `Arc` survived the failed swap.
    #[cfg(feature = "diarization-onnx")]
    struct SwapSentinelDiarizer;

    #[cfg(feature = "diarization-onnx")]
    impl crate::diarization::Diarize for SwapSentinelDiarizer {
        fn label_utterances(
            &self,
            _utterances: &mut [crate::transcription::Utterance],
            _audio_chunks: &[Vec<f32>],
            _format: crate::audio::CaptureFormat,
        ) {
            // No-op; presence in the slot is the assertion.
        }
    }

    /// When `OnnxDiarizer::new` fails (corrupt / wrong-format file),
    /// `swap_diarizer_after_download` must not touch the slot at all.
    /// A partial write would leave the running meeting pump in an
    /// indeterminate state — it must either succeed atomically or
    /// leave the original diarizer completely intact.
    #[cfg(feature = "diarization-onnx")]
    #[test]
    fn swap_diarizer_after_download_err_leaves_slot_intact() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-wespeaker.onnx");
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(b"definitely not a wespeaker model")
            .expect("write");
        drop(f);

        let sentinel: std::sync::Arc<dyn crate::diarization::Diarize> =
            std::sync::Arc::new(SwapSentinelDiarizer);
        let slot: crate::diarization::DiarizeSlot =
            std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::clone(&sentinel)));

        let res = super::swap_diarizer_after_download(&slot, &path);
        assert!(res.is_err(), "swap should reject a non-wespeaker file");

        let guard = slot.read().expect("slot read");
        assert!(
            std::sync::Arc::ptr_eq(&*guard, &sentinel),
            "swap failure must not replace the slot's Arc"
        );
    }

    // ---- remove_diarizer_model (#351) --------------------------------

    /// Test-side wrapper that calls the IPC body directly without
    /// needing a `tauri::State<'_, AppState>` constructor.
    async fn remove_diarizer_model_test(state: &AppState) -> IpcResult<()> {
        let model = crate::diarization::catalog::default_diarizer_model();
        let path = state.models_dir.join(&model.filename);
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
        {
            let mut slot = state
                .diarize_slot
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *slot = std::sync::Arc::new(crate::diarization::NoopDiarizer);
        }
        state
            .runtime_flags
            .diarization_enabled
            .store(false, std::sync::atomic::Ordering::Relaxed);
        state
            .settings
            .set(crate::settings::keys::DIARIZATION_ENABLED, "false")
            .await
            .map_err(|e| IpcError::Settings(e.to_string()))?;
        Ok(())
    }

    /// Removing when the file isn't present must succeed — covers the
    /// race where two `remove` calls fire or the user deleted the file
    /// out of band before clicking Remove.
    #[tokio::test]
    async fn remove_diarizer_model_is_idempotent_when_file_missing() {
        let state = crate::ipc::tests::mock_state();
        remove_diarizer_model_test(&state)
            .await
            .expect("idempotent on missing file");
    }

    /// After `remove_diarizer_model` the toggle must be cleared in
    /// both the in-memory atomic and the persisted settings row so
    /// the Speakers panel shows a consistent off-by-default state
    /// even after app restart.
    #[tokio::test]
    async fn remove_diarizer_model_persists_toggle_off() {
        let state = crate::ipc::tests::mock_state();
        state
            .runtime_flags
            .diarization_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        state
            .settings
            .set(crate::settings::keys::DIARIZATION_ENABLED, "true")
            .await
            .expect("seed settings");

        remove_diarizer_model_test(&state).await.expect("remove ok");

        assert!(
            !state
                .runtime_flags
                .diarization_enabled
                .load(std::sync::atomic::Ordering::Relaxed),
            "atomic should flip to false"
        );
        let persisted = state
            .settings
            .get(crate::settings::keys::DIARIZATION_ENABLED)
            .await
            .expect("settings get");
        assert_eq!(persisted.as_deref(), Some("false"));
    }

    // ---- download_diarizer_model_inner (#315) --------------------------------

    fn make_test_diarizer_model(url: &str) -> crate::diarization::catalog::DiarizerModelMetadata {
        crate::diarization::catalog::DiarizerModelMetadata {
            id: "wespeaker-test".into(),
            display_name: "Wespeaker (test)".into(),
            filename: "test_diarizer.onnx".into(),
            size_mb: 1,
            description: "test entry".into(),
            download_url: url.into(),
            sha256: "0".repeat(64),
        }
    }

    fn build_download_deps(
        emitter: std::sync::Arc<dyn crate::events::EventEmitter>,
        downloads: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<String, crate::transcription::download::CancelHandle>,
            >,
        >,
        models_dir: std::path::PathBuf,
    ) -> DiarizerDownloadDeps {
        DiarizerDownloadDeps {
            emitter,
            downloads,
            http: reqwest::Client::new(),
            diarize_slot: std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::new(
                crate::diarization::NoopDiarizer,
            ))),
            models_dir,
        }
    }

    /// A second `download_diarizer_model` call while a download is
    /// already in flight must be rejected immediately with
    /// `IpcError::Settings` and must not emit any events — the UI
    /// must not flash spurious progress bars.
    #[tokio::test]
    async fn download_diarizer_model_rejects_duplicate_concurrent_clicks() {
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            crate::transcription::download::CancelHandle,
        >::new()));
        let model = make_test_diarizer_model("http://127.0.0.1:1/never-fetched");
        downloads.lock().unwrap().insert(
            model.id.clone(),
            crate::transcription::download::CancelHandle::new(),
        );

        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn crate::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        let result = download_diarizer_model_inner(deps, model.clone()).await;
        match result {
            Err(IpcError::Settings(msg)) => {
                assert!(
                    msg.contains("already downloading"),
                    "expected duplicate-rejection message, got: {msg}"
                );
            }
            other => panic!("expected IpcError::Settings, got: {other:?}"),
        }

        assert!(
            recorder.events().is_empty(),
            "duplicate rejection should not emit any events; got {:?}",
            recorder.events()
        );

        let still_present = downloads.lock().unwrap().contains_key(&model.id);
        assert!(still_present, "pre-existing cancel handle was clobbered");
    }

    /// After a network failure the spawned task's cleanup path must
    /// remove the cancel-handle entry from `downloads` so a retry is
    /// not permanently blocked, and must emit exactly one
    /// `model:download-failed` event so the UI can surface the error.
    #[tokio::test]
    async fn download_diarizer_model_clears_cancel_handle_on_failure() {
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            crate::transcription::download::CancelHandle,
        >::new()));
        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn crate::events::EventEmitter> =
            std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let model = make_test_diarizer_model("http://127.0.0.1:1/will-fail");
        let deps = build_download_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        download_diarizer_model_inner(deps, model.clone())
            .await
            .expect("inner returns Ok before the spawn — failure happens inside the task");

        // Poll until the spawned task finishes (connect error surfaces
        // in single-digit ms; 5 s bound guards against CI hiccups).
        let cleared = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !downloads.lock().unwrap().contains_key(&model.id) {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);

        assert!(
            cleared,
            "cancel handle should have been removed by the failure branch"
        );

        let failures = recorder.payloads_for("model:download-failed");
        assert_eq!(
            failures.len(),
            1,
            "exactly one failure event expected; got {failures:?}"
        );
        let payload = &failures[0];
        assert_eq!(payload["id"], serde_json::Value::String(model.id.clone()));
        let msg = payload["message"]
            .as_str()
            .expect("failure event should carry a message string");
        assert!(!msg.is_empty(), "failure event message should be populated");
    }
}
