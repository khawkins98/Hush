//! Dictation-pipeline orchestration helpers.
//!
//! Extracted from `commands/dictation/mod.rs` under #597 (item 9). No
//! behaviour change.
//!
//! Mirrors the shape of [`crate::ipc::pipeline`] (which holds the
//! cross-cutting redirect-policy + transcriber-loader + run-pipeline
//! orchestration): the command shells in `mod.rs` (`start_dictation`,
//! `stop_dictation`, `audio_list_sources`) call into the helpers here
//! so the IPC handlers stay thin and the orchestration steps are
//! discoverable as named functions.
//!
//! Items are `pub(super)` — visible to the parent `dictation` module
//! (`mod.rs`) and its test module — but not exposed outside the
//! `dictation` namespace. If a consumer outside `commands::dictation`
//! ever needs one of these, promote it with the relevant rename
//! discipline first; the current visibility is what makes the
//! "command shell vs pipeline" split structurally meaningful.

use std::sync::Arc;

use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_notification::NotificationExt;

use crate::audio::AudioSource;
use crate::dictionary::{
    ReplacementRule, VocabularyTerm,
};
use crate::dictionary::vocabulary::format_initial_prompt;
use crate::history::NewHistoryEntry;
use crate::ipc::AppState;

use super::super::ForegroundApp;
use super::super::{classify_permission_error, poisoned, IpcError, IpcResult};

/// Body of `start_dictation`: pre-flight transcriber-loaded check,
/// foreground snapshot, mic-permission probe, and audio-backend
/// start. The `start_dictation` command shell in `mod.rs` is a thin
/// wrapper that calls this and returns its result.
pub(super) fn start_dictation_inner(state: &AppState, source: AudioSource) -> IpcResult<()> {
    // Pre-flight: refuse to open audio capture when no transcriber is
    // loaded. Pre-#195 this check lived only in `stop_dictation`, so a
    // user with no model would record audio (HUD up, mic hot, level
    // meter dancing), press Stop, and *only then* see the
    // "no transcription model loaded" error. The recording is wasted
    // — we have audio bytes nobody will ever transcribe — and the
    // user has spent N seconds waiting for an outcome that was never
    // possible. Fail fast at start so the error surfaces before
    // anyone speaks. The frontend's model-loaded banner gates the
    // Start button visually; this is the structural backstop for the
    // hotkey path that bypasses button gating.
    {
        let guard = state.transcribe.lock().map_err(poisoned)?;
        if guard.is_none() {
            return Err(IpcError::TranscriptionUnavailable);
        }
    }

    let foreground = capture_foreground();

    // Upfront mic-permission probe (#417). cpal's actual macOS-
    // mic-denial chain reads "Audio Unit: kAudioUnitErr_…" with
    // no "microphone" substring, so the post-call classifier in
    // the `.map_err` below rarely catches mic denials. Instead,
    // ask AVAuthorizationStatus directly before touching cpal:
    // if it's Denied (which `permissions` also normalises
    // Restricted into for UX purposes), surface the typed
    // variant upfront. NotDetermined falls through so the OS
    // prompt fires on the actual cpal call (the user hasn't
    // been asked yet). Same shape `meeting_start_manual` uses
    // post-#416 for SCK; this is the analogue for the dictation
    // mic path.
    //
    // Cfg-gated to macOS because the AVAuthorizationStatus
    // surface only exists there; on Linux/Windows the cpal
    // failure chain carries the platform-native diagnostic and
    // the post-call classifier handles it.
    #[cfg(target_os = "macos")]
    if matches!(source, AudioSource::Microphone(_)) {
        let mic_status = crate::permissions::read_all().microphone;
        if matches!(mic_status, crate::permissions::PermissionStatus::Denied) {
            return Err(IpcError::PermissionDenied("microphone".to_owned()));
        }
    }

    state.audio.start_with_source(source).map_err(|e| {
        // Promote permission-shaped chains to the typed
        // `PermissionDenied` variant (#386 / #416 close-out).
        // Mirrors the meeting_start_manual pattern so the
        // dictation start path goes through the same
        // discriminant the frontend can switch on.
        //
        // The mic arm of the classifier rarely fires from here
        // (#417): cpal's actual mic-denial chain doesn't
        // contain "microphone" or "not authorized", so this
        // branch mostly catches the SCK case (system-audio
        // source). Mic Denied is now caught upfront via the
        // AVAuthorizationStatus probe above; this stays
        // defensive for any future cpal text change AND for
        // the SCK case when the dictation source is
        // system-audio.
        if let Some(perm) = classify_permission_error(&e) {
            IpcError::PermissionDenied(perm.to_owned())
        } else {
            IpcError::Audio(e.to_string())
        }
    })?;

    *state.pending_foreground.lock().map_err(poisoned)? = foreground;

    Ok(())
}

/// Strip whisper-style placeholder tokens from the transcribed text
/// (`[BLANK_AUDIO]`, `[SOUND]`, etc.). Whisper emits these as
/// literal bracketed tokens; pasting them into the user's editor
/// would surface the model's internal vocabulary as transcript noise.
pub(super) fn strip_whisper_brackets(input: &str) -> String {
    // Build the output one char at a time; skip anything inside `[…]`.
    // The brackets are always single-line in whisper's output, so a
    // simple bracket-depth counter is enough — no need for a regex
    // dep just for this.
    let mut out = String::with_capacity(input.len());
    let mut depth: i32 = 0;
    for ch in input.chars() {
        match ch {
            '[' => depth += 1,
            ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    // Collapse whitespace runs introduced by stripped brackets and
    // trim the edges. Splitting on whitespace and re-joining is
    // simpler than walking the string with a state machine and is
    // cheap on the ms-scale strings whisper produces.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stop the audio stream and return the captured samples, mapping the
/// backend error to the right [`IpcError`] variant. Split out so
/// `stop_dictation` can keep its HUD-hide-on-error step a single line.
///
/// Routes [`crate::audio::DeviceLost`] (set by the cpal error
/// callback when the user's selected mic disconnects mid-session,
/// #587) to the typed [`IpcError::AudioDeviceLost`] so the frontend
/// can render a clear "microphone disconnected" message instead of
/// the generic "audio: …" bucket. Other audio failures stay in the
/// `Audio(String)` bucket — that's the residual catch-all for the
/// backend-error space we haven't typed yet.
pub(super) fn stop_audio_capture(state: &AppState) -> IpcResult<crate::audio::CapturedAudio> {
    state.audio.stop().map_err(|e| {
        if let Some(lost) = e.downcast_ref::<crate::audio::DeviceLost>() {
            IpcError::AudioDeviceLost(lost.device.clone())
        } else {
            IpcError::Audio(e.to_string())
        }
    })
}

/// Load the user's vocabulary terms and format them as the initial
/// Whisper prompt. Best-effort: a repository error logs and demotes
/// to the no-prompt path. The decoder treats an empty prompt as a no-op
/// (both via the trait's default `transcribe_with_prompt` and via
/// `set_initial_prompt` itself), so the caller never has to branch.
///
/// Combines three sources (in order of precedence — duplicates are
/// deduplicated by [`format_vocabulary_prompt`] keeping the first
/// occurrence):
/// 1. User's personal vocabulary from the `dictionary_terms` table.
/// 2. Vocabulary from any enabled preset packs (appended after user
///    terms so the user's casing / spelling wins on collision).
/// 3. A language-style prefix prepended before the term list.
pub(super) async fn load_vocabulary_prompt(state: &AppState) -> String {
    // User terms — load first so user spellings win deduplication.
    let mut all_terms: Vec<VocabularyTerm> = match state.data.vocabulary.list().await {
        Ok(terms) => terms,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load vocabulary; skipping prompt-biasing");
            Vec::new()
        }
    };

    // Enabled pack vocabulary — appended after user terms.
    let enabled_slugs = load_enabled_packs(state).await;
    for slug in &enabled_slugs {
        if let Some(pack) = crate::dictionary::packs::find_pack(slug) {
            for &term_str in pack.vocabulary {
                // Use a synthetic id that doesn't collide with any real row
                // (negative ids are never assigned by SQLite's AUTOINCREMENT).
                // format_vocabulary_prompt deduplicates case-insensitively,
                // so pack terms the user already has personally are silently
                // dropped.
                all_terms.push(VocabularyTerm {
                    id: -1,
                    term: term_str.to_owned(),
                });
            }
        }
    }

    let style_prefix = load_language_style_prefix(state).await;
    format_initial_prompt(&style_prefix, &all_terms)
}

/// Load post-transcription find/replace rules. Best-effort: a failure
/// here demotes to "no rules applied" rather than failing the whole
/// dictation. The user already has audio captured and a transcript
/// pending; surfacing a rules-load error as fatal would block them on
/// a strictly-secondary feature.
///
/// Enabled pack replacement rules are prepended (at sort_order −1) so
/// they run before user rules (sort_order 0 default). This means user
/// rules can override or layer on top of pack corrections.
pub(super) async fn load_replacement_rules(state: &AppState) -> Vec<ReplacementRule> {
    let user_rules = match state.data.replacements.list().await {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(error = ?e, "failed to load replacement rules; skipping post-processing");
            Vec::new()
        }
    };

    // Prepend enabled pack replacement rules (sort_order = -1, synthetic
    // ids starting at i64::MIN so they sort before any real DB rows at the
    // same sort_order level).
    let enabled_slugs = load_enabled_packs(state).await;
    let mut all_rules: Vec<ReplacementRule> = Vec::new();
    let mut synthetic_id: i64 = i64::MIN;
    for slug in &enabled_slugs {
        if let Some(pack) = crate::dictionary::packs::find_pack(slug) {
            for &(find, replace) in pack.replacements {
                all_rules.push(ReplacementRule {
                    id: synthetic_id,
                    find_text: find.to_owned(),
                    replace_text: replace.to_owned(),
                    // -1 so pack rules run before user-defined rules at
                    // the default sort_order of 0.
                    sort_order: -1,
                });
                synthetic_id = synthetic_id.saturating_add(1);
            }
        }
    }
    all_rules.extend(user_rules);
    all_rules
}

/// Read the list of enabled pack slugs from the settings table. Returns
/// an empty list on any error so callers can always treat the result as
/// best-effort.
pub(super) async fn load_enabled_packs(state: &AppState) -> Vec<String> {
    match state
        .settings
        .get(crate::settings::keys::ENABLED_PACKS)
        .await
    {
        Ok(Some(json)) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(error = ?e, "failed to load enabled packs; continuing with none");
            Vec::new()
        }
    }
}

/// Read the language style prefix from settings. Returns an empty string
/// for American English (the Whisper default) and for any missing /
/// unrecognised setting value.
pub(super) async fn load_language_style_prefix(state: &AppState) -> String {
    let style = match state
        .settings
        .get(crate::settings::keys::LANGUAGE_STYLE)
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) | Err(_) => String::new(),
    };
    language_style_prefix(&style)
}

/// Map a language style slug to the prompt prefix string.
///
/// Kept as a pure function so it can be unit-tested without a database.
pub(crate) fn language_style_prefix(style: &str) -> String {
    match style {
        "british" => "Use British English spelling.".to_owned(),
        "oxford" => "Use Oxford English spelling.".to_owned(),
        _ => String::new(), // "american" or any unrecognised value → no prefix
    }
}

/// Pop the foreground snapshot captured at `start_dictation`. Returns
/// `None` if the slot is empty (which can happen if a hotkey-driven
/// start raced the snapshot capture). The `Mutex` is fenced via
/// [`poisoned`] so a panicked thread doesn't bring down a Tauri command.
pub(super) fn take_foreground_snapshot(state: &AppState) -> IpcResult<Option<ForegroundApp>> {
    Ok(state.pending_foreground.lock().map_err(poisoned)?.take())
}

/// Write the final text to the system clipboard. Fatal on failure —
/// the clipboard is the user's actual artefact for this dictation.
pub(super) fn write_to_clipboard(app: &AppHandle, text: &str) -> IpcResult<()> {
    app.clipboard()
        .write_text(text.to_owned())
        .map_err(|e| IpcError::Clipboard(e.to_string()))
}

/// Fire the "Ready to paste" courtesy notification. Best-effort: on
/// platforms without a notification daemon (e.g. Linux without
/// dbus/notify-send) we log and continue.
pub(super) fn fire_ready_notification(app: &AppHandle) {
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
pub(super) fn spawn_history_create(
    history: Arc<dyn crate::history::HistoryRepository>,
    entry: NewHistoryEntry,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = history.create(entry).await {
            tracing::error!(error = ?e, "failed to persist transcription to history");
        }
    });
}

/// Snapshot the current foreground window via `active-win-pos-rs`.
///
/// `active-win-pos-rs` exposes a Result with the unit type as its error,
/// which is not particularly informative. We collapse the failure case to
/// `None` because losing the foreground snapshot is a graceful degradation
/// — the dictation still works, history just won't have the per-app
/// metadata for that row.
pub(super) fn capture_foreground() -> Option<ForegroundApp> {
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
