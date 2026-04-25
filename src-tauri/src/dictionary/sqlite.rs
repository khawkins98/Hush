//! SQLite-backed implementation of [`ReplacementRepository`].
//!
//! Mirrors the shape of [`crate::history::SqliteHistoryRepository`]:
//! borrows the pool from a shared [`SqliteDatabase`], every method is a
//! single round-trip query against the `replacements` table from
//! migration 0001.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;

use super::{NewReplacementRule, ReplacementRepository, ReplacementRule};

/// Concrete repository backed by a [`SqliteDatabase`] pool.
pub struct SqliteReplacementRepository {
    db: Arc<SqliteDatabase>,
}

impl SqliteReplacementRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ReplacementRepository for SqliteReplacementRepository {
    async fn list(&self) -> Result<Vec<ReplacementRule>> {
        // Sort `(sort_order, id)` so the IPC layer's
        // `apply_replacements` doesn't need to re-sort on every
        // transcription (it does anyway as a defensive belt-and-braces,
        // but the storage layer's job is to give back the canonical
        // order).
        sqlx::query_as::<_, ReplacementRule>(
            "SELECT id, find_text, replace_text, sort_order \
             FROM replacements \
             ORDER BY sort_order ASC, id ASC",
        )
        .fetch_all(self.db.pool())
        .await
        .context("list replacements")
    }

    async fn create(&self, rule: NewReplacementRule) -> Result<ReplacementRule> {
        let id = sqlx::query(
            "INSERT INTO replacements (find_text, replace_text, sort_order) \
             VALUES (?, ?, ?)",
        )
        .bind(&rule.find_text)
        .bind(&rule.replace_text)
        .bind(rule.sort_order)
        .execute(self.db.pool())
        .await
        .context("insert replacement")?
        .last_insert_rowid();

        // Round-trip the persisted row so the caller gets the
        // database-assigned id without a follow-up SELECT. Avoids a race
        // where another writer could insert between our INSERT and a
        // hypothetical follow-up read; here we know which id we just
        // generated.
        Ok(ReplacementRule {
            id,
            find_text: rule.find_text,
            replace_text: rule.replace_text,
            sort_order: rule.sort_order,
        })
    }

    async fn update(&self, rule: ReplacementRule) -> Result<()> {
        sqlx::query(
            "UPDATE replacements \
             SET find_text = ?, replace_text = ?, sort_order = ? \
             WHERE id = ?",
        )
        .bind(&rule.find_text)
        .bind(&rule.replace_text)
        .bind(rule.sort_order)
        .bind(rule.id)
        .execute(self.db.pool())
        .await
        .context("update replacement")?;
        Ok(())
    }

    async fn delete(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM replacements WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("delete replacement")?;
        Ok(())
    }
}

// Hand-rolled `FromRow` to keep parity with the history module — see
// the comment there for why we don't lean on `sqlx::FromRow` derive.
impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for ReplacementRule {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(ReplacementRule {
            id: row.try_get("id")?,
            find_text: row.try_get("find_text")?,
            replace_text: row.try_get("replace_text")?,
            sort_order: row.try_get("sort_order")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_repo() -> SqliteReplacementRepository {
        let db = SqliteDatabase::open_in_memory()
            .await
            .expect("in-memory db");
        SqliteReplacementRepository::new(Arc::new(db))
    }

    #[tokio::test]
    async fn create_then_list_returns_the_row() {
        let repo = fresh_repo().await;
        let created = repo
            .create(NewReplacementRule {
                find_text: "um ".into(),
                replace_text: "".into(),
                sort_order: 0,
            })
            .await
            .unwrap();
        assert!(created.id > 0);
        assert_eq!(created.find_text, "um ");

        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], created);
    }

    #[tokio::test]
    async fn list_orders_by_sort_then_id() {
        let repo = fresh_repo().await;
        let a = repo
            .create(NewReplacementRule {
                find_text: "a".into(),
                replace_text: "A".into(),
                sort_order: 5,
            })
            .await
            .unwrap();
        let b = repo
            .create(NewReplacementRule {
                find_text: "b".into(),
                replace_text: "B".into(),
                sort_order: 0,
            })
            .await
            .unwrap();
        let c = repo
            .create(NewReplacementRule {
                find_text: "c".into(),
                replace_text: "C".into(),
                sort_order: 0,
            })
            .await
            .unwrap();

        let rows = repo.list().await.unwrap();
        // sort_order = 0 first (ties broken by id), then sort_order = 5
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![b.id, c.id, a.id]
        );
    }

    #[tokio::test]
    async fn update_persists_new_field_values() {
        let repo = fresh_repo().await;
        let mut rule = repo
            .create(NewReplacementRule {
                find_text: "old".into(),
                replace_text: "new".into(),
                sort_order: 0,
            })
            .await
            .unwrap();

        rule.find_text = "older".into();
        rule.replace_text = "newer".into();
        rule.sort_order = 9;
        repo.update(rule.clone()).await.unwrap();

        let after = &repo.list().await.unwrap()[0];
        assert_eq!(after, &rule);
    }

    #[tokio::test]
    async fn update_missing_id_is_a_no_op() {
        let repo = fresh_repo().await;
        repo.update(ReplacementRule {
            id: 99_999,
            find_text: "ghost".into(),
            replace_text: "ghost2".into(),
            sort_order: 0,
        })
        .await
        .expect("missing id is fine");
        assert_eq!(repo.list().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let repo = fresh_repo().await;
        let r = repo
            .create(NewReplacementRule {
                find_text: "x".into(),
                replace_text: "y".into(),
                sort_order: 0,
            })
            .await
            .unwrap();
        repo.delete(r.id).await.unwrap();
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_missing_id_is_a_no_op() {
        let repo = fresh_repo().await;
        repo.delete(404).await.expect("missing id is fine");
    }
}
