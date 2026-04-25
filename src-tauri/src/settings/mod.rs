//! Generic key-value settings persisted in SQLite.
//!
//! Backs the `settings` table from migration 0001 (`key TEXT PRIMARY KEY,
//! value TEXT NOT NULL`). Higher layers (e.g. the model picker, hotkey
//! rebind UI) read and write their own keys directly — there is no
//! per-setting typed wrapper because every setting we ship is a small
//! number of strings or stringly-encoded values, and a typed wrapper for
//! each would balloon the surface for no real safety win.
//!
//! ## Why a generic K/V store rather than a structured config file
//!
//! - Migrating a structured `config.toml` between versions requires
//!   careful schema versioning. SQLite already gives us versioning via
//!   migrations, and rows can be added/removed without touching old
//!   keys.
//! - Reading settings concurrently with other db work (history insert,
//!   etc.) is naturally serialised by the WAL pool we already share.
//! - The first non-trivial setting — the model picker's
//!   `selected_model_id` — is a single string. Bringing in `serde` or
//!   `figment` for it would be premature.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers depend on the [`SettingsRepository`] trait, never on
//! [`SqliteSettingsRepository`] directly, so unit tests of consumers
//! can substitute a deterministic mock without touching SQLite.

pub mod sqlite;

pub use sqlite::SqliteSettingsRepository;

use anyhow::Result;
use async_trait::async_trait;

/// Settings keys consumed by the app. Centralising them so a typo in
/// one call site doesn't silently miss a stored value (the next call
/// would just see `None` and behave as if nothing was set).
pub mod keys {
    /// ID of the model the user picked in the model picker. Format is
    /// the catalog id (e.g. `whisper-base`); resolution to a filesystem
    /// path happens in the transcription module's setup.
    pub const SELECTED_MODEL_ID: &str = "selected_model_id";

    /// Marks that the macOS first-run welcome flow has been shown and
    /// dismissed. Stored as the literal string `"true"` once set; any
    /// other value (including absent) means "show the welcome on
    /// next launch". Per-platform behaviour: only the macOS frontend
    /// reads this — Linux/Windows never check.
    pub const FIRST_RUN_COMPLETED: &str = "first_run_completed";
}

/// Repository trait at the storage boundary.
///
/// `Send + Sync` so the IPC layer holds an `Arc<dyn SettingsRepository>`
/// across async Tauri commands; object-safe via `async-trait` for parity
/// with the other repositories in the codebase.
#[async_trait]
pub trait SettingsRepository: Send + Sync {
    /// Read a single setting. Returns `None` if the key has never been
    /// written rather than treating "absent" as a value, so callers can
    /// distinguish "user never set this" from "user set this to empty".
    async fn get(&self, key: &str) -> Result<Option<String>>;

    /// Write (or overwrite) a setting. The store has no notion of
    /// types — values are persisted verbatim and the caller is
    /// responsible for any serialisation.
    async fn set(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a setting. No-op if `key` does not exist, mirroring the
    /// other repository delete contracts.
    async fn remove(&self, key: &str) -> Result<()>;
}
