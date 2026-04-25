//! SQLite persistence layer.
//!
//! Wraps a [`sqlx::SqlitePool`] and bakes the embedded migrations from
//! `src-tauri/migrations/` into the binary at compile time via
//! [`sqlx::migrate!`]. Concrete domain queries (history, dictionary,
//! settings) live in their own modules and borrow the pool from
//! [`SqliteDatabase::pool`].
//!
//! ## Why a thin wrapper rather than passing `SqlitePool` directly
//!
//! - Centralises connect-time configuration: WAL journal mode, foreign-key
//!   enforcement, and any future PRAGMAs (cache sizing, busy timeout) are
//!   set in one place rather than repeated at every call site.
//! - Gives migrations a single, predictable home — the [`SqliteDatabase::open`]
//!   constructor runs them, so callers cannot accidentally use an unmigrated
//!   pool.
//! - Provides [`SqliteDatabase::open_in_memory`] for tests in higher
//!   layers (history, dictionary) without forcing each test to repeat the
//!   PRAGMA / migration setup.
//!
//! ## Why WAL
//!
//! Hush concurrently reads (history view, settings hot-reload) while a
//! transcription is being inserted. SQLite's default `DELETE` journal
//! mode serialises readers behind a writer; WAL lets readers proceed
//! against the previous snapshot while a writer appends. The cost is an
//! extra `-wal` and `-shm` sidecar file next to the db on disk, which is
//! a non-issue for a desktop app whose db lives in the platform
//! app-data directory anyway.
//!
//! ## Why foreign keys are forced ON
//!
//! SQLite's foreign-key enforcement is opt-in per connection (a long-
//! standing default-off footgun). We enable it on every connection via
//! the connect options so the schema's referential integrity is actually
//! enforced.
//!
//! ## Test seam (PRD §13.5)
//!
//! Higher layers will eventually depend on per-domain repository traits
//! (history, dictionary, settings) that this module's pool feeds. Those
//! traits will define the mockable seams. For now there is just the
//! database struct itself, since no methods exist beyond `open` /
//! `open_in_memory` for callers to mock against.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, SqlitePool};

/// Embedded migrations baked into the binary at compile time. The macro
/// reads `src-tauri/migrations/` (path is relative to `Cargo.toml`) and
/// emits a static [`sqlx::migrate::Migrator`] we can run against any pool.
static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// SQLite-backed database handle. Owns the pool; one is built at app
/// startup and shared via `tauri::State` for the process lifetime.
pub struct SqliteDatabase {
    pool: SqlitePool,
}

impl SqliteDatabase {
    /// Open (or create) the database file at `path` and run all embedded
    /// migrations.
    ///
    /// The parent directory is created if missing. WAL journal mode and
    /// foreign-key enforcement are set up before any connection is handed
    /// out, so callers never see an unmigrated or under-configured pool.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be created, the
    /// database file cannot be opened (permissions, disk full), or any
    /// migration fails to apply.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            // `tokio::fs` is overkill for a single mkdir on app startup —
            // `std::fs::create_dir_all` is synchronous but the cost is
            // negligible and we side-step a tokio runtime requirement on
            // the caller for this one operation.
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create db parent dir {}", parent.display()))?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // `Normal` is the recommended pairing with WAL: durable across
            // app crashes (which is what we care about), only at risk on
            // power loss between commit and the next checkpoint. The
            // default `Full` is overkill for a dictation history.
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            // sqlx logs every query at `info` by default, which buries
            // the dictation-shaped log lines under query-shaped noise.
            // Bump down to `debug` so they only show with explicit opt-in.
            .log_statements(log::LevelFilter::Debug);

        let pool = build_pool(opts).await?;
        Ok(Self { pool })
    }

    /// Open an in-memory database with the same migrations applied.
    /// Intended for tests in this crate (and downstream domain modules
    /// once history / dictionary acquire methods).
    ///
    /// SQLite's `:memory:` database is per-connection by default, so we
    /// pin the pool to a single connection — otherwise different pool
    /// connections each get their own, empty, in-memory database and
    /// the migrations only land in one of them.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .context("parse :memory: connect string")?
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .connect_with(opts)
            .await
            .context("open in-memory sqlite pool")?;

        MIGRATIONS
            .run(&pool)
            .await
            .context("run migrations on in-memory pool")?;

        Ok(Self { pool })
    }

    /// Borrow the underlying pool for query execution.
    ///
    /// Domain modules execute queries directly against this — there is
    /// no indirection through this struct other than the connect-time
    /// configuration done in `open`.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Build the production pool with default sizing. Pulled out so `open`
/// stays focused on path resolution and the pool tuning lives in one
/// place when we need to revisit it.
async fn build_pool(opts: SqliteConnectOptions) -> Result<SqlitePool> {
    // 4 max connections: the app is single-user with at most a few
    // concurrent operations (history list + active transcription insert
    // + settings reload). Bumping this would only mask a synchronisation
    // bug in the domain layers, not actually unlock parallelism.
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .context("open sqlite pool")?;

    MIGRATIONS.run(&pool).await.context("run migrations")?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_to_in_memory_pool() {
        let db = SqliteDatabase::open_in_memory()
            .await
            .expect("in-memory db must open");

        // The migration is the source of truth for the schema; assert
        // the four base tables landed plus the FTS5 virtual table. If
        // someone adds a table to migration 0001, this list updates.
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master \
             WHERE type IN ('table') AND name NOT LIKE 'sqlite_%' \
             AND name NOT LIKE '_sqlx_%' \
             ORDER BY name",
        )
        .fetch_all(db.pool())
        .await
        .expect("query sqlite_master");

        for expected in [
            "history",
            "dictionary_terms",
            "replacements",
            "settings",
            "history_fts",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}: got {tables:?}"
            );
        }
    }

    #[tokio::test]
    async fn foreign_keys_pragma_is_on() {
        // SQLite's foreign-key enforcement defaults to OFF unless
        // explicitly requested per connection. Production code relies
        // on this being ON; regressing the connect options would
        // silently break referential integrity rather than fail loudly.
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .expect("query foreign_keys pragma");
        assert_eq!(fk, 1);
    }

    #[tokio::test]
    async fn fts5_index_finds_inserted_history_row() {
        // Smoke-tests two things at once: (1) the FTS5 virtual table
        // and the AFTER INSERT trigger from migration 0001 actually
        // wire up at runtime against the bundled SQLite, and (2) a
        // row inserted into `history` is searchable through the
        // `history_fts` virtual table without extra setup. If either
        // regresses, history search (TODO(#7)) will silently return
        // zero rows.
        let db = SqliteDatabase::open_in_memory().await.unwrap();

        sqlx::query("INSERT INTO history (transcript, app_name, model) VALUES (?, ?, ?)")
            .bind("the quick brown fox jumps over the lazy dog")
            .bind("TestApp")
            .bind("base")
            .execute(db.pool())
            .await
            .expect("insert history");

        let hits: Vec<String> =
            sqlx::query_scalar("SELECT transcript FROM history_fts WHERE history_fts MATCH ?")
                .bind("brown fox")
                .fetch_all(db.pool())
                .await
                .expect("fts match query");

        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("brown fox"));
    }

    #[tokio::test]
    async fn opens_database_at_filesystem_path_and_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("hush.db");

        {
            let db = SqliteDatabase::open(&path).await.expect("open new db");
            sqlx::query("INSERT INTO history (transcript, model) VALUES ('persisted', 'base')")
                .execute(db.pool())
                .await
                .expect("insert");
        }

        // Re-open the same file: the row from the first session must
        // still be there, proving the WAL + normal-sync configuration
        // actually commits to disk on connection close (not just on a
        // timed checkpoint), which is what we want for a desktop app
        // whose history is the user's artefact.
        let db = SqliteDatabase::open(&path).await.expect("re-open db");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM history WHERE transcript = 'persisted'")
                .fetch_one(db.pool())
                .await
                .expect("count");

        assert_eq!(count, 1);
    }
}
