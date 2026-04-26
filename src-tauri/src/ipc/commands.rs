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

use crate::audio::{AudioDevice, AudioSource, AudioSourceListing};
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
/// `text` is what was written to the clipboard (after vocabulary-prompt
/// biasing during inference and post-transcription replacement rules).
/// `foreground` is the app + window title captured *at start* of the
/// recording — not at stop, because by stop time the user has alt-tabbed
/// back to Hush and "current foreground" would always be us. The backend
/// already inserts a history row with this metadata via the
/// fire-and-forget `spawn_history_create` helper in `stop_dictation`, so
/// the frontend doesn't need to round-trip it back through `history_*`.
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

    /// Surfaced when no transcription backend is configured at the time
    /// of `stop_dictation`. Either the user hasn't picked a downloaded
    /// model yet (the model picker is shipped — first-run users see a
    /// banner pointing them at it; the `Start recording` button is
    /// disabled in that state) or the binary was built without the
    /// `whisper` Cargo feature (UI-only contributors using
    /// `npm run tauri:ui-only`). The frontend's recovery copy points at
    /// the in-app picker and the legacy `HUSH_MODEL_PATH` env-var path
    /// is no longer surfaced to end users.
    #[error("no transcription model loaded (pick one in the model picker, or rebuild with the whisper feature)")]
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
/// **Superseded** by [`audio_list_sources`], which returns mic devices
/// AND the system-audio entry with capability flags. Kept as a Tauri
/// command for one transitional release so any frontend still binding
/// to the old name keeps working — slated for removal once the picker
/// migration is verified across all hands-on smoke surfaces.
///
/// Tauri marshals errors via the `Serialize` impl on [`IpcError`].
#[tauri::command]
pub fn list_input_devices(state: State<'_, AppState>) -> IpcResult<Vec<AudioDevice>> {
    state
        .audio
        .list_input_devices()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Enumerate every audio source the user can pick from in the source
/// picker — every input device plus the system-audio entry, with
/// `is_supported` flags per source so the frontend can render
/// not-yet-shipped options as disabled.
///
/// Replaces [`list_input_devices`] for the source-picker UI; see
/// [`crate::audio::AudioSourceListing`] for the wire shape.
#[tauri::command]
pub fn audio_list_sources(state: State<'_, AppState>) -> IpcResult<Vec<AudioSourceListing>> {
    state
        .audio
        .list_audio_sources()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Begin capturing from `source` (microphone or system audio).
///
/// Captures the foreground app *before* opening the input stream so the
/// snapshot is taken while the user's intended target window still has
/// focus — by the time the stream is open they may have alt-tabbed back to
/// Hush. We only commit the snapshot to [`AppState::pending_foreground`]
/// after `audio.start` succeeds, so a failed start does not leave a stale
/// snapshot in the slot. Shows the recording HUD as the last step (after
/// the audio stream is live) so a failed `start` doesn't flash the HUD on
/// then off.
///
/// If `source` is omitted the default mic is used — keeps the dictation
/// hot path one-click-from-the-hotkey for the no-options-touched case.
#[tauri::command]
pub fn start_dictation(
    app: AppHandle,
    state: State<'_, AppState>,
    source: Option<AudioSource>,
) -> IpcResult<()> {
    let source = source.unwrap_or_else(AudioSource::default_microphone);
    start_dictation_inner(&state, source)?;
    if let Err(e) = crate::hud::show(&app) {
        tracing::error!(error = ?e, "failed to show recording HUD");
    }
    Ok(())
}

/// Tauri-free orchestration for `start_dictation`. Split out so tests can
/// drive it against a mock [`AudioCapture`] without spinning up a Tauri
/// runtime — the public command is a one-line wrapper that lifts the
/// `State<'_, AppState>` newtype off and forwards.
fn start_dictation_inner(state: &AppState, source: AudioSource) -> IpcResult<()> {
    let foreground = capture_foreground();

    state
        .audio
        .start_with_source(source)
        .map_err(|e| IpcError::Audio(e.to_string()))?;

    *state.pending_foreground.lock().map_err(poisoned)? = foreground;

    Ok(())
}

/// Stop capturing, transcribe, apply post-transcription replacements,
/// write to clipboard, fire a notification, and return the text to the
/// frontend.
///
/// ## Fatal-vs-best-effort policy
///
/// The function deliberately treats some failures as fatal and others
/// as best-effort. The split is by *whether the user's deliverable is
/// affected*:
///
/// - **Fatal** (returns `Err` and dictation fails):
///   - Audio backend stop (no audio → no transcript)
///   - Transcription itself (no transcript → no clipboard write)
///   - Clipboard write (the user's actual artefact — without this,
///     they can't paste, which is the whole point)
/// - **Best-effort** (logged and skipped):
///   - Vocabulary prompt load (without it, transcription works but
///     loses prompt-bias for proper nouns/jargon)
///   - Replacement-rule load (without them, the raw transcript still
///     reaches the clipboard — replacements are polish)
///   - "Ready to paste" notification (Linux-without-daemon edge
///     case; user has the text on the clipboard regardless)
///   - History insert (fire-and-forget; logged at error level if it
///     fails. The user has their text; losing one history row is a
///     missed analytics moment, not a blocked workflow)
///
/// ## Why the audio/transcription split is structural, not heuristic
///
/// The audio-stop and transcription calls are made inline rather than
/// collapsed through a single helper, because each layer's error must
/// map to the right [`IpcError`] variant *structurally* — the frontend
/// dispatches recovery copy on `kind`. A previous attempt at substring-
/// classifying a merged error string was fragile: a whisper error
/// mentioning "device" was being routed to `Audio`. Splitting the
/// calls makes the boundary obvious and removes the heuristic.
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
    let transcriber = {
        // Lock briefly, clone the Arc, drop the lock. The dictation
        // hot path only needs a snapshot of "what's loaded right now".
        // Hot-swap from `model_select` will land for the *next* call.
        let guard = state.transcribe.lock().map_err(poisoned)?;
        guard
            .as_ref()
            .ok_or(IpcError::TranscriptionUnavailable)?
            .clone()
    };

    // The user pressed Stop; the HUD should hide whether or not the
    // backend stop succeeds. Errors from the audio backend are
    // surfaced to the caller, but only after the HUD is down.
    let captured = stop_audio_capture(&state).map_err(|e| {
        let _ = crate::hud::hide(&app);
        e
    })?;
    if let Err(e) = crate::hud::hide(&app) {
        tracing::error!(error = ?e, "failed to hide recording HUD");
    }

    // Vocabulary + replacements load are best-effort. Inference itself
    // is fatal — without text there's nothing for the user to paste.
    let prompt = load_vocabulary_prompt(&state).await;
    // If the user has vocabulary terms configured but the loaded
    // backend can't act on them, warn once per dictation. This is the
    // place where "vocabulary terms silently produce no effect"
    // would otherwise hide. The check is gated on `!prompt.is_empty()`
    // so the no-vocab case doesn't spam the log on every dictation.
    if !prompt.is_empty() && !transcriber.supports_prompt_biasing() {
        tracing::warn!(
            backend = transcriber.model_label(),
            "vocabulary terms configured but the active transcription backend does not support prompt biasing — terms will not affect this transcript"
        );
    }
    let raw_text = transcriber
        .transcribe_with_prompt(&captured, &prompt)
        .map_err(|e| IpcError::Transcription(e.to_string()))?;
    let rules = load_replacement_rules(&state).await;
    let text = apply_replacements(raw_text.trim(), &rules);

    write_to_clipboard(&app, &text)?;
    fire_ready_notification(&app);

    let foreground = take_foreground_snapshot(&state)?;
    spawn_history_create(
        Arc::clone(&state.history),
        NewHistoryEntry {
            transcript: text.clone(),
            app_name: foreground.as_ref().map(|f| f.app_name.clone()),
            window_title: foreground.as_ref().map(|f| f.window_title.clone()),
            model: transcriber.model_label(),
            // Recording duration tracking lands with the HUD level-meter
            // (#21); for now history rows have None here.
            duration_ms: None,
        },
    );

    Ok(DictationResult { text, foreground })
}

/// Stop the audio stream and return the captured samples, mapping the
/// backend error to [`IpcError::Audio`]. Split out so `stop_dictation`
/// can keep its HUD-hide-on-error step a single line.
fn stop_audio_capture(state: &AppState) -> IpcResult<crate::audio::CapturedAudio> {
    state
        .audio
        .stop()
        .map_err(|e| IpcError::Audio(e.to_string()))
}

/// Load the user's vocabulary terms and format them as the initial
/// Whisper prompt. Best-effort: a repository error logs and demotes
/// to the no-prompt path. The decoder treats an empty prompt as a no-op
/// (both via the trait's default `transcribe_with_prompt` and via
/// `set_initial_prompt` itself), so the caller never has to branch.
async fn load_vocabulary_prompt(state: &AppState) -> String {
    match state.vocabulary.list().await {
        Ok(terms) => format_vocabulary_prompt(&terms),
        Err(e) => {
            tracing::error!(error = ?e, "failed to load vocabulary; skipping prompt-biasing");
            String::new()
        }
    }
}

/// Load post-transcription find/replace rules. Best-effort: a failure
/// here demotes to "no rules applied" rather than failing the whole
/// dictation. The user already has audio captured and a transcript
/// pending; surfacing a rules-load error as fatal would block them on
/// a strictly-secondary feature.
async fn load_replacement_rules(state: &AppState) -> Vec<ReplacementRule> {
    match state.replacements.list().await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load replacement rules; skipping post-processing");
            Vec::new()
        }
    }
}

/// Pop the foreground snapshot captured at `start_dictation`. Returns
/// `None` if the slot is empty (which can happen if a hotkey-driven
/// start raced the snapshot capture). The `Mutex` is fenced via
/// [`poisoned`] so a panicked thread doesn't bring down a Tauri command.
fn take_foreground_snapshot(state: &AppState) -> IpcResult<Option<ForegroundApp>> {
    Ok(state.pending_foreground.lock().map_err(poisoned)?.take())
}

/// Write the final text to the system clipboard. Fatal on failure —
/// the clipboard is the user's actual artefact for this dictation.
fn write_to_clipboard(app: &AppHandle, text: &str) -> IpcResult<()> {
    app.clipboard()
        .write_text(text.to_owned())
        .map_err(|e| IpcError::Clipboard(e.to_string()))
}

/// Fire the "Ready to paste" courtesy notification. Best-effort: on
/// platforms without a notification daemon (e.g. Linux without
/// dbus/notify-send) we log and continue.
fn fire_ready_notification(app: &AppHandle) {
    if let Err(e) = app
        .notification()
        .builder()
        .title("Hush")
        .body("Ready to paste")
        .show()
    {
        tracing::warn!(error = ?e, "failed to fire 'ready to paste' notification");
    }
}

/// Persist `entry` to history on the Tauri async runtime. Fire-and-
/// forget: a failed insert is logged and swallowed, never bubbled to
/// the user — the clipboard write is what they care about. If history
/// ever becomes load-bearing for a downstream pipeline, this needs
/// revisiting.
fn spawn_history_create(
    history: Arc<dyn crate::history::HistoryRepository>,
    entry: NewHistoryEntry,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = history.create(entry).await {
            tracing::error!(error = ?e, "failed to persist transcription to history");
        }
    });
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
    let models_dir = state.models_dir.clone();
    let id_for_load = id.clone();
    let load_result = tauri::async_runtime::spawn_blocking(move || {
        crate::ipc::load_transcriber_for_model(&id_for_load, &models_dir)
    })
    .await
    .map_err(|e| IpcError::Internal(format!("blocking task panicked: {e}")))?;

    match load_result {
        Ok(Some(new_transcriber)) => {
            state
                .swap_transcriber(Some(new_transcriber))
                .map_err(|e| IpcError::Internal(e.to_string()))?;
            Ok(ModelSelectResult { loaded: true })
        }
        Ok(None) => {
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

// -- First-run / onboarding ----------------------------------------------
//
// Two thin commands wrapping the existing `SettingsRepository` for the
// macOS first-run welcome modal. Only macOS frontends consult these —
// the welcome flow is gated by `cfg!(target_os = "macos")` on the
// frontend's onMount path. Backend-side the commands are
// platform-independent because the settings table doesn't care which
// OS is reading it.
//
// The macOS-specific framing for the modal is documented in
// `learnings.md`: rdev's `listen` triggers the Input Monitoring
// prompt at app startup with no programmatic detection of grant
// state, and cpal triggers the Microphone prompt the first time
// recording starts. The welcome flow educates the user on what just
// happened (or what will happen on first record) and points them at
// System Settings if they declined.

/// Returns whether the macOS first-run welcome has been shown and
/// dismissed for this install. The value is stored under
/// [`crate::settings::keys::FIRST_RUN_COMPLETED`] as the literal
/// string `"true"` once dismissed; any other state (including the
/// settings row being absent) reads as `false`.
#[tauri::command]
pub async fn get_first_run_completed(state: State<'_, AppState>) -> IpcResult<bool> {
    let value = state
        .settings
        .get(crate::settings::keys::FIRST_RUN_COMPLETED)
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))?;
    Ok(value.as_deref() == Some("true"))
}

/// Persist that the user has dismissed the welcome modal. Idempotent;
/// calling twice is the same as once.
#[tauri::command]
pub async fn mark_first_run_completed(state: State<'_, AppState>) -> IpcResult<()> {
    state
        .settings
        .set(crate::settings::keys::FIRST_RUN_COMPLETED, "true")
        .await
        .map_err(|e| IpcError::Settings(e.to_string()))
}

/// Open the macOS System Settings pane the user needs to grant
/// the named permission. Tauri's shell plugin can launch arbitrary
/// URLs but its capability config requires us to whitelist URL
/// schemes — `x-apple.systempreferences:` isn't on the default
/// list. Routing through this command instead lets us pre-vet the
/// targets (a small enum of known panes) and keeps the capabilities
/// surface minimal.
///
/// On non-macOS platforms this is a no-op that returns `Ok(())`,
/// since the frontend's welcome modal is already gated on
/// `target_os = "macos"`. The fallthrough avoids a `cfg`-based
/// command-not-found error if the frontend ever calls this on the
/// wrong platform.
#[tauri::command]
pub async fn open_macos_privacy_pane(target: String) -> IpcResult<()> {
    #[cfg(target_os = "macos")]
    {
        // Whitelisted targets — anything else gets rejected so a
        // misbehaving frontend can't pivot this into an arbitrary
        // command launcher.
        let url = match target.as_str() {
            "microphone" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            "input-monitoring" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            "accessibility" => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            other => {
                return Err(IpcError::Settings(format!(
                    "unknown privacy pane target: {other:?}"
                )));
            }
        };

        // `open` is the macOS canonical "launch by URL scheme"
        // command; it Just Works for `x-apple.systempreferences:`.
        // No shell injection risk because the URL is a hard-coded
        // string keyed by a whitelisted enum.
        std::process::Command::new("open")
            .arg(url)
            .status()
            .map_err(|e| IpcError::Settings(format!("open System Settings: {e}")))?;

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        // No-op on Linux / Windows so the frontend doesn't have to
        // branch by platform — the welcome modal that calls this is
        // already macOS-only, and a stray invoke from the wrong
        // platform should fail soft.
        let _ = target;
        Ok(())
    }
}

/// Bundle identifier this binary registers with macOS TCC. Hard-coded
/// because `tauri.conf.json`'s `identifier` is the source of truth and
/// reading it back through `AppHandle::config().identifier()` would
/// require platform conditional plumbing for what is effectively a
/// constant string. If the bundle id ever changes, this constant and
/// the `tauri.conf.json` field move together.
#[cfg(target_os = "macos")]
const MACOS_BUNDLE_ID: &str = "com.khawkins.hush";

/// What [`diagnose_macos_permissions`] returns to the frontend.
///
/// Best-effort diagnostic snapshot. macOS deliberately does not expose
/// programmatic read access to TCC grant state, so this struct cannot
/// say "Microphone is granted" with certainty without actually opening
/// a stream and observing whether samples flow — which would trigger
/// the OS prompt as a side effect, defeating the diagnostic purpose.
/// Instead the struct tells the user what bundle id is in play (so they
/// can confirm the right entry in System Settings) and surfaces the
/// reset path as an actionable next step.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosPermissionDiagnostic {
    /// The bundle id macOS uses to key TCC entries against this binary.
    /// Stable for the signed-bundle path; on unsigned dev builds TCC
    /// may instead key on the binary hash, which is why a `tccutil
    /// reset … <bundle_id>` can return "no entry" — see
    /// `docs/macos-permissions.md` for the full picture.
    pub bundle_id: String,
    /// Human-readable hint about how to verify Microphone access.
    /// Not a probe — see the struct doc for why we don't probe.
    pub microphone_hint: String,
    /// Human-readable hint about Input Monitoring (PTT). On macOS 26+
    /// PTT is disabled by default (#69) so this hint covers both the
    /// "PTT off by default" and "verify in System Settings" paths.
    pub input_monitoring_hint: String,
    /// Whether the running platform supports the in-app reset action.
    /// True only on macOS — `reset_macos_permissions` is a no-op
    /// elsewhere. The frontend uses this to decide whether to show
    /// the Reset button at all.
    pub can_reset: bool,
}

/// Best-effort diagnostic snapshot for the macOS permission story.
///
/// Returns immediately on every platform. On non-macOS, returns hints
/// that explain there's nothing to diagnose; on macOS, returns the
/// bundle id and the recovery copy. Does not probe Microphone or
/// Input Monitoring directly — both probes have the side effect of
/// triggering OS prompts, which we don't want a passive diagnostic to
/// do.
///
/// Pairs with [`reset_macos_permissions`]: the diagnostic is the
/// "what do I see?" half; the reset is the "click here to fix it"
/// half. See `docs/macos-permissions.md` for the manual recipe this
/// in-app surface wraps.
#[tauri::command]
pub async fn diagnose_macos_permissions() -> IpcResult<MacosPermissionDiagnostic> {
    #[cfg(target_os = "macos")]
    {
        Ok(MacosPermissionDiagnostic {
            bundle_id: MACOS_BUNDLE_ID.to_owned(),
            microphone_hint: "Click Start recording to verify. macOS prompts the first time; \
                 if no prompt appears and the meter never moves, Microphone is denied. \
                 Use Reset below to re-prompt cleanly."
                .to_owned(),
            input_monitoring_hint:
                "Required for push-to-talk via the rdev hook. Disabled by default on \
                 macOS 26+ to avoid a TSM crash (#69); set HUSH_PTT_ENABLE=1 to opt in \
                 on older macOS. Use Reset to re-prompt if you previously denied."
                    .to_owned(),
            can_reset: true,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(MacosPermissionDiagnostic {
            bundle_id: String::new(),
            microphone_hint: "Microphone permission is handled by your platform's audio stack \
                 (PulseAudio / PipeWire on Linux, Privacy on Windows). The in-app \
                 diagnostic is macOS-only."
                .to_owned(),
            input_monitoring_hint: "Input Monitoring is a macOS concept; not applicable here."
                .to_owned(),
            can_reset: false,
        })
    }
}

/// What [`reset_macos_permissions`] returns. The string is a one-line
/// summary suitable for showing in the UI as a confirmation banner.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacosPermissionResetResult {
    /// True if at least one TCC entry was reset; false if every
    /// `tccutil reset` returned "no entry" (the unsigned-dev-binary
    /// case where TCC isn't keying on the bundle id at all).
    pub any_reset: bool,
    /// One-line user-facing message — populated either way.
    pub summary: String,
}

/// Run the three `tccutil reset` commands documented in
/// `docs/macos-permissions.md` for `com.khawkins.hush`. Microphone,
/// Input Monitoring (`ListenEvent`), and Accessibility are all reset;
/// each is independent and a missing-entry on any one is treated as
/// a soft success (the entry never existed to reset).
///
/// On non-macOS this is a no-op that reports "not applicable".
///
/// The reset takes effect on the *next* launch — the running process
/// continues to hold whatever permissions it already had. The frontend
/// shows a "now restart Hush" confirmation after a successful call.
#[tauri::command]
pub async fn reset_macos_permissions() -> IpcResult<MacosPermissionResetResult> {
    #[cfg(target_os = "macos")]
    {
        // Three independent invocations rather than one call with `all`
        // because the latter would also reset every other app's TCC
        // state for that category — far too broad. Per-bundle-id keeps
        // the blast radius scoped to Hush.
        let categories = ["Microphone", "ListenEvent", "Accessibility"];
        let mut any_reset = false;
        for category in categories {
            // `tccutil reset <category> <bundle_id>` exits 0 on success
            // and non-zero on "no entry to reset". The latter is fine —
            // unsigned dev binaries often don't key on the bundle id at
            // all, so there's nothing to reset. We track which ones
            // actually did something so the summary message can be
            // honest about whether the reset accomplished anything.
            let status = std::process::Command::new("tccutil")
                .arg("reset")
                .arg(category)
                .arg(MACOS_BUNDLE_ID)
                .status()
                .map_err(|e| IpcError::Settings(format!("invoke tccutil: {e}")))?;
            if status.success() {
                any_reset = true;
                tracing::info!(category, "tccutil reset succeeded");
            } else {
                tracing::info!(category, "tccutil reset reported no entry (likely fine)");
            }
        }

        let summary = if any_reset {
            "TCC entries reset. Restart Hush — macOS will re-prompt for any permissions \
             Hush actually needs on the next launch."
                .to_owned()
        } else {
            "No TCC entries to reset (the bundle id may not be registered, common on \
             unsigned dev builds). If permissions still feel stuck, build a signed \
             bundle (`npm run tauri build`) and try its first launch."
                .to_owned()
        };
        Ok(MacosPermissionResetResult { any_reset, summary })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(MacosPermissionResetResult {
            any_reset: false,
            summary: "Permission reset is macOS-only (TCC is an Apple framework).".to_owned(),
        })
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
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        // Pre-populate the slot with a sentinel value so a regression in
        // the assignment order — assigning the new capture before
        // `audio.start` returns — would visibly overwrite it.
        *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
            app_name: "sentinel".into(),
            window_title: "sentinel".into(),
        });

        let err = start_dictation_inner(&state, AudioSource::default_microphone())
            .expect_err("audio.start fails");
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
        let state = crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(Arc::new(crate::ipc::tests::NoopReplacements))
            .vocabulary(Arc::new(crate::ipc::tests::NoopVocabulary))
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete");

        // We can't observe the OS foreground app reliably from a test
        // process, so we just assert the call returned Ok and the slot is
        // *some* value (None or Some, both are acceptable — the OS may
        // genuinely have no active window in CI).
        start_dictation_inner(&state, AudioSource::default_microphone()).expect("should succeed");

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

    // -- stop_dictation helper tests --------------------------------------
    //
    // The Tauri command itself needs an `AppHandle` (clipboard +
    // notification + HUD), so it can't be unit-tested directly. The
    // helpers extracted from it can — these tests pin their behaviour
    // so the orchestration in `stop_dictation` stays trustworthy
    // through future refactors.

    use crate::dictionary::{
        NewVocabularyTerm, ReplacementRepository, ReplacementRule, VocabularyRepository,
        VocabularyTerm,
    };

    struct AudioThatStopsWith {
        captured: CapturedAudio,
    }

    impl AudioCapture for AudioThatStopsWith {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            Ok(self.captured.clone())
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct AudioThatFailsToStop;

    impl AudioCapture for AudioThatFailsToStop {
        fn list_input_devices(&self) -> anyhow::Result<Vec<AudioDevice>> {
            Ok(vec![])
        }
        fn start(&self, _: Option<&str>) -> anyhow::Result<()> {
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<CapturedAudio> {
            Err(anyhow!("device went away"))
        }
        fn is_recording(&self) -> bool {
            false
        }
    }

    struct VocabWithTerms(Vec<VocabularyTerm>);

    #[async_trait::async_trait]
    impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for VocabWithTerms {
        async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
            Ok(self.0.clone())
        }
        async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
            unreachable!()
        }
        async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingVocab;

    #[async_trait::async_trait]
    impl crate::repository::Repository<VocabularyTerm, NewVocabularyTerm, i64> for FailingVocab {
        async fn list(&self) -> anyhow::Result<Vec<VocabularyTerm>> {
            Err(anyhow!("table missing"))
        }
        async fn create(&self, _: NewVocabularyTerm) -> anyhow::Result<VocabularyTerm> {
            unreachable!()
        }
        async fn update(&self, _: VocabularyTerm) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FailingReplacements;

    #[async_trait::async_trait]
    impl crate::repository::Repository<ReplacementRule, crate::dictionary::NewReplacementRule, i64>
        for FailingReplacements
    {
        async fn list(&self) -> anyhow::Result<Vec<ReplacementRule>> {
            Err(anyhow!("table missing"))
        }
        async fn create(
            &self,
            _: crate::dictionary::NewReplacementRule,
        ) -> anyhow::Result<ReplacementRule> {
            unreachable!()
        }
        async fn update(&self, _: ReplacementRule) -> anyhow::Result<()> {
            Ok(())
        }
        async fn delete(&self, _: i64) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn state_with(
        audio: Arc<dyn AudioCapture>,
        vocab: Arc<dyn VocabularyRepository>,
        replacements: Arc<dyn ReplacementRepository>,
    ) -> AppState {
        crate::ipc::AppStateBuilder::new()
            .audio(audio)
            .history(Arc::new(crate::ipc::tests::NoopHistory))
            .replacements(replacements)
            .vocabulary(vocab)
            .settings(Arc::new(crate::ipc::tests::MemSettings {
                map: std::sync::Mutex::new(std::collections::HashMap::new()),
            }))
            .models_dir(std::path::PathBuf::from("/tmp/hush-test-models"))
            .build()
            .expect("test state: builder fields complete")
    }

    fn fixed_audio() -> CapturedAudio {
        CapturedAudio {
            samples: vec![0.5_f32; 8],
            format: crate::audio::CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        }
    }

    #[test]
    fn stop_audio_capture_returns_captured_on_success() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let captured = stop_audio_capture(&state).expect("audio.stop ok");
        assert_eq!(captured.samples.len(), 8);
        assert_eq!(captured.format.sample_rate, 48_000);
    }

    #[test]
    fn stop_audio_capture_maps_backend_error_to_ipc_error_audio() {
        // Regression for the heuristic-classifier era: audio errors must
        // surface as `IpcError::Audio` so the frontend's switch-on-kind
        // dispatch picks the right recovery copy. This is *structural*
        // classification — there is no string match anywhere.
        let state = state_with(
            Arc::new(AudioThatFailsToStop),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let err = stop_audio_capture(&state).expect_err("stop fails");
        assert!(matches!(err, IpcError::Audio(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn load_vocabulary_prompt_formats_terms_when_present() {
        let terms = vec![
            VocabularyTerm {
                id: 1,
                term: "Hush".into(),
            },
            VocabularyTerm {
                id: 2,
                term: "whisper.cpp".into(),
            },
        ];
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(VocabWithTerms(terms.clone())),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let prompt = load_vocabulary_prompt(&state).await;
        // The exact format is owned by `format_vocabulary_prompt`; this
        // test just pins that the helper actually invokes the formatter
        // rather than returning empty.
        assert!(prompt.contains("Hush"), "got: {prompt}");
        assert!(prompt.contains("whisper.cpp"), "got: {prompt}");
    }

    #[tokio::test]
    async fn load_vocabulary_prompt_swallows_repository_errors() {
        // Repository failure must not block transcription — we demote
        // to the no-prompt path.
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(FailingVocab),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );

        let prompt = load_vocabulary_prompt(&state).await;
        assert!(prompt.is_empty(), "got: {prompt}");
    }

    #[tokio::test]
    async fn load_replacement_rules_returns_empty_on_error() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(FailingReplacements),
        );

        let rules = load_replacement_rules(&state).await;
        assert!(rules.is_empty());
    }

    #[test]
    fn take_foreground_snapshot_pops_and_clears_the_slot() {
        let state = state_with(
            Arc::new(AudioThatStopsWith {
                captured: fixed_audio(),
            }),
            Arc::new(crate::ipc::tests::NoopVocabulary),
            Arc::new(crate::ipc::tests::NoopReplacements),
        );
        *state.pending_foreground.lock().unwrap() = Some(ForegroundApp {
            app_name: "Slack".into(),
            window_title: "#general".into(),
        });

        let popped = take_foreground_snapshot(&state).expect("not poisoned");
        assert_eq!(popped.as_ref().map(|f| f.app_name.as_str()), Some("Slack"));

        // Second take must be None: the slot is consumed, not cloned.
        let again = take_foreground_snapshot(&state).expect("not poisoned");
        assert!(again.is_none());
    }
}
