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
/// Must be tighter than the in-session threshold (0.4) because a
/// false cross-session merge is permanent — we'd link two different
/// people's entire meeting histories.
pub const AUTO_ACCEPT_THRESHOLD: f32 = 0.25;

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

/// SpeakerStore trait — the mockable seam for cross-session identity.
#[async_trait]
pub trait SpeakerStore: Send + Sync {
    /// Load all known identities with their stored embeddings for
    /// matching. Returns `(id, embedding, utterance_count)` triples.
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
    /// keep_id's centroid as a weighted mean, delete absorb_id.
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
