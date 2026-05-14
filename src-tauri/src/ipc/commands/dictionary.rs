//! Vocabulary + replacement-rule CRUD IPC commands (#431).
//!
//! Lifted out of the [`super`] mega-module so the per-domain
//! command surface lives in a peer file the way `meeting.rs`,
//! `models.rs`, and `macos.rs` already do.
//!
//! ## Why both subsystems share one module
//!
//! The frontend renders vocabulary terms (Settings → Vocabulary)
//! and replacement rules (Settings → Replacements) under one
//! "Dictionary" mental model: pre-transcription priming
//! (vocabulary) + post-transcription editing (replacements). Both
//! errors funnel through `IpcError::Replacements` for the same
//! reason — the user sees one combined error switch, not two
//! near-identical branches that drift over time. Keeping the
//! commands together in a single peer module mirrors that
//! grouping.
//!
//! ## Registration
//!
//! Each `#[tauri::command]` is registered in
//! `src-tauri/src/lib.rs` via its full path
//! (`ipc::commands::dictionary::vocabulary_list`, etc.). `pub use`
//! re-exports do not carry the macro's hidden `__cmd__<name>`
//! symbol — see `learnings.md` 2026-04-25.

use tauri::State;

use crate::dictionary::{NewReplacementRule, NewVocabularyTerm, ReplacementRule, VocabularyTerm};

use super::super::AppState;
use super::{IpcError, IpcResult};

// -- Replacement rules ----------------------------------------------------

/// All replacement rules in `(sort_order, id)` order.
#[tauri::command]
pub async fn replacements_list(state: State<'_, AppState>) -> IpcResult<Vec<ReplacementRule>> {
    state
        .data
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
        .data
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
        .data
        .replacements
        .update(rule)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a single replacement. No-op if `id` does not exist.
#[tauri::command]
pub async fn replacement_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .replacements
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Vocabulary CRUD ------------------------------------------------------
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
        .data
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
        .data
        .vocabulary
        .create(NewVocabularyTerm { term })
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Update an existing vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_update(state: State<'_, AppState>, term: VocabularyTerm) -> IpcResult<()> {
    state
        .data
        .vocabulary
        .update(term)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Delete a vocabulary term. No-op if `id` does not exist.
#[tauri::command]
pub async fn vocabulary_delete(state: State<'_, AppState>, id: i64) -> IpcResult<()> {
    state
        .data
        .vocabulary
        .delete(id)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Preset packs ----------------------------------------------------------
//
// Pack contents are **static** — compiled into the binary. Only the list of
// enabled pack slugs is persisted in the settings table. Commands here
// read/write that slug list; the pack vocabulary and replacement rules are
// applied at transcription time in `dictation/pipeline.rs`.

use crate::dictionary::packs::{self, PackDescriptor};
use crate::settings;

/// Wire shape returned by `list_packs`.
///
/// Extends [`PackDescriptor`] with an `enabled` boolean derived from the
/// stored enabled-pack-slugs setting. Using a dedicated wire type keeps
/// the frontend from reasoning about the difference between "no packs
/// setting row yet" and "empty list".
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackStatus {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub vocabulary_count: usize,
    pub replacement_count: usize,
    pub enabled: bool,
}

impl PackStatus {
    fn from_descriptor(desc: &'static PackDescriptor, enabled: bool) -> Self {
        PackStatus {
            slug: desc.slug.to_owned(),
            name: desc.name.to_owned(),
            description: desc.description.to_owned(),
            vocabulary_count: desc.vocabulary.len(),
            replacement_count: desc.replacements.len(),
            enabled,
        }
    }
}

/// All built-in preset packs with their current enabled/disabled state.
#[tauri::command]
pub async fn list_packs(state: State<'_, AppState>) -> IpcResult<Vec<PackStatus>> {
    let enabled = load_enabled_slugs(&state).await?;
    Ok(packs::all_packs()
        .iter()
        .map(|p| PackStatus::from_descriptor(p, enabled.contains(&p.slug.to_owned())))
        .collect())
}

/// Enable a preset pack. Adds the slug to the enabled-packs setting if
/// it is not already present. No-op if the slug is not a known pack.
#[tauri::command]
pub async fn enable_pack(state: State<'_, AppState>, slug: String) -> IpcResult<()> {
    if packs::find_pack(&slug).is_none() {
        return Err(IpcError::Replacements(format!("unknown pack: {slug}")));
    }
    let mut enabled = load_enabled_slugs(&state).await?;
    if !enabled.contains(&slug) {
        enabled.push(slug);
        save_enabled_slugs(&state, &enabled).await?;
    }
    Ok(())
}

/// Disable a preset pack. Removes the slug from the enabled-packs setting.
/// No-op if the pack was not enabled.
#[tauri::command]
pub async fn disable_pack(state: State<'_, AppState>, slug: String) -> IpcResult<()> {
    let mut enabled = load_enabled_slugs(&state).await?;
    let before = enabled.len();
    enabled.retain(|s| s != &slug);
    if enabled.len() != before {
        save_enabled_slugs(&state, &enabled).await?;
    }
    Ok(())
}

async fn load_enabled_slugs(state: &AppState) -> IpcResult<Vec<String>> {
    match state
        .settings
        .get(crate::settings::keys::ENABLED_PACKS)
        .await
    {
        Ok(Some(json)) => Ok(serde_json::from_str::<Vec<String>>(&json).unwrap_or_default()),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(IpcError::Replacements(e.to_string())),
    }
}

async fn save_enabled_slugs(state: &AppState, slugs: &[String]) -> IpcResult<()> {
    let json = serde_json::to_string(slugs)
        .map_err(|e| IpcError::Replacements(format!("serialize pack slugs: {e}")))?;
    state
        .settings
        .set(settings::keys::ENABLED_PACKS, &json)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

// -- Language style --------------------------------------------------------

/// Get the stored language style preference.
///
/// Returns `"american"` if the setting is absent or unrecognised — that is
/// Whisper's default behaviour and the product default for Hush.
#[tauri::command]
pub async fn get_language_style(state: State<'_, AppState>) -> IpcResult<String> {
    let style = state
        .settings
        .get(settings::keys::LANGUAGE_STYLE)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))?
        .unwrap_or_default();
    Ok(normalise_language_style(&style).to_owned())
}

/// Set the language style preference.
///
/// Accepted values: `"american"`, `"british"`, `"oxford"`. Any other
/// value is rejected with an error so the frontend can't silently persist
/// garbage that later defaults to American silently.
#[tauri::command]
pub async fn set_language_style(state: State<'_, AppState>, style: String) -> IpcResult<()> {
    if !["american", "british", "oxford"].contains(&style.as_str()) {
        return Err(IpcError::Replacements(format!(
            "invalid language style {style:?}; expected american, british, or oxford"
        )));
    }
    state
        .settings
        .set(settings::keys::LANGUAGE_STYLE, &style)
        .await
        .map_err(|e| IpcError::Replacements(e.to_string()))
}

/// Normalise a stored style slug to a known value, defaulting to
/// `"american"` for anything unrecognised (including the empty string
/// from an absent row).
fn normalise_language_style(stored: &str) -> &'static str {
    match stored {
        "british" => "british",
        "oxford" => "oxford",
        _ => "american",
    }
}
