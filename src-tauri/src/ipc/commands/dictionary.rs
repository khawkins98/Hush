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
