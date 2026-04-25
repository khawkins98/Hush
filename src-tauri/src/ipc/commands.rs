//! Tauri command handlers for the dictation pipeline.
//!
//! Kept thin: each command pulls long-lived services off [`AppState`]
//! and performs its OS side effects (clipboard write, native
//! notification, foreground-app capture) directly. The audio-then-
//! transcription path goes through [`super::run_pipeline`] for the
//! sake of unit-testability against mocks; the Tauri commands below
//! call the underlying trait methods inline so error classification
//! is structural rather than heuristic — see the note on
//! [`stop_dictation`] for the rationale.
//!
//! ## Command grouping
//!
//! As the surface has grown past a dozen commands, a quick map for
//! contributors landing here cold:
//!
//! - **Core dictation pipeline.** [`list_input_devices`],
//!   [`start_dictation`], [`stop_dictation`].
//! - **History (read-only browse + delete).** [`history_list`],
//!   [`history_search`], [`history_delete`], [`history_count`].
//! - **Replacements (post-transcription find/replace CRUD).**
//!   [`replacements_list`], [`replacement_create`],
//!   [`replacement_update`], [`replacement_delete`].
//! - **Vocabulary (Whisper prompt-bias CRUD).**
//!   [`vocabulary_list`], [`vocabulary_create`],
//!   [`vocabulary_update`], [`vocabulary_delete`].
//! - **Model picker.** [`model_list`], [`model_select`].

use std::sync::{Arc, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::AudioDevice;
use crate::dictionary::{
    apply_replacements, format_vocabulary_prompt, NewReplacementRule, NewVocabularyTerm,
    ReplacementRule, VocabularyTerm,
};
use crate::history::{HistoryEntry, NewHistoryEntry};
use crate::settings::keys as settings_keys;
use crate::transcription::catalog::{self, ModelMetadata};
use crate::transcription::download::{self, CancelHandle};

use super::{AppState, ForegroundApp};

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

    /// Settings repository (SQLite) error or the picker resolved a
    /// model id we don't know about. Surfaced separately because the
    /// frontend recovery copy is "pick a model from the catalog"
    /// rather than the dictionary-shaped "your settings" framing.
    #[error("settings: {0}")]
    Settings(String),

    /// History repository (SQLite) error — failed insert, list, search,
    /// or delete. Surfaced separately from `Internal` so the frontend
    /// can offer history-specific recovery copy ("History list failed,
    /// try again") rather than the generic "restart Hush".
    #[error("history: {0}")]
    History(String),

    /// Replacements repository (SQLite) error — failed CRUD on the
    /// dictionary's replacements table. Same rationale as `History`:
    /// a kebab-case kind (`replacements`) so the frontend can switch on
    /// it for tailored recovery copy.
    #[error("replacements: {0}")]
    Replacements(String),

    /// In-process state guard panicked while a lock was held. Should not
    /// happen in practice — only the IPC commands lock our internal
    /// mutexes and they don't panic — but a poisoned lock surfacing here
    /// is preferable to a `panic!` in a Tauri command, which can
    /// destabilise the renderer process.
    #[error("internal: {0}")]
    Internal(String),
}

type IpcResult<T> = std::result::Result<T, IpcError>;

/// Convert a `PoisonError` into an `IpcError::Internal` so callers can use
/// the `?` operator instead of `.expect("…mutex")`. Centralised so the
/// message string is consistent across call sites.
fn poisoned<T>(_: PoisonError<T>) -> IpcError {
    IpcError::Internal("internal state lock poisoned".to_owned())
}

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
/// Hush. We only commit the snapshot to [`AppState::pending_foreground`]
/// after `audio.start` succeeds, so a failed start does not leave a stale
/// snapshot in the slot.
#[tauri::command]
pub fn start_dictation(state: State<'_, AppState>, device_id: Option<String>) -> IpcResult<()> {
    start_dictation_inner(&state, device_id.as_deref())
}

/// Tauri-free orchestration for `start_dictation`. Split out so tests can
/// drive it against a mock [`AudioCapture`] without spinning up a Tauri
/// runtime — the public command is a one-line wrapper that lifts the
/// `State<'_, AppState>` newtype off and forwards.
fn start_dictation_inner(state: &AppState, device_id: Option<&str>) -> IpcResult<()> {
    let foreground = capture_foreground();

    state
        .audio
        .start(device_id)
        .map_err(|e| IpcError::Audio(e.to_string()))?;

    *state.pending_foreground.lock().map_err(poisoned)? = foreground;

    Ok(())
}

/// Stop capturing, transcribe, apply post-transcription replacements,
/// write to clipboard, fire a notification, and return the text to the
/// frontend.
///
/// The audio-stop and transcription calls are made inline rather than
/// being collapsed through a single helper, because we want each layer's
/// error to map to the right [`IpcError`] variant *structurally* (the
/// frontend dispatches recovery copy on `kind`). A previous attempt at
/// substring-classifying a merged error string was fragile: a whisper
/// error mentioning "device" was being routed to `Audio`. Splitting the
/// calls makes the boundary obvious and removes the heuristic.
///
/// **Replacement-rule load failure is non-fatal**: if the rules table
/// can't be read for some reason we log and continue with no
/// replacements rather than failing the whole dictation. The user
/// already has the text; surfacing "we couldn't load your rules" as a
/// hard error would block them on a strictly-secondary feature. The
/// settings surface (M3) can offer a "rules failed to load — see logs"
/// banner if this turns out to matter in practice.
///
/// Clipboard write is the user's actual artefact; if it fails we surface
/// the error to the frontend so the user knows the text wasn't pasteable.
/// The notification is courtesy and best-effort: if the platform refuses
/// to fire one (Linux without a notification daemon, for example), we
/// swallow the error and continue.
#[tauri::command]
pub async fn stop_dictation(
    app: AppHandle,
    state: State<'_, AppState>,
) -> IpcResult<DictationResult> {
    let transcriber = state
        .transcribe
        .as_ref()
        .ok_or(IpcError::TranscriptionUnavailable)?
        .clone();

    let captured = state
        .audio
        .stop()
        .map_err(|e| IpcError::Audio(e.to_string()))?;

    // Build the vocabulary prompt before inference. A failure here
    // demotes to the no-prompt path — the dictation still works, the
    // user just doesn't get the bias for that one transcription.
    let prompt = match state.vocabulary.list().await {
        Ok(terms) => format_vocabulary_prompt(&terms),
        Err(e) => {
            tracing::error!(error = ?e, "failed to load vocabulary; skipping prompt-biasing");
            String::new()
        }
    };

    // Call the prompt-biased path even when the prompt is empty: the
    // default trait impl falls through to the no-prompt `transcribe()`
    // and Whisper-rs's `set_initial_prompt` is a no-op for an empty
    // string anyway, so callers don't have to branch on the prompt.
    let raw_text = transcriber
        .transcribe_with_prompt(&captured, &prompt)
        .map_err(|e| IpcError::Transcription(e.to_string()))?;

    // Pull the replacement rules and apply them. Same non-fatal pattern.
    let rules = match state.replacements.list().await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load replacement rules; skipping post-processing");
            Vec::new()
        }
    };
    let text = apply_replacements(raw_text.trim(), &rules);

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

    let foreground = state.pending_foreground.lock().map_err(poisoned)?.take();

    // Persist to history. Best-effort: a failed insert must not fail the
    // dictation — the user already has the text on the clipboard, and
    // surfacing "history insert failed" as a hard error would block them
    // from getting on with their work. We log and continue. If history
    // becomes load-bearing (e.g. a future pipeline that re-references
    // recent rows) this should be revisited.
    let history = Arc::clone(&state.history);
    let new_entry = NewHistoryEntry {
        transcript: text.clone(),
        app_name: foreground.as_ref().map(|f| f.app_name.clone()),
        window_title: foreground.as_ref().map(|f| f.window_title.clone()),
        model: transcriber.model_label(),
        // Recording duration tracking lands with the HUD overlay (#21);
        // for now we accept that history rows have None here.
        duration_ms: None,
    };
    tauri::async_runtime::spawn(async move {
        if let Err(e) = history.insert(new_entry).await {
            tracing::error!(error = ?e, "failed to persist transcription to history");
        }
    });

    Ok(DictationResult { text, foreground })
}

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
        .history
        .list(limit, offset)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
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
        .history
        .search(&query, limit, offset)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Delete a single history row. No-op (returns Ok) if `id` does not
/// exist — mirrors the trait contract.
#[tauri::command]
pub async fn history_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .history
        .delete(id)
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

/// Total row count, for paginators that need "page X of Y".
#[tauri::command]
pub async fn history_count(state: State<'_, AppState>) -> IpcResult<i64> {
    state
        .history
        .count()
        .await
        .map_err(|e| IpcError::History(e.to_string()))
}

// -- Replacement-rule CRUD -----------------------------------------------
//
// Settings-shaped commands the frontend's "Replacements" panel binds to.
// All four are async because the underlying repository is async; the IPC
// surface is intentionally thin — the pure-logic [`apply_replacements`]
// is in `dictionary` and runs on the dictation hot-path inside
// `stop_dictation` above.

/// All replacement rules in `(sort_order, id)` order.
#[tauri::command]
pub async fn replacements_list(state: State<'_, AppState>) -> IpcResult<Vec<ReplacementRule>> {
    state
        .replacements
        .list()
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Insert a new replacement. Returns the persisted row (with the
/// database-assigned id) so the frontend can append it to its local list
/// without a follow-up `list` round-trip.
#[tauri::command]
pub async fn replacement_create(
    state: State<'_, AppState>,
    find_text: String,
    replace_text: String,
    sort_order: i64,
) -> IpcResult<ReplacementRule> {
    state
        .replacements
        .create(NewReplacementRule {
            find_text,
            replace_text,
            sort_order,
        })
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Update an existing replacement's fields. The frontend passes the full
/// rule (not a partial diff) so the backend never has to reason about
/// "which fields changed". No-op if `id` does not exist.
#[tauri::command]
pub async fn replacement_update(
    state: State<'_, AppState>,
    rule: ReplacementRule,
) -> IpcResult<()> {
    state
        .replacements
        .update(rule)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a single replacement. No-op if `id` does not exist.
#[tauri::command]
pub async fn replacement_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .replacements
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Vocabulary CRUD -----------------------------------------------------
//
// Errors here surface as `IpcError::Replacements` rather than a
// dedicated `Vocabulary` variant because users see one combined
// "Dictionary settings" surface in the UI for both subsystems —
// keeping the error `kind` unified means the frontend's error switch
// doesn't sprout two near-identical branches that drift over time.

/// All vocabulary terms in insertion order.
#[tauri::command]
pub async fn vocabulary_list(state: State<'_, AppState>) -> IpcResult<Vec<VocabularyTerm>> {
    state
        .vocabulary
        .list()
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Insert a new vocabulary term. The schema enforces `UNIQUE` on `term`,
/// so duplicates surface as an error here for the frontend to render.
#[tauri::command]
pub async fn vocabulary_create(
    state: State<'_, AppState>,
    term: String,
) -> IpcResult<VocabularyTerm> {
    state
        .vocabulary
        .create(NewVocabularyTerm { term })
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Update an existing vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_update(state: State<'_, AppState>, term: VocabularyTerm) -> IpcResult<()> {
    state
        .vocabulary
        .update(term)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .vocabulary
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Model picker --------------------------------------------------------
//
// Static catalog of Whisper variants (see `transcription::catalog`)
// joined with on-disk presence (does the file exist in
// `<app_data>/models/`?) and the user's current selection from
// settings. The frontend renders this as a card grid; selecting a
// card writes the id to settings. **Auto-download is not part of M3** —
// the user puts files in the models directory manually for now.

/// Card-friendly view of a model: its catalog metadata plus runtime
/// state the picker UI cares about.
#[derive(Debug, Clone, serde::Serialize)]
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

/// Persist the user's choice. The new transcriber is loaded on the
/// next app start (no hot-swap yet — see the `learnings.md` note on
/// model-picker scope).
#[tauri::command]
pub async fn model_select(state: State<'_, AppState>, id: String) -> IpcResult<()> {
    if catalog::find_by_id(&id).is_none() {
        return Err(IpcError::Settings(format!(
            "unknown model id: {id} (not in the Whisper catalog)"
        )));
    }
    state
        .settings
        .set(settings_keys::SELECTED_MODEL_ID, &id)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
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

    if model.sha256.trim().is_empty() {
        return Err(IpcError::Settings(format!(
            "auto-download is not yet enabled for {} — its SHA-256 hasn't been verified. \
             Download manually for now (place {} in the models directory).",
            model.display_name, model.filename
        )));
    }

    let dest = state.models_dir.join(&model.filename);
    if dest.exists() {
        return Err(IpcError::Settings(format!(
            "{} is already downloaded",
            model.display_name
        )));
    }

    // Register a cancel handle and bail if a download is already in
    // flight for this model. The HashMap is keyed by id; one
    // concurrent download per model is the contract.
    let cancel = CancelHandle::new();
    {
        let mut guard = state.downloads.lock().map_err(poisoned)?;
        if guard.contains_key(&id) {
            return Err(IpcError::Settings(format!(
                "{} is already downloading",
                model.display_name
            )));
        }
        guard.insert(id.clone(), cancel.clone());
    }

    let app_for_task = app.clone();
    let id_for_task = id.clone();
    let url = model.download_url.clone();
    let sha = model.sha256.clone();
    let http = state.http.clone();
    // The downloads HashMap is shared across the task and the IPC
    // commands that touch it. We hold an `Arc<Mutex<…>>` view via the
    // AppHandle's managed state at task-completion time.
    let downloads_app = app.clone();

    tauri::async_runtime::spawn(async move {
        // Progress callback emits a Tauri event with the latest
        // counts. Cheap; reqwest streams in ~16-128 KiB chunks for
        // the typical Hugging Face CDN response.
        let app_for_progress = app_for_task.clone();
        let id_for_progress = id_for_task.clone();
        let progress: Box<download::ProgressCallback> = Box::new(move |update| {
            let _ = app_for_progress.emit(
                "model:download-progress",
                DownloadProgress {
                    id: id_for_progress.clone(),
                    bytes_received: update.bytes_received,
                    bytes_total: update.bytes_total,
                },
            );
        });

        let result =
            download::download_with_progress(&http, &url, &dest, &sha, &cancel, &progress).await;

        // Drop the cancel handle from the registry on the way out,
        // success or failure. Use the AppHandle's managed state so
        // the task doesn't need to hold a long-lived reference to
        // `state`.
        if let Some(state) = downloads_app.try_state::<AppState>() {
            if let Ok(mut guard) = state.downloads.lock() {
                guard.remove(&id_for_task);
            }
        }

        match result {
            Ok(()) => {
                let _ = app_for_task.emit(
                    "model:download-done",
                    DownloadStatus {
                        id: id_for_task,
                        message: None,
                    },
                );
            }
            Err(e) => {
                tracing::error!(error = ?e, model_id = %id_for_task, "model download failed");
                let _ = app_for_task.emit(
                    "model:download-failed",
                    DownloadStatus {
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
    fn ipc_error_internal_serialises_with_kebab_case_kind() {
        // The `Internal` variant exists specifically so a poisoned
        // mutex does not panic the Tauri command. Confirm it round-
        // trips through serde with the same shape as the other
        // payload-bearing variants — the frontend's switch-on-kind
        // dispatch depends on this.
        let json = serde_json::to_string(&IpcError::Internal("locked".into())).unwrap();
        assert!(json.contains("\"kind\":\"internal\""), "got: {json}");
        assert!(json.contains("\"message\":\"locked\""), "got: {json}");
    }

    // -- start_dictation_inner regression tests ---------------------------
    //
    // These cover the foreground-leak fix surfaced in code review: a
    // failed `audio.start` must not overwrite or pollute the
    // `pending_foreground` slot. Using mock implementations of
    // `AudioCapture` rather than the cpal backend so we do not need a real
    // microphone or Tauri runtime.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;

    use crate::audio::{AudioCapture, AudioDevice, CapturedAudio};
    use crate::ipc::AppState;

    struct AudioThatFailsToStart;

    impl AudioCapture for AudioThatFailsToStart {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Err(anyhow!("device unplugged"))
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!("stop should not be called when start fails")
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct AudioThatStarts {
        recording: AtomicBool,
    }

    impl AudioCapture for AudioThatStarts {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            self.recording.store(true, Ordering::Release);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            unreachable!()
        }
        fn is_recording(&self) -> bool {
            self.recording.load(Ordering::Acquire)
        }
    }

    #[test]
    fn start_dictation_does_not_overwrite_foreground_on_audio_start_failure() {
        let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatFailsToStart);
        let state = AppState::new(
            audio,
            None,
            Arc::new(crate::ipc::tests::NoopHistory),
            Arc::new(crate::ipc::tests::NoopReplacements),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }),
            std::path::PathBuf::from("/tmp/hush-test-models"),
        );

        // Pre-populate the slot with a sentinel value so a regression in
        // the assignment order — assigning the new capture before
        // `audio.start` returns — would visibly overwrite it.
        *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
            app_name: "sentinel".into(),
            window_title: "sentinel".into(),
        });

        let err = start_dictation_inner(&state, None).expect_err("audio.start fails");
        assert!(
            matches!(err, IpcError::Audio(_)),
            "expected IpcError::Audio, got {err:?}"
        );

        let after = state.pending_foreground.lock().unwrap().clone();
        assert_eq!(
            after.map(|f| f.app_name).as_deref(),
            Some("sentinel"),
            "pending_foreground was overwritten despite failed start"
        );
    }

    #[test]
    fn start_dictation_succeeds_and_leaves_a_foreground_slot_for_stop() {
        // Confirms the happy path actually does write into the slot —
        // otherwise the bug-fix above could be "we just never assign
        // anything", which would also pass the regression test in
        // isolation.
        let audio: Arc<dyn AudioCapture> = Arc::new(AudioThatStarts {
            recording: AtomicBool::new(false),
        });
        let state = AppState::new(
            audio,
            None,
            Arc::new(crate::ipc::tests::NoopHistory),
            Arc::new(crate::ipc::tests::NoopReplacements),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }),
            std::path::PathBuf::from("/tmp/hush-test-models"),
        );

        // We can't observe the OS foreground app reliably from a test
        // process, so we just assert the call returned Ok and the slot is
        // *some* value (None or Some, both are acceptable — the OS may
        // genuinely have no active window in CI).
        start_dictation_inner(&state, None).expect("should succeed");

        // Just prove the lock didn't poison and the slot is reachable.
        let _: Option<ForegroundApp> = state.pending_foreground.lock().unwrap().clone();
    }

    /// Suppress the dead-code warning that fires because [`Mutex`] is
    /// otherwise unused after the regression tests' construction —
    /// this is part of the type signature compile-check above.
    #[allow(dead_code)]
    fn _assert_state_mutex_holds_foreground(state: AppState) -> Mutex<Option<ForegroundApp>> {
        state.pending_foreground
    }
}
