//! SQLite-backed [`SpeakerStore`] (#667).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::db::SqliteDatabase;
use crate::diarization::cluster::cosine_distance;

use super::{SpeakerIdentity, SpeakerStore, CURRENT_EMBEDDING_VERSION};

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
            "SELECT id, embedding, utterance_count FROM speaker_identities \
             WHERE embedding_version = ? ORDER BY id",
        )
        .bind(CURRENT_EMBEDDING_VERSION)
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
            "INSERT INTO speaker_identities (embedding, utterance_count, embedding_version) \
             VALUES (?, ?, ?) RETURNING id",
        )
        .bind(blob)
        .bind(utterance_count)
        .bind(CURRENT_EMBEDDING_VERSION)
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
             SET embedding = ?, utterance_count = ?, embedding_version = ?, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
             WHERE id = ?",
        )
        .bind(blob)
        .bind(new_utterance_count)
        .bind(CURRENT_EMBEDDING_VERSION)
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
        let keep: Option<(Vec<u8>, i64, i64)> = sqlx::query_as(
            "SELECT embedding, utterance_count, embedding_version FROM speaker_identities WHERE id = ?",
        )
        .bind(keep_id)
        .fetch_optional(self.db.pool())
        .await
        .context("fetch keep identity for merge")?;

        let absorb: Option<(Vec<u8>, i64, i64)> = sqlx::query_as(
            "SELECT embedding, utterance_count, embedding_version FROM speaker_identities WHERE id = ?",
        )
        .bind(absorb_id)
        .fetch_optional(self.db.pool())
        .await
        .context("fetch absorb identity for merge")?;

        // Re-link utterances before deleting absorb_id.
        sqlx::query("UPDATE utterances SET speaker_identity_id = ? WHERE speaker_identity_id = ?")
            .bind(keep_id)
            .bind(absorb_id)
            .execute(self.db.pool())
            .await
            .context("re-link utterances for merge")?;

        // Update keep_id's centroid as weighted mean and delete absorb_id.
        if let (
            Some((keep_blob, keep_count, keep_ver)),
            Some((absorb_blob, absorb_count, absorb_ver)),
        ) = (keep, absorb)
        {
            let keep_emb = blob_to_embedding(&keep_blob);
            let absorb_emb = blob_to_embedding(&absorb_blob);
            let total = keep_count + absorb_count;
            if total > 0 {
                let (new_centroid, new_ver) = merged_centroid(
                    (&keep_emb, keep_count, keep_ver),
                    (&absorb_emb, absorb_count, absorb_ver),
                );
                let blob = embedding_to_blob(&new_centroid);
                sqlx::query(
                    "UPDATE speaker_identities \
                     SET embedding = ?, utterance_count = ?, embedding_version = ?, \
                         updated_at = strftime('%Y-%m-%dT%H:%M:%SZ','now') \
                     WHERE id = ?",
                )
                .bind(blob)
                .bind(total)
                .bind(new_ver)
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

/// Centroid + version for a merge of two stored voiceprints. Same
/// version: count-weighted mean. Different versions: the newer
/// voiceprint wins outright, because averaging vectors from two
/// embedding spaces is meaningless — and this is exactly the "merge the
/// new provisional identity into my old named one" re-enrolment path.
fn merged_centroid(keep: (&[f32], i64, i64), absorb: (&[f32], i64, i64)) -> (Vec<f32>, i64) {
    let (keep_emb, keep_count, keep_ver) = keep;
    let (absorb_emb, absorb_count, absorb_ver) = absorb;
    if keep_ver != absorb_ver {
        return if absorb_ver > keep_ver {
            (absorb_emb.to_vec(), absorb_ver)
        } else {
            (keep_emb.to_vec(), keep_ver)
        };
    }
    let total = (keep_count + absorb_count).max(1) as f32;
    let mean: Vec<f32> = keep_emb
        .iter()
        .zip(absorb_emb)
        .map(|(k, a)| (k * keep_count as f32 + a * absorb_count as f32) / total)
        .collect();
    (mean, keep_ver)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> SqliteSpeakerStore {
        let db = SqliteDatabase::open_in_memory().await.unwrap();
        SqliteSpeakerStore::new(Arc::new(db))
    }

    fn unit(axis: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; 256];
        v[axis] = 1.0;
        v
    }

    /// Insert a pre-#1013 (version 1) voiceprint the way migration 0010
    /// leaves existing rows.
    async fn insert_legacy(s: &SqliteSpeakerStore, emb: &[f32], count: i64) -> i64 {
        let row = sqlx::query(
            "INSERT INTO speaker_identities (embedding, utterance_count, embedding_version) \
             VALUES (?, ?, 1) RETURNING id",
        )
        .bind(embedding_to_blob(emb))
        .bind(count)
        .fetch_one(s.db.pool())
        .await
        .unwrap();
        use sqlx::Row;
        row.try_get("id").unwrap()
    }

    #[tokio::test]
    async fn legacy_voiceprints_are_listed_but_never_matched() {
        let s = store().await;
        let legacy = insert_legacy(&s, &unit(0), 10).await;
        let current = s.create(&unit(1), 5).await.unwrap();
        let matchable: Vec<i64> = s
            .list_with_embeddings()
            .await
            .unwrap()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(matchable, vec![current]);
        let listed: Vec<i64> = s.list().await.unwrap().into_iter().map(|i| i.id).collect();
        assert_eq!(
            listed,
            vec![legacy, current],
            "names stay visible in the UI"
        );
    }

    #[tokio::test]
    async fn merging_new_into_legacy_reenrols_the_named_identity() {
        let s = store().await;
        let legacy = insert_legacy(&s, &unit(0), 10).await;
        s.rename(legacy, Some("Ken".to_owned())).await.unwrap();
        let fresh = s.create(&unit(1), 6).await.unwrap();
        s.merge(legacy, fresh).await.unwrap();
        let matchable = s.list_with_embeddings().await.unwrap();
        assert_eq!(matchable.len(), 1);
        let (id, emb, count) = &matchable[0];
        assert_eq!(*id, legacy, "the named identity survives");
        assert_eq!(*count, 16);
        assert_eq!(
            emb,
            &unit(1),
            "the current-version voiceprint wins, no averaging"
        );
    }

    #[test]
    fn merged_centroid_same_version_is_weighted_mean() {
        let (c, v) = merged_centroid((&[1.0, 0.0], 3, 2), (&[0.0, 1.0], 1, 2));
        assert_eq!(v, 2);
        assert!((c[0] - 0.75).abs() < 1e-6 && (c[1] - 0.25).abs() < 1e-6);
        let (c, v) = merged_centroid((&[0.0, 1.0], 9, 2), (&[1.0, 0.0], 1, 1));
        assert_eq!((c, v), (vec![0.0, 1.0], 2), "stale absorb is ignored");
    }
}
