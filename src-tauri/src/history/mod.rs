//! Transcription history — paginated list, full-text search, delete.
//!
//! Concept inspired by VoiceInk's history view. Reimplemented from
//! observed public behaviour; no source code referenced. See §13.8 of the
//! PRD.
//!
//! ## Responsibilities
//!
//! - Persist a row per successful transcription, with the focused-app
//!   metadata captured at recording start by the IPC layer.
//! - Provide paginated list and FTS5 search queries that the UI binds
//!   directly to a history view.
//! - Allow individual rows to be deleted.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on the [`HistoryRepository`] trait, never on the
//! [`SqliteHistoryRepository`] type, so unit tests of the IPC layer can
//! plug in a deterministic mock without spinning up SQLite. The trait is
//! `async` (because the SQLite-backed impl does async I/O); we use
//! `async-trait` to keep the trait object-safe.
//!
//! ## Out of scope (deferred to follow-up PRs)
//!
//! - **CSV export.** Needs the `tauri-plugin-dialog` save-file picker
//!   wired through the frontend. Cleaner as a separate change so the
//!   list/search/delete surface lands first.
//! - **Filter by foreground app.** Schema supports it (`app_name` is on
//!   the row); UI/query work follows the first cut once we know what
//!   the filter UX should look like.
//! - **Retention policies / pruning.** Not yet decided what default
//!   retention to apply; deferred to settings (M3).

pub mod sqlite;

pub use sqlite::SqliteHistoryRepository;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A persisted history row.
///
/// Mirrors the migration-0001 `history` table. Field types match the
/// SQLite-storage types: timestamps are kept as the ISO-8601 strings
/// SQLite generates, not parsed `chrono::DateTime`s, because the only
/// consumer is the frontend formatter and shipping a date-time crate
/// just to do `.to_rfc3339()` round-trip would be overkill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub transcript: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub model: String,
    pub duration_ms: Option<i64>,
    /// ISO-8601 UTC, e.g. `2026-04-25T14:32:11Z`. Populated by the
    /// SQLite default `strftime('%Y-%m-%dT%H:%M:%SZ', 'now')` so callers
    /// don't have to provide it.
    pub created_at: String,
}

/// Fields callers supply when inserting a new row. Separate from
/// [`HistoryEntry`] so the database-generated id and timestamp can't be
/// accidentally hand-rolled.
#[derive(Debug, Clone)]
pub struct NewHistoryEntry {
    pub transcript: String,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    pub model: String,
    pub duration_ms: Option<i64>,
}

/// Repository trait at the storage boundary.
///
/// `Send + Sync` so the IPC layer can hold an `Arc<dyn HistoryRepository>`
/// across async Tauri commands. Object-safe via `async-trait`.
#[async_trait]
pub trait HistoryRepository: Send + Sync {
    /// Insert a new row and return its generated id.
    async fn insert(&self, entry: NewHistoryEntry) -> Result<i64>;

    /// Paginated list, newest first. `limit` is hard-capped to a
    /// reasonable upper bound by the implementation so a misbehaving
    /// caller cannot accidentally pull the whole table.
    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>>;

    /// FTS5 search over the transcript text, newest match first. Empty
    /// `query` falls through to [`HistoryRepository::list`]; whitespace-
    /// only queries also fall through to keep the UI's "type to filter"
    /// pattern simple to wire up.
    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>>;

    /// Delete a single row by id. A no-op (returns `Ok`) if the id does
    /// not exist — the caller's expressed intent has been satisfied
    /// either way, and surfacing the not-found case as an error would
    /// just force every UI call site to ignore it.
    async fn delete(&self, id: i64) -> Result<()>;

    /// Total row count (no filter). Used by the frontend to drive
    /// pagination state ("page 3 of 12") without paging back to the end.
    async fn count(&self) -> Result<i64>;
}
