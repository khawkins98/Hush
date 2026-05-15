//! SQLite-backed [`SpeakerStore`] (#667).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;
use crate::diarization::cluster::cosine_distance;

use super::{SpeakerIdentity, SpeakerStore};

pub struct SqliteSpeakerStore {
    db: Arc<SqliteDatabase>,
}

impl SqliteSpeakerStore {
    pub fn new(db: Arc<SqliteDatabase>) -> Self {
        Self { db }
    }
}

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn blob_to_embedding(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[async_trait]
impl SpeakerStore for SqliteSpeakerStore {
    async fn list_with_embeddings(&self) -> Result<Vec<(i64, Vec<f32>, i64)>> {
        let rows: Vec<(i64, Vec<u8>, i64)> = sqlx::query_as(
            "SELECT id, embedding, utterance_count FROM speaker_identities ORDER BY id",
        )
        .fetch_all(self.db.pool())
        .await
        .context("list speaker identities with embeddings")?;

        Ok(rows
            .into_iter()
            .map(|(id, blob, count)| (id, blob_to_embedding(&blob), count))
            .collect())
    }

    async fn create(&self, centroid: &[f32], utterance_count: i64) -> Result<i64> {
        let blob = embedding_to_blob(centroid);
        let row = sqlx::query(
            "INSERT INTO speaker_identities (embedding, utterance_count) VALUES (?, ?) RETURNING id",
        )
        .bind(blob)
        .bind(utterance_count)
        .fetch_one(self.db.pool())
        .await
        .context("create speaker identity")?;

        use sqlx::Row;
        Ok(row.try_get("id").context("read new identity id")?)
    }

    async fn update_centroid(
        &self,
        identity_id: i64,
        new_centroid: &[f32],
        new_utterance_count: i64,
    ) -> Result<()> {
        let blob = embedding_to_blob(new_centroid);
        sqlx::query(
            "UPDATE speaker_identities \
             SET embedding = ?, utterance_count = ?, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
             WHERE id = ?",
        )
        .bind(blob)
        .bind(new_utterance_count)
        .bind(identity_id)
        .execute(self.db.pool())
        .await
        .context("update speaker identity centroid")?;
        Ok(())
    }

    async fn link_utterances(
        &self,
        session_id: i64,
        speaker_label: &str,
        identity_id: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE utterances SET speaker_identity_id = ? \
             WHERE session_id = ? AND speaker_label = ?",
        )
        .bind(identity_id)
        .bind(session_id)
        .bind(speaker_label)
        .execute(self.db.pool())
        .await
        .context("link utterances to speaker identity")?;
        Ok(())
    }

    async fn rename(&self, identity_id: i64, display_name: Option<String>) -> Result<()> {
        sqlx::query(
            "UPDATE speaker_identities \
             SET display_name = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
             WHERE id = ?",
        )
        .bind(display_name)
        .bind(identity_id)
        .execute(self.db.pool())
        .await
        .context("rename speaker identity")?;
        Ok(())
    }

    async fn delete(&self, identity_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM speaker_identities WHERE id = ?")
            .bind(identity_id)
            .execute(self.db.pool())
            .await
            .context("delete speaker identity")?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SpeakerIdentity>> {
        sqlx::query_as::<_, SpeakerIdentityRow>(
            "SELECT id, display_name, utterance_count, confidence_state, \
                    created_at, updated_at \
             FROM speaker_identities \
             ORDER BY id",
        )
        .fetch_all(self.db.pool())
        .await
        .context("list speaker identities")
        .map(|rows| rows.into_iter().map(SpeakerIdentity::from).collect())
    }

    async fn merge(&self, keep_id: i64, absorb_id: i64) -> Result<()> {
        // Fetch both identities' embeddings and counts for centroid merge.
        let keep: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT embedding, utterance_count FROM speaker_identities WHERE id = ?",
        )
        .bind(keep_id)
        .fetch_optional(self.db.pool())
        .await
        .context("fetch keep identity for merge")?;

        let absorb: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT embedding, utterance_count FROM speaker_identities WHERE id = ?",
        )
        .bind(absorb_id)
        .fetch_optional(self.db.pool())
        .await
        .context("fetch absorb identity for merge")?;

        // Re-link utterances before deleting absorb_id.
        sqlx::query(
            "UPDATE utterances SET speaker_identity_id = ? WHERE speaker_identity_id = ?",
        )
        .bind(keep_id)
        .bind(absorb_id)
        .execute(self.db.pool())
        .await
        .context("re-link utterances for merge")?;

        // Update keep_id's centroid as weighted mean and delete absorb_id.
        if let (Some((keep_blob, keep_count)), Some((absorb_blob, absorb_count))) = (keep, absorb) {
            let keep_emb = blob_to_embedding(&keep_blob);
            let absorb_emb = blob_to_embedding(&absorb_blob);
            let total = keep_count + absorb_count;
            if total > 0 {
                let new_centroid: Vec<f32> = keep_emb
                    .iter()
                    .zip(absorb_emb.iter())
                    .map(|(k, a)| {
                        (k * keep_count as f32 + a * absorb_count as f32) / total as f32
                    })
                    .collect();
                let blob = embedding_to_blob(&new_centroid);
                sqlx::query(
                    "UPDATE speaker_identities \
                     SET embedding = ?, utterance_count = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
                     WHERE id = ?",
                )
                .bind(blob)
                .bind(total)
                .bind(keep_id)
                .execute(self.db.pool())
                .await
                .context("update centroid after merge")?;
            }
        }

        sqlx::query("DELETE FROM speaker_identities WHERE id = ?")
            .bind(absorb_id)
            .execute(self.db.pool())
            .await
            .context("delete absorbed identity")?;

        Ok(())
    }
}

// Internal row type for sqlx::query_as mapping.
#[derive(sqlx::FromRow)]
struct SpeakerIdentityRow {
    id: i64,
    display_name: Option<String>,
    utterance_count: i64,
    confidence_state: String,
    created_at: String,
    updated_at: String,
}

impl From<SpeakerIdentityRow> for SpeakerIdentity {
    fn from(r: SpeakerIdentityRow) -> Self {
        SpeakerIdentity {
            id: r.id,
            display_name: r.display_name,
            utterance_count: r.utterance_count,
            confidence_state: r.confidence_state,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Find the closest known identity to `query_embedding`.
/// Returns `(identity_id, distance)` if any exist, or `None`.
pub fn find_best_match(
    known: &[(i64, Vec<f32>, i64)],
    query_embedding: &[f32],
) -> Option<(i64, f32)> {
    known
        .iter()
        .map(|(id, emb, _count)| (*id, cosine_distance(query_embedding, emb)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
}
