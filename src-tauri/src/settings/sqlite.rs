//! SQLite-backed [`SettingsRepository`].
//!
//! Single-row-per-key store backed by the `settings` table from
//! migration 0001. Implementation is intentionally boring — every
//! method is a one-statement `INSERT OR REPLACE` / `SELECT` /
//! `DELETE`.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;

use super::SettingsRepository;

pub struct SqliteSettingsRepository {
    db: Arc<SqliteDatabase>,
}

impl SqliteSettingsRepository {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SettingsRepository for SqliteSettingsRepository {
    async fn get(&self, key: &str) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(self.db.pool())
            .await
            .context("read setting")?;
        Ok(row.map(|(v,)| v))
    }

    async fn set(&self, key: &str, value: &str) -> Result<()> {
        // `INSERT OR REPLACE` keeps the call site simple at the cost of
        // re-allocating the row id on every write. Since rows here are
        // referenced only by `key` (the PRIMARY KEY), nothing depends on
        // the `rowid` staying stable across writes.
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(self.db.pool())
            .await
            .context("write setting")?;
        Ok(())
    }

    async fn remove(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(self.db.pool())
            .await
            .context("delete setting")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_repo() -> SqliteSettingsRepository {
        let db = SqliteDatabase::open_in_memory()
            .await
            .expect("in-memory db");
        SqliteSettingsRepository::new(Arc::new(db))
    }

    #[tokio::test]
    async fn get_missing_key_returns_none() {
        let repo = fresh_repo().await;
        assert_eq!(repo.get("never-written").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips() {
        let repo = fresh_repo().await;
        repo.set("model", "whisper-base").await.unwrap();
        assert_eq!(
            repo.get("model").await.unwrap().as_deref(),
            Some("whisper-base")
        );
    }

    #[tokio::test]
    async fn set_overwrites_existing_value() {
        let repo = fresh_repo().await;
        repo.set("model", "whisper-tiny").await.unwrap();
        repo.set("model", "whisper-base").await.unwrap();
        assert_eq!(
            repo.get("model").await.unwrap().as_deref(),
            Some("whisper-base")
        );
    }

    #[tokio::test]
    async fn empty_string_value_is_distinct_from_missing() {
        // Confirm the `Option<String>` contract: a deliberate empty
        // string is "the user set this to nothing" and survives a
        // round-trip; an unset key is `None`.
        let repo = fresh_repo().await;
        repo.set("explicit-empty", "").await.unwrap();
        assert_eq!(
            repo.get("explicit-empty").await.unwrap().as_deref(),
            Some("")
        );
        assert_eq!(repo.get("never-set").await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_deletes_the_value() {
        let repo = fresh_repo().await;
        repo.set("temp", "value").await.unwrap();
        repo.remove("temp").await.unwrap();
        assert_eq!(repo.get("temp").await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_missing_key_is_a_no_op() {
        // Same contract as the other repo deletes — the caller's intent
        // is "this key should not exist", which is true either way.
        let repo = fresh_repo().await;
        repo.remove("ghost").await.expect("missing key is fine");
    }
}
