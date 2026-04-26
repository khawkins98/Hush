//! Generic CRUD repository trait shared by domain repositories with
//! symmetric semantics.
//!
//! ## Why this exists
//!
//! Several per-domain repository traits in this crate
//! ([`crate::dictionary::ReplacementRepository`],
//! [`crate::dictionary::VocabularyRepository`]) had four near-identical
//! method signatures: `list`, `create`, `update`, `delete`. Each
//! re-declared the shape, and a future fifth repository (e.g. a
//! model-state store) would re-declare them yet again. Pulling the
//! shared shape into [`Repository`] forces naming consistency (no more
//! `insert` vs `create` drift) and lets a future addition opt into the
//! convention with one trait bound rather than four method signatures.
//!
//! ## Why not all repositories inherit
//!
//! - [`crate::history::HistoryRepository`] has a fundamentally
//!   different surface: paginated `list(limit, offset)`, plus
//!   domain-specific `search` and `count`, and no `update` at all
//!   (history rows are append-only by design). Forcing it under
//!   [`Repository`] would mean either degrading its list signature or
//!   adding a stub `update` that just returns `Ok(())`. Both are worse
//!   than letting history have its own trait.
//! - [`crate::settings::SettingsRepository`] is a key/value store
//!   (`get`/`set`/`remove`), not a CRUD-of-rows store. It deliberately
//!   stays its own trait — wrapping it under a `Repository<T>` would
//!   be the kind of premature unification this design avoids.
//!
//! ## Why not also extract a `SqliteRepository<T>`
//!
//! Each per-domain SQLite impl has bespoke schema (different columns,
//! different `RETURNING` shapes, different `ORDER BY` rules). A generic
//! SQLite layer would need either a row mapper passed in everywhere or
//! a macro, both of which are more friction than the four small
//! near-identical impl blocks they would replace. See PR #88's design
//! note for the trade-off.

use anyhow::Result;
use async_trait::async_trait;

/// Generic CRUD repository.
///
/// Type parameters:
/// - `T` — the persisted row type (with id and any other DB-assigned fields).
/// - `NewT` — the caller-supplied "new row" shape, separate from `T` so the
///   database-generated id can't be accidentally hand-rolled.
/// - `Id` — the row identifier type. In practice this is always `i64` for
///   SQLite-backed repositories, but the parameter keeps the trait open
///   for a future repo keyed by something else (e.g. a model id string).
///
/// Implementations are expected to be `Send + Sync` so the IPC layer can
/// hold them as `Arc<dyn …>` across async Tauri commands. Object-safety
/// is provided by `async-trait`.
#[async_trait]
pub trait Repository<T, NewT, Id>: Send + Sync {
    /// All rows. No pagination — implementors of this trait are expected
    /// to back small collections (handful to low hundreds of rows). A
    /// repository that holds an unbounded collection should NOT implement
    /// this trait; it should define its own paginated `list` instead
    /// (see [`crate::history::HistoryRepository`]).
    async fn list(&self) -> Result<Vec<T>>;

    /// Insert a new row and return the persisted shape (with its
    /// assigned id) so the frontend can append it to its local list
    /// without an extra round-trip.
    async fn create(&self, new: NewT) -> Result<T>;

    /// Update an existing row's fields. No-op (returns `Ok`) if the id
    /// does not exist — the caller's expressed intent (this row should
    /// hold these values) is satisfied either way, and surfacing the
    /// not-found case as an error would just force every UI call site
    /// to ignore it.
    async fn update(&self, item: T) -> Result<()>;

    /// Delete a single row. Same no-op-on-missing semantics as
    /// [`Repository::update`].
    async fn delete(&self, id: Id) -> Result<()>;
}
