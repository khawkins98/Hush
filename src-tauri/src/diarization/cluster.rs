//! Cosine-distance helpers for the D2 speaker-embedding diarizer.
//!
//! Production clustering happens in
//! [`crate::diarization::onnx::SessionClusterState`] — online 1-NN
//! with a cosine-distance threshold over the full session history,
//! so cluster IDs are stable across pump ticks. This module is the
//! shared metric: [`cosine_distance`] (used by both the assignment
//! pass and any future re-cluster) plus the
//! [`DEFAULT_DISTANCE_THRESHOLD`] constant.
//!
//! Pre-#310 this file also held an offline complete-link
//! agglomerative `cluster_with_threshold` function. It was the
//! original D2 batch clusterer in PR-D (#298), but #303 replaced
//! it on the production path with the streaming matcher in
//! `onnx.rs` — per-tick agglomerative produced unstable IDs
//! across ticks. The function was kept for "potential batch use"
//! after that, but no caller ever materialised; #310 removed the
//! dead code. The streaming matcher's design rationale, including
//! the choice of 1-NN over agglomerative, is documented inline in
//! `onnx::SessionClusterState::assign`.

/// Default cosine-distance threshold for declaring two clusters
/// distinct. Tuned for Wespeaker ResNet34-LM embeddings. Lower
/// threshold → more clusters (over-segmentation); higher threshold
/// → fewer clusters (speakers merge together).
///
/// 0.6 is mid-range based on the published evaluation curves for
/// this family of models. Exposed so future tuning can flow
/// through a settings row without API churn.
pub const DEFAULT_DISTANCE_THRESHOLD: f32 = 0.6;

/// Cosine distance between two embedding vectors. Distance, not
/// similarity: `0.0` is identical, `2.0` is anti-correlated.
///
/// Returns `1.0` (the "no information" distance) when either vector
/// has zero magnitude. Embedding models occasionally emit a
/// near-zero vector for silence-only utterances; refusing to divide
/// by zero is the right shape, and "treat as unrelated" is a safer
/// default than "identical" — a silent utterance should not collapse
/// into whichever cluster happens to be first.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(
        a.len(),
        b.len(),
        "cosine_distance requires equal-length vectors"
    );
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom <= f32::EPSILON {
        return 1.0;
    }
    let sim = (dot / denom).clamp(-1.0, 1.0);
    1.0 - sim
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an embedding vector pointing along a single axis. Two
    /// embeddings sharing an axis have cosine distance 0; two on
    /// orthogonal axes have cosine distance 1.
    fn axis(dim: usize, axis_index: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; dim];
        v[axis_index] = 1.0;
        v
    }

    #[test]
    fn cosine_distance_identical_is_zero() {
        let a = vec![1.0, 2.0, 3.0];
        let d = cosine_distance(&a, &a);
        assert!(d.abs() < 1e-6, "expected ≈0, got {d}");
    }

    #[test]
    fn cosine_distance_orthogonal_is_one() {
        let a = axis(3, 0);
        let b = axis(3, 1);
        let d = cosine_distance(&a, &b);
        assert!((d - 1.0).abs() < 1e-6, "expected 1.0, got {d}");
    }

    #[test]
    fn cosine_distance_anti_parallel_is_two() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let d = cosine_distance(&a, &b);
        assert!((d - 2.0).abs() < 1e-6, "expected 2.0, got {d}");
    }

    #[test]
    fn cosine_distance_zero_vector_returns_one() {
        // Embedding models occasionally emit a near-zero vector for
        // silence-only utterances. Treating that as "unrelated"
        // (distance 1) is safer than dividing by zero or returning
        // 0 (which would collapse the silence into whichever
        // cluster the loop visits first).
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-6);
        assert!((cosine_distance(&b, &a) - 1.0).abs() < 1e-6);
        assert!((cosine_distance(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_distance_scale_invariance() {
        // Magnitude must not affect distance — that's the entire
        // point of using cosine over euclidean for embeddings.
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![10.0, 20.0, 30.0];
        let d = cosine_distance(&a, &b);
        assert!(
            d.abs() < 1e-6,
            "scaled-up vector should be identical, got {d}"
        );
    }
}
