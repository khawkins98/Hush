//! Cross-session speaker identity store (#667).
//!
//! Maintains a `speaker_identities` SQLite table whose rows represent
//! durable speaker profiles built from the 256-d wespeaker embeddings
//! that the diarizer produces per-session. At session close the
//! `SessionManager` snapshots the in-session centroids (via
//! `Diarize::session_centroids`) and calls
//! `SpeakerStore::resolve_session_speakers` to auto-link them to known
//! identities or create provisional new ones.
//!
//! Privacy: embeddings are voice biometrics. The feature is opt-in
//! (`speaker_identity_enabled` settings key, default false). The
//! `SpeakerStore` is only wired into `DataServices` when running;
//! in tests it defaults to a `MemSpeakerStore` (empty, no-op).
//!
//! ## Trait seam
//!
//! `SpeakerStore` is the trait; `SqliteSpeakerStore` is the production
//! impl; `MemSpeakerStore` is the hand-rolled in-memory mock for tests.

pub mod sqlite;

pub use sqlite::SqliteSpeakerStore;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Auto-accept threshold for cross-session speaker matching.
/// Must be tighter than the in-session threshold (0.6) because a
/// false cross-session merge is permanent — we'd link two different
/// people's entire meeting histories.
///
/// Re-checked for #1013 on 5-utterance centroids from different
/// LibriSpeech chapters (separate recording sessions), clean and through
/// Opus 16 kb/s: with CMN, 0.25 accepts 91–93 % of same-speaker pairs
/// (including the clean↔Opus channel shift) with **0** false merges in
/// 2 756–5 512 cross-speaker pairs; the closest cross-speaker pair sat at
/// 0.335. Before CMN the same 0.25 would have falsely merged 11–16 % of
/// cross-speaker pairs. 0.30 would also have been clean on this set but
/// leaves under 0.04 of margin, so the value stays at 0.25.
pub const AUTO_ACCEPT_THRESHOLD: f32 = 0.25;

/// Version of the embedding pipeline that produced a stored voiceprint
/// (`speaker_identities.embedding_version`, migration 0010). Bump it
/// whenever a change moves embeddings to a different space — a new
/// front end, normalisation, or model — so stale voiceprints stop being
/// auto-matched instead of producing meaningless distances.
///
/// - 1: pre-#1013 (no CMN, Povey window, raw running-mean centroids).
/// - 2: #1013 kaldi-compatible fbank + CMN, unit-vector centroids.
pub const CURRENT_EMBEDDING_VERSION: i64 = 2;

/// Minimum utterance count in a session cluster before we attempt
/// cross-session matching. Below this the centroid is too noisy.
pub const MIN_UTTERANCE_COUNT_FOR_MATCH: usize = 5;

/// One row from the `speaker_identities` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerIdentity {
    pub id: i64,
    pub display_name: Option<String>,
    pub utterance_count: i64,
    pub confidence_state: String,
    pub created_at: String,
    pub updated_at: String,
    // Embedding NOT serialised to frontend — biometric data stays on backend.
}

/// One session-cluster to resolve at session-close time.
/// Produced by `Diarize::session_centroids()`.
pub struct SessionCluster {
    /// In-session cluster index (0-based); corresponds to "Speaker N+1"
    /// in the utterance labels.
    pub cluster_id: usize,
    /// Running-mean centroid embedding (256 f32).
    pub centroid: Vec<f32>,
    /// Number of utterances in this cluster for the just-ended session.
    pub utterance_count: usize,
}

/// `v / |v|` (zero vector stays zero). Lives here rather than in
/// `diarization::session` because the speaker store must build without
/// the `diarization-onnx` feature.
pub fn unit_vector(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return vec![0.0; v.len()];
    }
    v.iter().map(|x| x / norm).collect()
}

/// SpeakerStore trait — the mockable seam for cross-session identity.
#[async_trait]
pub trait SpeakerStore: Send + Sync {
    /// Load all known identities with their stored embeddings for
    /// matching. Returns `(id, embedding, utterance_count)` triples.
    /// Only identities whose embedding is
    /// [`CURRENT_EMBEDDING_VERSION`] are returned: older voiceprints live
    /// in a different embedding space and must not be matched.
    async fn list_with_embeddings(&self) -> Result<Vec<(i64, Vec<f32>, i64)>>;

    /// Create a new provisional identity with the given centroid.
    /// Returns the new row's id.
    async fn create(&self, centroid: &[f32], utterance_count: i64) -> Result<i64>;

    /// Update a known identity's centroid (weighted running mean) and
    /// utterance count. `new_utterance_count` is the TOTAL new count
    /// (old + session).
    async fn update_centroid(
        &self,
        identity_id: i64,
        new_centroid: &[f32],
        new_utterance_count: i64,
    ) -> Result<()>;

    /// Set `speaker_identity_id` on all utterances in `session_id`
    /// whose `speaker_label` matches `speaker_label` (e.g. "Speaker 1").
    async fn link_utterances(
        &self,
        session_id: i64,
        speaker_label: &str,
        identity_id: i64,
    ) -> Result<()>;

    /// Rename a speaker identity (sets display_name).
    async fn rename(&self, identity_id: i64, display_name: Option<String>) -> Result<()>;

    /// Delete a speaker identity. The FK is ON DELETE SET NULL so
    /// utterance links are NULLed rather than deleted.
    async fn delete(&self, identity_id: i64) -> Result<()>;

    /// List all identities (no embeddings). For IPC.
    async fn list(&self) -> Result<Vec<SpeakerIdentity>>;

    /// Merge `absorb_id` into `keep_id`: re-link all utterances, update
    /// keep_id's centroid as a weighted mean, delete absorb_id. When the
    /// two voiceprints come from different embedding versions, the
    /// current one wins outright (averaging across spaces is
    /// meaningless), so merging a new identity into an old named one
    /// re-enrols it.
    async fn merge(&self, keep_id: i64, absorb_id: i64) -> Result<()>;
}

/// In-memory no-op store for tests. Every write succeeds silently;
/// reads return empty results.
pub struct MemSpeakerStore;

#[async_trait]
impl SpeakerStore for MemSpeakerStore {
    async fn list_with_embeddings(&self) -> Result<Vec<(i64, Vec<f32>, i64)>> {
        Ok(Vec::new())
    }
    async fn create(&self, _centroid: &[f32], _utterance_count: i64) -> Result<i64> {
        Ok(0)
    }
    async fn update_centroid(
        &self,
        _identity_id: i64,
        _new_centroid: &[f32],
        _new_utterance_count: i64,
    ) -> Result<()> {
        Ok(())
    }
    async fn link_utterances(
        &self,
        _session_id: i64,
        _speaker_label: &str,
        _identity_id: i64,
    ) -> Result<()> {
        Ok(())
    }
    async fn rename(&self, _identity_id: i64, _display_name: Option<String>) -> Result<()> {
        Ok(())
    }
    async fn delete(&self, _identity_id: i64) -> Result<()> {
        Ok(())
    }
    async fn list(&self) -> Result<Vec<SpeakerIdentity>> {
        Ok(Vec::new())
    }
    async fn merge(&self, _keep_id: i64, _absorb_id: i64) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_vector_normalises_and_keeps_zero() {
        let u = unit_vector(&[3.0, 4.0]);
        assert!((u[0] - 0.6).abs() < 1e-6 && (u[1] - 0.8).abs() < 1e-6);
        assert_eq!(unit_vector(&[0.0, 0.0]), vec![0.0, 0.0]);
    }
}
