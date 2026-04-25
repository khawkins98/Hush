//! SQLite-backed implementation of [`HistoryRepository`].
//!
//! Borrows the pool from a shared [`SqliteDatabase`]; every method is a
//! single round-trip query against the FTS5-equipped `history` table from
//! migration 0001. Pure SQL, no domain logic — the test seam lives in the
//! parent module's trait so consumers can mock above this layer.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;

use super::{HistoryEntry, HistoryRepository, NewHistoryEntry};

/// Hard cap on `limit` parameters. Big enough to back any realistic UI
/// page size, small enough that a misbehaving caller can't accidentally
/// pull tens of thousands of rows in a single round-trip and stall the
/// renderer process.
const MAX_LIMIT: i64 = 500;

/// Concrete repository backed by a [`SqliteDatabase`] pool.
///
/// Cheaply cloneable via the inner `Arc` — typically one is built at app
/// startup and shared via `tauri::State`.
pub struct SqliteHistoryRepository {
    db: Arc<SqliteDatabase>,
}

impl SqliteHistoryRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }

    fn cap(limit: i64) -> i64 {
        // Negative limits would surface as "no rows" from SQLite, which
        // is technically defensible but unhelpful. Treat them as 0.
        limit.clamp(0, MAX_LIMIT)
    }
}

#[async_trait]
impl HistoryRepository for SqliteHistoryRepository {
    async fn insert(&self, entry: NewHistoryEntry) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO history (transcript, app_name, window_title, model, duration_ms) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(entry.transcript)
        .bind(entry.app_name)
        .bind(entry.window_title)
        .bind(entry.model)
        .bind(entry.duration_ms)
        .execute(self.db.pool())
        .await
        .context("insert history row")?;

        Ok(result.last_insert_rowid())
    }

    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>> {
        let limit = Self::cap(limit);
        let offset = offset.max(0);

        sqlx::query_as::<_, HistoryEntry>(
            "SELECT id, transcript, app_name, window_title, model, duration_ms, created_at \
             FROM history \
             ORDER BY id DESC \
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.db.pool())
        .await
        .context("list history")
    }

    async fn search(&self, query: &str, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>> {
        // Empty / whitespace-only searches fall through to the standard
        // list query so the UI's "type to filter" pattern works without
        // a special "if box is empty, fetch list, else search" branch.
        if query.trim().is_empty() {
            return self.list(limit, offset).await;
        }

        let limit = Self::cap(limit);
        let offset = offset.max(0);

        // Wrap the user's query in double quotes so FTS5 treats it as a
        // phrase (escaping any double quotes the user typed). Without
        // this, a query like `foo OR bar` would be parsed as the FTS5
        // `OR` operator — which we may want to expose later, but right
        // now the UI just wants a literal-substring "find this" feel.
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));

        sqlx::query_as::<_, HistoryEntry>(
            "SELECT h.id, h.transcript, h.app_name, h.window_title, h.model, \
                    h.duration_ms, h.created_at \
             FROM history h \
             INNER JOIN history_fts fts ON fts.rowid = h.id \
             WHERE history_fts MATCH ? \
             ORDER BY h.id DESC \
             LIMIT ? OFFSET ?",
        )
        .bind(phrase)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.db.pool())
        .await
        .context("search history")
    }

    async fn delete(&self, id: i64) -> Result<()> {
        // The AFTER DELETE trigger from migration 0001 keeps the FTS5
        // index in sync, so we don't need to touch `history_fts` here.
        sqlx::query("DELETE FROM history WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("delete history row")?;
        Ok(())
    }

    async fn count(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM history")
            .fetch_one(self.db.pool())
            .await
            .context("count history")?;
        Ok(row.0)
    }
}

// `FromRow` impl deliberately hand-rolled rather than derived: the
// `serde::Serialize` derive on `HistoryEntry` already pulls `serde` in,
// but `sqlx::FromRow` would need either the derive feature on `sqlx` or
// a manual `impl`. The manual impl is short and keeps the dep surface
// minimal.
impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for HistoryEntry {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(HistoryEntry {
            id: row.try_get("id")?,
            transcript: row.try_get("transcript")?,
            app_name: row.try_get("app_name")?,
            window_title: row.try_get("window_title")?,
            model: row.try_get("model")?,
            duration_ms: row.try_get("duration_ms")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_repo() -> SqliteHistoryRepository {
        let db = SqliteDatabase::open_in_memory()
            .await
            .expect("in-memory db");
        SqliteHistoryRepository::new(Arc::new(db))
    }

    fn sample(transcript: &str, app: Option<&str>) -> NewHistoryEntry {
        NewHistoryEntry {
            transcript: transcript.to_owned(),
            app_name: app.map(str::to_owned),
            window_title: None,
            model: "test-model".to_owned(),
            duration_ms: Some(1234),
        }
    }

    #[tokio::test]
    async fn insert_then_list_returns_the_row() {
        let repo = fresh_repo().await;
        let id = repo
            .insert(sample("hello world", Some("Slack")))
            .await
            .unwrap();
        assert!(id > 0);

        let rows = repo.list(10, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].transcript, "hello world");
        assert_eq!(rows[0].app_name.as_deref(), Some("Slack"));
        assert_eq!(rows[0].model, "test-model");
        assert_eq!(rows[0].duration_ms, Some(1234));
    }

    #[tokio::test]
    async fn list_orders_newest_first() {
        let repo = fresh_repo().await;
        repo.insert(sample("first", None)).await.unwrap();
        repo.insert(sample("second", None)).await.unwrap();
        repo.insert(sample("third", None)).await.unwrap();

        let rows = repo.list(10, 0).await.unwrap();
        let transcripts: Vec<_> = rows.iter().map(|r| r.transcript.as_str()).collect();
        assert_eq!(transcripts, vec!["third", "second", "first"]);
    }

    #[tokio::test]
    async fn list_paginates_with_limit_and_offset() {
        let repo = fresh_repo().await;
        for i in 0..5 {
            repo.insert(sample(&format!("row {i}"), None))
                .await
                .unwrap();
        }
        let page = repo.list(2, 0).await.unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].transcript, "row 4");

        let next = repo.list(2, 2).await.unwrap();
        assert_eq!(next.len(), 2);
        assert_eq!(next[0].transcript, "row 2");
    }

    #[tokio::test]
    async fn list_caps_excessive_limit_at_max() {
        let repo = fresh_repo().await;
        repo.insert(sample("only", None)).await.unwrap();
        // A nonsense huge limit must not blow up; clamp prevents the
        // sqlite query from binding a value outside i64 sense.
        let rows = repo.list(i64::MAX, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn search_matches_fts5_terms() {
        let repo = fresh_repo().await;
        repo.insert(sample("the quick brown fox", None))
            .await
            .unwrap();
        repo.insert(sample("the lazy dog", None)).await.unwrap();
        repo.insert(sample("brown bears eat fish", None))
            .await
            .unwrap();

        let hits = repo.search("brown", 10, 0).await.unwrap();
        assert_eq!(hits.len(), 2);
        // Both rows that contain "brown" come back; the lazy dog row does not.
        for hit in &hits {
            assert!(hit.transcript.contains("brown"));
        }
    }

    #[tokio::test]
    async fn search_with_blank_query_returns_full_list() {
        // Mirrors the UI's "empty search box → show everything" behaviour
        // so the frontend can call `search("")` unconditionally on each
        // keystroke without a guard.
        let repo = fresh_repo().await;
        repo.insert(sample("a", None)).await.unwrap();
        repo.insert(sample("b", None)).await.unwrap();

        let rows = repo.search("   ", 10, 0).await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn search_quotes_are_escaped_to_avoid_fts5_parser_errors() {
        // FTS5 treats unescaped double quotes as phrase delimiters; a
        // user-typed quote in the search box would otherwise produce a
        // confusing "syntax error" from the engine. We wrap and double
        // any embedded quotes so the literal text is the search.
        let repo = fresh_repo().await;
        repo.insert(sample(r#"the cat said "hello""#, None))
            .await
            .unwrap();

        let rows = repo
            .search(r#"said "hello"#, 10, 0)
            .await
            .expect("must not surface a sqlite parser error");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn delete_removes_row_and_updates_count() {
        let repo = fresh_repo().await;
        let id = repo.insert(sample("doomed", None)).await.unwrap();
        assert_eq!(repo.count().await.unwrap(), 1);

        repo.delete(id).await.unwrap();
        assert_eq!(repo.count().await.unwrap(), 0);
        assert!(repo.list(10, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_missing_id_is_a_no_op_not_an_error() {
        // The contract is "the caller's intent (this row should not
        // exist) is satisfied either way" — see the trait doc. Useful
        // for the UI's per-row delete button: a double-click can't
        // surface a confusing error.
        let repo = fresh_repo().await;
        repo.delete(99_999).await.expect("missing id is fine");
    }

    #[tokio::test]
    async fn delete_drops_row_from_fts_index() {
        // The AFTER DELETE trigger in migration 0001 should keep
        // history_fts in sync — without it, search would return the
        // deleted row's transcript indefinitely. This guards the
        // trigger as much as the repository.
        let repo = fresh_repo().await;
        let id = repo
            .insert(sample("haystack needle haystack", None))
            .await
            .unwrap();

        let before = repo.search("needle", 10, 0).await.unwrap();
        assert_eq!(before.len(), 1);

        repo.delete(id).await.unwrap();
        let after = repo.search("needle", 10, 0).await.unwrap();
        assert!(after.is_empty(), "fts index still returned: {after:?}");
    }

    #[tokio::test]
    async fn count_starts_at_zero_and_grows_with_inserts() {
        let repo = fresh_repo().await;
        assert_eq!(repo.count().await.unwrap(), 0);
        repo.insert(sample("one", None)).await.unwrap();
        repo.insert(sample("two", None)).await.unwrap();
        assert_eq!(repo.count().await.unwrap(), 2);
    }
}
