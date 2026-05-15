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

use super::{DictationStats, HistoryEntry, HistoryRepository, NewHistoryEntry};

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
    async fn create(&self, entry: NewHistoryEntry) -> Result<i64> {
        let result = sqlx::query(
            "INSERT INTO history (transcript, app_name, window_title, model, duration_ms, ignored) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(entry.transcript)
        .bind(entry.app_name)
        .bind(entry.window_title)
        .bind(entry.model)
        .bind(entry.duration_ms)
        .bind(entry.ignored as i64)
        .execute(self.db.pool())
        .await
        .context("insert history row")?;

        Ok(result.last_insert_rowid())
    }

    async fn list(&self, limit: i64, offset: i64) -> Result<Vec<HistoryEntry>> {
        let limit = Self::cap(limit);
        let offset = offset.max(0);

        sqlx::query_as::<_, HistoryEntry>(
            "SELECT id, transcript, app_name, window_title, model, duration_ms, created_at, \
                    ignored, name \
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
                    h.duration_ms, h.created_at, h.ignored, h.name \
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

    async fn get_by_id(&self, id: i64) -> Result<Option<HistoryEntry>> {
        // Single B-tree probe via the PK index — mirrors the column list in
        // `list()` so a future column add only needs to touch one place.
        sqlx::query_as::<_, HistoryEntry>(
            "SELECT id, transcript, app_name, window_title, model, duration_ms, created_at, \
                    ignored, name \
             FROM history \
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
        .context("get history row by id")
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

    async fn clear(&self) -> Result<i64> {
        // The AFTER DELETE trigger from migration 0001 fires per row
        // and keeps `history_fts` in sync. For a several-thousand-row
        // history this still runs in tens of milliseconds — the FTS5
        // delta operations are cheap. If profiling later shows a
        // smarter "rebuild fts from scratch" path is needed, that's
        // a follow-up.
        let result = sqlx::query("DELETE FROM history")
            .execute(self.db.pool())
            .await
            .context("clear history")?;
        Ok(result.rows_affected() as i64)
    }

    async fn count(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as("SELECT count(*) FROM history")
            .fetch_one(self.db.pool())
            .await
            .context("count history")?;
        Ok(row.0)
    }

    async fn get_stats(&self) -> Result<DictationStats> {
        // Single-pass aggregate over the history table (#293).
        // Word count is "whitespace tokens" — for non-empty
        // transcripts: (length-of-trimmed-text minus
        // length-after-removing-spaces) plus one. This matches a
        // simple split-on-whitespace; tabs/newlines aren't
        // counted as separators but transcripts are space-
        // delimited in practice. `total_chars` is the keystroke-
        // saved approximation; the UI labels it "~N keystrokes"
        // so the imprecision is honest.
        let row: (i64, i64, i64, i64) = sqlx::query_as(
            r#"
            SELECT
              COUNT(*),
              COALESCE(SUM(
                CASE
                  WHEN TRIM(transcript) = '' OR transcript IS NULL THEN 0
                  ELSE LENGTH(TRIM(transcript)) - LENGTH(REPLACE(TRIM(transcript), ' ', '')) + 1
                END
              ), 0),
              COALESCE(SUM(duration_ms), 0),
              COALESCE(SUM(LENGTH(COALESCE(transcript, ''))), 0)
            FROM history
            WHERE ignored = 0
            "#,
        )
        .fetch_one(self.db.pool())
        .await
        .context("get history stats")?;
        Ok(DictationStats {
            session_count: row.0,
            word_count: row.1,
            total_recording_ms: row.2,
            total_chars: row.3,
        })
    }

    async fn list_all_for_export(&self, query: Option<&str>) -> Result<Vec<HistoryEntry>> {
        match query {
            None | Some("") => sqlx::query_as::<_, HistoryEntry>(
                "SELECT id, transcript, app_name, window_title, model, duration_ms, created_at, \
                        ignored, name \
                 FROM history \
                 WHERE ignored = 0 \
                 ORDER BY id DESC",
            )
            .fetch_all(self.db.pool())
            .await
            .context("list_all_for_export"),
            Some(q) => {
                let phrase = format!("\"{}\"", q.replace('"', "\"\""));
                sqlx::query_as::<_, HistoryEntry>(
                    "SELECT h.id, h.transcript, h.app_name, h.window_title, h.model, \
                            h.duration_ms, h.created_at, h.ignored, h.name \
                     FROM history h \
                     INNER JOIN history_fts fts ON fts.rowid = h.id \
                     WHERE history_fts MATCH ? \
                       AND h.ignored = 0 \
                     ORDER BY h.id DESC",
                )
                .bind(phrase)
                .fetch_all(self.db.pool())
                .await
                .context("list_all_for_export (search)")
            }
        }
    }

    async fn set_name(&self, id: i64, name: Option<String>) -> Result<()> {
        let name = name.and_then(|n| {
            let t = n.trim().to_owned();
            if t.is_empty() { None } else { Some(t) }
        });
        sqlx::query("UPDATE history SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("set history entry name")?;
        Ok(())
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
            ignored: row.try_get::<i64, _>("ignored").unwrap_or(0) != 0,
            name: row.try_get("name")?,
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
            ignored: false,
        }
    }

    #[tokio::test]
    async fn insert_then_list_returns_the_row() {
        let repo = fresh_repo().await;
        let id = repo
            .create(sample("hello world", Some("Slack")))
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
        repo.create(sample("first", None)).await.unwrap();
        repo.create(sample("second", None)).await.unwrap();
        repo.create(sample("third", None)).await.unwrap();

        let rows = repo.list(10, 0).await.unwrap();
        let transcripts: Vec<_> = rows.iter().map(|r| r.transcript.as_str()).collect();
        assert_eq!(transcripts, vec!["third", "second", "first"]);
    }

    #[tokio::test]
    async fn list_paginates_with_limit_and_offset() {
        let repo = fresh_repo().await;
        for i in 0..5 {
            repo.create(sample(&format!("row {i}"), None))
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
        repo.create(sample("only", None)).await.unwrap();
        // A nonsense huge limit must not blow up; clamp prevents the
        // sqlite query from binding a value outside i64 sense.
        let rows = repo.list(i64::MAX, 0).await.unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn search_matches_fts5_terms() {
        let repo = fresh_repo().await;
        repo.create(sample("the quick brown fox", None))
            .await
            .unwrap();
        repo.create(sample("the lazy dog", None)).await.unwrap();
        repo.create(sample("brown bears eat fish", None))
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
        repo.create(sample("a", None)).await.unwrap();
        repo.create(sample("b", None)).await.unwrap();

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
        repo.create(sample(r#"the cat said "hello""#, None))
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
        let id = repo.create(sample("doomed", None)).await.unwrap();
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
            .create(sample("haystack needle haystack", None))
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
        repo.create(sample("one", None)).await.unwrap();
        repo.create(sample("two", None)).await.unwrap();
        assert_eq!(repo.count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn clear_removes_every_row_and_returns_the_count() {
        let repo = fresh_repo().await;
        for txt in ["one", "two", "three"] {
            repo.create(sample(txt, None)).await.unwrap();
        }
        let removed = repo.clear().await.unwrap();
        assert_eq!(removed, 3);
        assert_eq!(repo.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn get_stats_returns_zeros_on_empty_table() {
        let repo = fresh_repo().await;
        let stats = repo.get_stats().await.unwrap();
        assert_eq!(stats, DictationStats::default());
    }

    #[tokio::test]
    async fn get_stats_aggregates_words_and_chars_and_duration() {
        // Three rows with distinct shapes:
        //   - "hello world" — 11 chars, 2 words, 1234 ms
        //   - "one two three four" — 18 chars, 4 words, 1234 ms
        //   - "" — 0 chars, 0 words, 1234 ms (still counts toward
        //     session_count)
        // Totals: 3 sessions, 6 words, 3702 ms, 29 chars.
        let repo = fresh_repo().await;
        repo.create(sample("hello world", None)).await.unwrap();
        repo.create(sample("one two three four", None))
            .await
            .unwrap();
        repo.create(sample("", None)).await.unwrap();
        let stats = repo.get_stats().await.unwrap();
        assert_eq!(stats.session_count, 3);
        assert_eq!(stats.word_count, 6, "two + four = 6 words");
        assert_eq!(stats.total_recording_ms, 3702);
        assert_eq!(stats.total_chars, 29);
    }

    #[tokio::test]
    async fn get_stats_word_count_handles_leading_trailing_whitespace() {
        // The TRIM-then-count-spaces formula must not over-count
        // when the user's transcript has padding whitespace (the
        // whisper pipeline can produce these in practice).
        let repo = fresh_repo().await;
        repo.create(sample("  hello world  ", None)).await.unwrap();
        let stats = repo.get_stats().await.unwrap();
        assert_eq!(stats.word_count, 2);
    }

    #[tokio::test]
    async fn get_stats_word_count_overcounts_multi_space_runs() {
        // Documented imprecision (#406): the TRIM-then-count-spaces
        // formula counts every space as a separator, so a double-
        // space run reads as N+1 words. We pin this so a future
        // "fix" that collapses runs must update the test deliberately
        // — and so a contributor reading `get_stats` knows the SQL
        // has known edge-case behaviour rather than discovering it
        // when their off-by-one count surprises them.
        //
        // Whisper's tokeniser doesn't typically emit double-spaced
        // output, but pasted-in transcripts (manual edits, future
        // import paths) might. If this becomes load-bearing the
        // right move is a recursive CTE collapsing whitespace runs;
        // the ~2× SQL cost is fine on a few-thousand-row history.
        let repo = fresh_repo().await;
        repo.create(sample("hello  world", None)).await.unwrap();
        let stats = repo.get_stats().await.unwrap();
        assert_eq!(stats.word_count, 3);
    }

    #[tokio::test]
    async fn get_stats_excludes_ignored_rows() {
        // Ignored rows (too-short recordings) must not inflate any of the
        // four aggregate values: session_count, word_count,
        // total_recording_ms, or total_chars.
        let repo = fresh_repo().await;
        repo.create(sample("hello world", None)).await.unwrap();
        // This entry is ignored — it must not appear in any stat.
        repo.create(NewHistoryEntry {
            transcript: String::new(),
            app_name: None,
            window_title: None,
            model: String::new(),
            duration_ms: Some(400),
            ignored: true,
        })
        .await
        .unwrap();
        let stats = repo.get_stats().await.unwrap();
        assert_eq!(
            stats.session_count, 1,
            "ignored row must not count as a session"
        );
        assert_eq!(stats.word_count, 2);
        assert_eq!(
            stats.total_recording_ms, 1234,
            "ignored row duration excluded"
        );
        assert_eq!(stats.total_chars, 11);
    }

    #[tokio::test]
    async fn clear_drops_rows_from_fts_index() {
        // Same trigger discipline as `delete`: the AFTER DELETE
        // trigger from migration 0001 fires once per cleared row,
        // so search should return empty immediately afterwards.
        let repo = fresh_repo().await;
        repo.create(sample("haystack needle haystack", None))
            .await
            .unwrap();
        repo.clear().await.unwrap();
        let after = repo.search("needle", 10, 0).await.unwrap();
        assert!(after.is_empty(), "fts index still returned: {after:?}");
    }

    #[tokio::test]
    async fn clear_on_empty_table_returns_zero() {
        // No rows means nothing to delete — `clear` succeeds and
        // reports `0` rather than erroring. The frontend's
        // confirmation flow can still be exercised on an empty
        // history (the user sees "Cleared 0 transcripts") which
        // is preferable to a hidden no-op.
        let repo = fresh_repo().await;
        assert_eq!(repo.clear().await.unwrap(), 0);
    }
}
