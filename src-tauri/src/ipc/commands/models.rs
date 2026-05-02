//! Model-picker IPC commands (#82 extraction).
//!
//! Five commands span the model lifecycle:
//!
//! - **Read**: [`model_list`] returns one [`ModelCard`] per catalog
//!   entry, decorated with on-disk presence + the user's selection.
//! - **Pick**: [`model_select`] persists the selection and best-
//!   effort hot-loads the GGUF if it's on disk.
//! - **Auto-download**: [`model_download`] / [`model_cancel_download`]
//!   wrap the orchestrator in `transcription::download`, with a
//!   [`CancelHandle`] held in [`AppState::downloads`]. Three Tauri
//!   events fan out the lifecycle — `model:download-progress`,
//!   `model:download-done`, `model:download-failed`.
//! - **Remove**: [`model_remove`] deletes a file from disk and
//!   sweeps the orphan `.part` if one exists.
//!
//! Extracted from `commands/mod.rs` under #82 to give the model
//! domain its own seam — the second-largest cohesive command group
//! after meeting (the first extraction). Models has its own types
//! (`ModelCard`, `ModelSelectResult`, `DownloadProgress`,
//! `DownloadStatus`) and helpers, all moved together.

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::events::EventEmitter;
use crate::ipc::events::TauriEventEmitter;
use crate::ipc::AppState;
use crate::settings::keys as settings_keys;
use crate::transcription::catalog::{self, ModelMetadata};
use crate::transcription::download::{self, CancelHandle};

use super::{poisoned, IpcError, IpcResult};

// -- Model picker --------------------------------------------------------
//
// Static catalog of Whisper variants (see `transcription::catalog`)
// joined with on-disk presence (does the file exist in
// `<app_data>/models/`?) and the user's current selection from
// settings. The frontend renders this as a card grid; selecting a
// card writes the id to settings.

/// Card-friendly view of a model: its catalog metadata plus runtime
/// state the picker UI cares about.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCard {
    /// Static metadata from the catalog (id, name, size, ratings, …).
    #[serde(flatten)]
    pub metadata: ModelMetadata,
    /// Whether the GGUF file is present in `<models_dir>/<filename>`.
    /// `false` cards render greyed-out with a "place this file at …"
    /// hint until auto-download lands.
    pub is_downloaded: bool,
    /// Whether this is the user's currently-selected model — the one
    /// the running transcriber was loaded from. The catalog's default
    /// model gets the badge only when no explicit selection is in
    /// settings.
    pub is_selected: bool,
    /// Absolute path the user can copy-and-cd-into to drop the file.
    /// Surfaced in the picker UI; cheaper than asking the user to
    /// reconstruct the platform app-data path themselves.
    pub expected_path: String,
}

/// Returns one card per catalog entry, decorated with on-disk
/// presence and the user's selection.
#[tauri::command]
pub async fn model_list(state: State<'_, AppState>) -> IpcResult<Vec<ModelCard>> {
    let selected_id = state
        .settings
        .get(settings_keys::SELECTED_MODEL_ID)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;

    // Treat "no selection in settings" as "the catalog's default is
    // implicitly selected". Matches the picker's first-run mental
    // model where `Whisper Base` shows the Default badge until the
    // user explicitly picks something else. `default_id` outlives the
    // map below so the `&str` borrow is sound.
    let default_id = catalog::default_model().id;
    let effective_selection: &str = selected_id.as_deref().unwrap_or(default_id.as_str());

    let cards = catalog::whisper_models()
        .into_iter()
        .map(|metadata| {
            let path = state.models_dir.join(&metadata.filename);
            let is_downloaded = path.exists();
            let is_selected = metadata.id == effective_selection;
            ModelCard {
                expected_path: path.to_string_lossy().into_owned(),
                metadata,
                is_downloaded,
                is_selected,
            }
        })
        .collect();
    Ok(cards)
}

/// Result returned to the frontend by [`model_select`]. The frontend
/// uses `loaded` to decide whether to show "Loaded — ready to record"
/// (true) or "Saved as default — Download this model to use it"
/// (false).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelectResult {
    /// Whether the transcriber was successfully hot-swapped to the
    /// newly-selected model. `false` when the model file isn't on
    /// disk yet (user picked an undownloaded model — selection still
    /// persists, but they'll need to Download before they can record).
    pub loaded: bool,
}

/// Persist the user's choice and hot-load the new model if its file
/// is on disk. Hot-load is best-effort: if the file isn't there yet,
/// the selection still persists (so the picker remembers it across
/// restarts and the eventual Download lands on the right model). The
/// frontend reads `loaded` to know which message to show.
#[tauri::command]
pub async fn model_select(state: State<'_, AppState>, id: String) -> IpcResult<ModelSelectResult> {
    if catalog::find_by_id(&id).is_none() {
        return Err(IpcError::Settings(format!(
            "unknown model id: {id} (not in the Whisper catalog)"
        )));
    }
    state
        .settings
        .set(settings_keys::SELECTED_MODEL_ID, &id)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;

    // Try to hot-load. The GGUF parse can take ~50–500 ms depending on
    // model size; do it on a blocking task so the IPC handler doesn't
    // hold the tokio runtime. If the file isn't on disk yet this
    // returns Ok(None) and we report `loaded: false` — selection has
    // already persisted, so the picker remembers across restarts.
    //
    // Loaded twice — once for the dictation slot, once for the
    // meeting-pump slot (#248). Both share the mmap'd weights on
    // disk, so the marginal cost of the second load is small.
    let models_dir = state.models_dir.clone();
    let id_for_load = id.clone();
    let inference_threads = std::sync::Arc::clone(&state.runtime_flags.inference_threads);
    let load_result = tauri::async_runtime::spawn_blocking(move || {
        let dictation =
            crate::ipc::load_transcriber_for_model(&id_for_load, &models_dir, &inference_threads)?;
        let meeting =
            crate::ipc::load_transcriber_for_model(&id_for_load, &models_dir, &inference_threads)?;
        Ok::<_, anyhow::Error>((dictation, meeting))
    })
    .await
    .map_err(|e| IpcError::Internal(format!("blocking task panicked: {e}")))?;

    match load_result {
        Ok((Some(dictation), Some(meeting))) => {
            state
                .swap_transcriber(Some(dictation), Some(meeting))
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(ModelSelectResult { loaded: true })
        }
        Ok((None, _)) | Ok((_, None)) => {
            // File not yet on disk, or whisper feature off. Selection
            // still persisted; user just needs to Download (or rebuild
            // with the whisper feature, but that's a contributor
            // concern, not an end-user one).
            Ok(ModelSelectResult { loaded: false })
        }
        Err(e) => {
            // File was on disk but failed to load (corrupted GGUF,
            // wrong format). Surface as a clear error so the user
            // knows to redownload.
            Err(IpcError::Transcription(format!(
                "failed to load {id}: {e:#}"
            )))
        }
    }
}

// -- Model auto-download -------------------------------------------------
//
// Three commands that wrap the pure-logic orchestrator in
// `transcription::download`. The orchestrator runs on a tokio task
// spawned from `model_download`; a [`CancelHandle`] is held in
// [`AppState::downloads`] so `model_cancel_download` can flip the flag
// from a separate command. Frontend listens for three Tauri events:
// `model:download-progress`, `model:download-done`,
// `model:download-failed`.

/// Payload for the `model:download-progress` event the frontend
/// listens for. Bandwidth-cheap; the frontend's progress bar is
/// driven from these alone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub id: String,
    pub bytes_received: u64,
    pub bytes_total: Option<u64>,
}

/// Payload for `model:download-done` and `:download-failed`. Done
/// carries no extra fields; failed carries a user-facing message
/// already mapped through [`IpcError`] formatting.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatus {
    pub id: String,
    pub message: Option<String>,
}

/// Begin downloading the model identified by `id`. Returns
/// immediately; the actual download runs on a tokio task and
/// reports progress via `model:download-progress` events.
///
/// The catalog must declare a non-empty `sha256` for the model —
/// integrity is non-negotiable. A model with an empty hash surfaces
/// as a clear error and the picker tells the user to download
/// manually until a contributor fills in the catalog.
///
/// Implementation delegates to [`model_download_inner`] (the
/// testable seam introduced in #315 follow-up) so the spawned-
/// task paths (cancel-handle cleanup, failure emit) can be
/// driven from a `#[tokio::test]` without a real Tauri runtime.
#[tauri::command]
pub async fn model_download(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> IpcResult<()> {
    let model = catalog::find_by_id(&id).ok_or_else(|| {
        IpcError::Settings(format!(
            "unknown model id: {id} (not in the Whisper catalog)"
        ))
    })?;

    let emitter: std::sync::Arc<dyn EventEmitter> =
        std::sync::Arc::new(TauriEventEmitter::new(app));
    model_download_inner(
        ModelDownloadDeps {
            emitter,
            downloads: std::sync::Arc::clone(&state.downloads),
            http: state.http.clone(),
            models_dir: state.models_dir.clone(),
        },
        model,
    )
    .await
}

/// Bundled dependencies the Whisper download needs at runtime.
/// Pulled out of [`AppState`] so [`model_download_inner`] can run
/// from a `#[tokio::test]` without a real `AppHandle` /
/// `tauri::State` (#315 follow-up; mirrors the diarizer's
/// `DiarizerDownloadDeps`).
pub(crate) struct ModelDownloadDeps {
    pub emitter: std::sync::Arc<dyn EventEmitter>,
    pub downloads:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, CancelHandle>>>,
    pub http: reqwest::Client,
    pub models_dir: std::path::PathBuf,
}

/// Inner implementation of [`model_download`]. Same behaviour as
/// the `#[tauri::command]` wrapper but takes the dependencies as
/// plain values so tests can drive both:
///
/// - the duplicate-rejection guard inside `state.downloads.lock()`;
///   and
/// - the cancel-handle cleanup on the spawned task's failure
///   branch (the same pattern the diarizer download uses).
///
/// Both the duplicate-rejection guard *and* the on-disk
/// existence check now live inside the same
/// `deps.downloads.lock()` critical section. Pre-#421 the
/// existence check sat outside the lock and two rapid clicks
/// could both pass it before either took the lock; one would
/// then start a download on top of a freshly-finalized file
/// (the other click's task having just removed its cancel
/// handle and written the file). Mirrors the audit-2 fix
/// landed for `download_diarizer_model_inner` in #308.
pub(crate) async fn model_download_inner(
    deps: ModelDownloadDeps,
    model: ModelMetadata,
) -> IpcResult<()> {
    if model.sha256.trim().is_empty() {
        return Err(IpcError::Settings(format!(
            "auto-download is not yet enabled for {} — its SHA-256 hasn't been verified. \
             Download manually for now (place {} in the models directory).",
            model.display_name, model.filename
        )));
    }

    let id = model.id.clone();
    let dest = deps.models_dir.join(&model.filename);

    // Register a cancel handle, re-check on-disk presence, and
    // refuse a duplicate concurrent click — all inside a single
    // critical section. The exists-check moved inside the lock
    // (audit-2 close-out, mirroring the diarizer fix from #308):
    // pre-fix two rapid clicks could both pass the on-disk check
    // before either took the lock, then both insert + start.
    // Holding the lock for the existence test means a concurrent
    // download that *just* finished is observable as either
    // "file exists now" or "cancel handle still in flight" —
    // caller gets a clean error either way and we never start a
    // duplicate download on top of a freshly-finalized file.
    let cancel = CancelHandle::new();
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

    tauri::async_runtime::spawn(async move {
        // Progress callback emits a Tauri event with the latest
        // counts. Cheap; reqwest streams in ~16-128 KiB chunks for
        // the typical Hugging Face CDN response.
        let emitter_for_progress = std::sync::Arc::clone(&emitter_for_task);
        let id_for_progress = id_for_task.clone();
        let progress: Box<download::ProgressCallback> = Box::new(move |update| {
            emitter_for_progress.emit(
                "model:download-progress",
                &DownloadProgress {
                    id: id_for_progress.clone(),
                    bytes_received: update.bytes_received,
                    bytes_total: update.bytes_total,
                },
            );
        });

        let result =
            download::download_with_progress(&http, &url, &dest, &sha, &cancel, &progress).await;

        // Drop the cancel handle from the registry on the way out,
        // success or failure.
        if let Ok(mut guard) = downloads_for_task.lock() {
            guard.remove(&id_for_task);
        }

        match result {
            Ok(()) => {
                emitter_for_task.emit(
                    "model:download-done",
                    &DownloadStatus {
                        id: id_for_task,
                        message: None,
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = ?e, model_id = %id_for_task, "model download failed");
                emitter_for_task.emit(
                    "model:download-failed",
                    &DownloadStatus {
                        id: id_for_task,
                        message: Some(format!("{e:#}")),
                    },
                );
            }
        }
    });

    Ok(())
}

/// Cancel an in-flight download. Flips the cancel flag held in
/// [`AppState::downloads`]; the spawned task notices on its next
/// chunk boundary and exits cleanly, deleting the partial file.
/// No-op if no download for `id` is in flight.
#[tauri::command]
pub fn model_cancel_download(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    let guard = state.downloads.lock().map_err(poisoned)?;
    if let Some(cancel) = guard.get(&id) {
        cancel.cancel();
    }
    Ok(())
}

/// Delete a model file from disk. Used both for "I changed my mind
/// about this model" and as the recovery path after a failed
/// download leaves a `.part` behind (though the orchestrator should
/// always clean up its own `.part` files).
#[tauri::command]
pub async fn model_remove(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    let model = catalog::find_by_id(&id).ok_or_else(|| {
        IpcError::Settings(format!(
            "unknown model id: {id} (not in the Whisper catalog)"
        ))
    })?;

    let path = state.models_dir.join(&model.filename);
    if !path.exists() {
        // Same no-op-on-missing pattern as the repository delete
        // contracts — caller's intent is satisfied either way.
        return Ok(());
    }

    tokio::fs::remove_file(&path)
        .await
        .map_err(|e| IpcError::Settings(format!("failed to remove {}: {e}", path.display())))?;

    // Also remove any orphan `.part` from a prior interrupted
    // download — best-effort, errors swallowed.
    let part = path.with_extension(format!(
        "{}.part",
        path.extension().and_then(|s| s.to_str()).unwrap_or("")
    ));
    let _ = tokio::fs::remove_file(part).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Whisper-shaped catalog entry pointing at whatever
    /// URL the test wants. SHA is a 64-char hex placeholder; the
    /// duplicate-rejection test never reaches the integrity check
    /// and the failure-cleanup test fails on connect before SHA
    /// is consulted.
    fn make_test_model(url: &str) -> ModelMetadata {
        ModelMetadata {
            id: "whisper-test".into(),
            display_name: "Whisper (test)".into(),
            filename: "ggml-test.bin".into(),
            size_mb: 1,
            speed_rating: 5,
            accuracy_rating: 5,
            description: "test entry".into(),
            is_default: false,
            download_url: url.into(),
            sha256: "0".repeat(64),
        }
    }

    fn build_deps(
        emitter: std::sync::Arc<dyn EventEmitter>,
        downloads: std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<String, CancelHandle>>,
        >,
        models_dir: std::path::PathBuf,
    ) -> ModelDownloadDeps {
        ModelDownloadDeps {
            emitter,
            downloads,
            http: reqwest::Client::new(),
            models_dir,
        }
    }

    #[tokio::test]
    async fn model_download_rejects_duplicate_concurrent_clicks() {
        // Pre-seed the downloads map with a cancel handle (as if a
        // prior click had spawned a task). Second call must return
        // `IpcError::Settings` and emit nothing.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            CancelHandle,
        >::new()));
        let model = make_test_model("http://127.0.0.1:1/never-fetched");
        downloads
            .lock()
            .unwrap()
            .insert(model.id.clone(), CancelHandle::new());

        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn EventEmitter> = std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let deps = build_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        match model_download_inner(deps, model.clone()).await {
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
        assert!(
            downloads.lock().unwrap().contains_key(&model.id),
            "pre-existing cancel handle was clobbered by the rejection path"
        );
    }

    #[tokio::test]
    async fn model_download_rejects_when_destination_already_exists() {
        // Audit-2 TOCTOU close (#308 / #421). Pre-fix the on-disk
        // existence check sat outside the lock; this test pins
        // that it's now part of the rejection path inside the
        // critical section. Pre-place the destination file, then
        // call the inner — must return `IpcError::Settings`
        // ("already downloaded") and emit no events.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            CancelHandle,
        >::new()));
        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn EventEmitter> = std::sync::Arc::new(recorder.clone());
        let tmp = tempfile::tempdir().unwrap();
        let model = make_test_model("http://127.0.0.1:1/never-fetched");

        // Pre-place the dest file as if a prior download had
        // landed it.
        let dest = tmp.path().join(&model.filename);
        std::fs::write(&dest, b"already here").unwrap();

        let deps = build_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        match model_download_inner(deps, model.clone()).await {
            Err(IpcError::Settings(msg)) => {
                assert!(
                    msg.contains("already downloaded"),
                    "expected exists-rejection message, got: {msg}"
                );
            }
            other => panic!("expected IpcError::Settings, got: {other:?}"),
        }
        assert!(
            recorder.events().is_empty(),
            "exists rejection should not emit any events"
        );
        // The cancel handle map must NOT have an entry for this
        // id — the rejection bailed before insert.
        assert!(
            !downloads.lock().unwrap().contains_key(&model.id),
            "exists rejection should not register a cancel handle"
        );
    }

    #[tokio::test]
    async fn model_download_rejects_empty_sha_with_clear_error() {
        // Catalog hygiene gate: a model whose SHA hasn't been
        // verified should never auto-download. Mirrors the message
        // the picker UI surfaces verbatim.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            CancelHandle,
        >::new()));
        let mut model = make_test_model("http://127.0.0.1:1/will-never-be-tried");
        model.sha256 = String::new();

        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn EventEmitter> = std::sync::Arc::new(recorder.clone());
        let tmp = tempfile::tempdir().unwrap();
        let deps = build_deps(emitter, downloads, tmp.path().to_path_buf());

        match model_download_inner(deps, model.clone()).await {
            Err(IpcError::Settings(msg)) => {
                assert!(
                    msg.contains("SHA-256 hasn't been verified"),
                    "expected sha-gate message, got: {msg}"
                );
            }
            other => panic!("expected IpcError::Settings, got: {other:?}"),
        }
        assert!(recorder.events().is_empty());
    }

    #[tokio::test]
    async fn model_download_clears_cancel_handle_on_failure() {
        // Drive the download into the failure branch by pointing
        // it at an unbindable port (127.0.0.1:1). The spawned task
        // must remove its cancel-handle entry from `downloads` AND
        // emit `model:download-failed` with a non-empty message.
        let downloads = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            CancelHandle,
        >::new()));
        let recorder = crate::ipc::events::RecordingEventEmitter::new();
        let emitter: std::sync::Arc<dyn EventEmitter> = std::sync::Arc::new(recorder.clone());

        let tmp = tempfile::tempdir().unwrap();
        let model = make_test_model("http://127.0.0.1:1/will-fail");
        let deps = build_deps(
            emitter,
            std::sync::Arc::clone(&downloads),
            tmp.path().to_path_buf(),
        );

        model_download_inner(deps, model.clone())
            .await
            .expect("inner returns Ok before the spawn — failure happens inside the task");

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
        assert_eq!(
            payload["id"],
            serde_json::Value::String(model.id.clone()),
            "failure event id should match the requested model id"
        );
        let msg = payload["message"]
            .as_str()
            .expect("failure event should carry a message string");
        assert!(!msg.is_empty(), "failure event message should be populated");
    }
}
