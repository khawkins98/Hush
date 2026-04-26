//! SQLite-backed implementation of [`VocabularyRepository`].
//!
//! Schema column is `term TEXT NOT NULL UNIQUE`, so create/update can
//! fail with a UNIQUE constraint error that we surface unmodified — the
//! caller (the IPC layer) can map it to user-facing copy.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;
use crate::repository::Repository;

use super::{NewVocabularyTerm, VocabularyTerm};

/// SQLite-backed [`VocabularyRepository`]. Mirrors the shape of
/// [`crate::dictionary::SqliteReplacementRepository`].
pub struct SqliteVocabularyRepository {
    db: Arc<SqliteDatabase>,
}

impl SqliteVocabularyRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Repository<VocabularyTerm, NewVocabularyTerm, i64> for SqliteVocabularyRepository {
    async fn list(&self) -> Result<Vec<VocabularyTerm>> {
        sqlx::query_as::<_, VocabularyTerm>("SELECT id, term FROM dictionary_terms ORDER BY id ASC")
            .fetch_all(self.db.pool())
            .await
            .context("list vocabulary")
    }

    async fn create(&self, new_term: NewVocabularyTerm) -> Result<VocabularyTerm> {
        let id = sqlx::query("INSERT INTO dictionary_terms (term) VALUES (?)")
            .bind(&new_term.term)
            .execute(self.db.pool())
            .await
            .context("insert vocabulary term")?
            .last_insert_rowid();

        Ok(VocabularyTerm {
            id,
            term: new_term.term,
        })
    }

    async fn update(&self, term: VocabularyTerm) -> Result<()> {
        sqlx::query("UPDATE dictionary_terms SET term = ? WHERE id = ?")
            .bind(&term.term)
            .bind(term.id)
            .execute(self.db.pool())
            .await
            .context("update vocabulary term")?;
        Ok(())
    }

    async fn delete(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM dictionary_terms WHERE id = ?")
            .bind(id)
            .execute(self.db.pool())
            .await
            .context("delete vocabulary term")?;
        Ok(())
    }
}

impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for VocabularyTerm {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(VocabularyTerm {
            id: row.try_get("id")?,
            term: row.try_get("term")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_vocab_repo() -> SqliteVocabularyRepository {
        let db = SqliteDatabase::open_in_memory()
            .await
            .expect("in-memory db");
        SqliteVocabularyRepository::new(Arc::new(db))
    }

    #[tokio::test]
    async fn vocab_create_then_list_returns_the_term() {
        let repo = fresh_vocab_repo().await;
        let created = repo
            .create(NewVocabularyTerm {
                term: "Hush".into(),
            })
            .await
            .unwrap();
        assert!(created.id > 0);
        assert_eq!(created.term, "Hush");

        let all = repo.list().await.unwrap();
        assert_eq!(all, vec![created]);
    }

    #[tokio::test]
    async fn vocab_create_rejects_duplicate_term() {
        // The schema's UNIQUE constraint is what enforces this; the
        // assertion below confirms the constraint actually fires (and
        // that we surface it as an error rather than swallowing).
        let repo = fresh_vocab_repo().await;
        repo.create(NewVocabularyTerm {
            term: "Hush".into(),
        })
        .await
        .unwrap();

        let err = repo
            .create(NewVocabularyTerm {
                term: "Hush".into(),
            })
            .await
            .expect_err("duplicate must error");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("unique"),
            "expected UNIQUE-constraint message, got {msg}"
        );
    }

    #[tokio::test]
    async fn vocab_list_orders_by_insertion_id() {
        let repo = fresh_vocab_repo().await;
        let a = repo
            .create(NewVocabularyTerm {
                term: "Tauri".into(),
            })
            .await
            .unwrap();
        let b = repo
            .create(NewVocabularyTerm {
                term: "Whisper".into(),
            })
            .await
            .unwrap();

        assert_eq!(
            repo.list()
                .await
                .unwrap()
                .iter()
                .map(|t| t.id)
                .collect::<Vec<_>>(),
            vec![a.id, b.id]
        );
    }

    #[tokio::test]
    async fn vocab_update_persists_new_text() {
        let repo = fresh_vocab_repo().await;
        let mut term = repo
            .create(NewVocabularyTerm {
                term: "hush".into(),
            })
            .await
            .unwrap();
        term.term = "Hush".into();
        repo.update(term.clone()).await.unwrap();
        assert_eq!(repo.list().await.unwrap()[0], term);
    }

    #[tokio::test]
    async fn vocab_update_missing_id_is_a_no_op() {
        let repo = fresh_vocab_repo().await;
        repo.update(VocabularyTerm {
            id: 9999,
            term: "ghost".into(),
        })
        .await
        .expect("missing id is fine");
        assert_eq!(repo.list().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn vocab_update_rejects_unique_collision() {
        // Trait doc claims the update path errors on UNIQUE collision —
        // the round-2 Rust review flagged that no test exercised this.
        // Mirrors the pattern in `vocab_create_rejects_duplicate_term`.
        let repo = fresh_vocab_repo().await;
        let a = repo
            .create(NewVocabularyTerm {
                term: "Tauri".into(),
            })
            .await
            .unwrap();
        let b = repo
            .create(NewVocabularyTerm {
                term: "whisper".into(),
            })
            .await
            .unwrap();

        // Try to rename `b` to the value `a` already holds.
        let err = repo
            .update(VocabularyTerm {
                id: b.id,
                term: a.term.clone(),
            })
            .await
            .expect_err("UNIQUE collision must error");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("unique"),
            "expected UNIQUE-constraint message, got {msg}"
        );
    }

    #[tokio::test]
    async fn vocab_delete_removes_row() {
        let repo = fresh_vocab_repo().await;
        let t = repo
            .create(NewVocabularyTerm {
                term: "drop".into(),
            })
            .await
            .unwrap();
        repo.delete(t.id).await.unwrap();
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn vocab_delete_missing_id_is_a_no_op() {
        let repo = fresh_vocab_repo().await;
        repo.delete(404).await.expect("missing id is fine");
    }
}
