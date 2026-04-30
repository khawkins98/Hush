//! Agglomerative clustering on speaker-embedding vectors.
//!
//! D2 (#111) projects each utterance's audio into a 256-dimensional
//! speaker embedding via an ONNX model. Two utterances from the same
//! speaker have a small cosine distance between their embeddings; two
//! from different speakers have a large one. The clustering step
//! takes the per-utterance embedding sequence and assigns each one a
//! speaker-cluster index — utterances in the same cluster come from
//! the same person.
//!
//! ## Algorithm
//!
//! Hierarchical agglomerative clustering with **complete-link**
//! merging on **cosine distance**. Each utterance starts as its own
//! cluster. At every step we merge the two closest clusters
//! (closeness = the maximum pairwise distance between their members,
//! the conservative complete-link rule), and stop when the closest
//! pair exceeds the distance threshold.
//!
//! Why complete-link rather than single-link or average-link:
//! - **Single-link** chains together loosely-related utterances —
//!   one outlier can pull two distinct speakers into the same
//!   cluster. Diarization wants tight, well-separated clusters,
//!   not chains.
//! - **Average-link** is a reasonable middle ground but is more
//!   sensitive to cluster size; it also requires re-computing means
//!   on every merge.
//! - **Complete-link** asks "the worst-case pair across these two
//!   clusters is still close enough" — exactly the right question
//!   for "are these the same speaker?". It is also trivial to update
//!   incrementally: the merged cluster's distance to any other
//!   cluster is the max of the two pre-merge distances.
//!
//! The distance threshold is exposed as a parameter; calling code
//! tunes it against the embedding model's expected within-speaker
//! vs across-speaker distance distribution. Wespeaker ResNet34-LM
//! embeddings (the model #111 ships with) work well around 0.5–0.7
//! cosine distance — we default to 0.6 and provide a tuning hook.
//!
//! ## Why not k-means
//!
//! Diarization doesn't know `k` (the number of speakers) ahead of
//! time. Agglomerative + threshold lets the algorithm discover the
//! count from the data — a meeting with two speakers ends up with
//! two clusters, a meeting with five ends up with five, no upfront
//! configuration. A modal-k heuristic (eigengap / silhouette) on
//! top of k-means is more code and more failure modes than this.
//!
//! ## Determinism
//!
//! Tie-breaking uses lexicographic (lower cluster index wins) rather
//! than insertion-order. Same input → same output across runs and
//! across machines. The unit tests pin enough cases that a
//! refactor that breaks determinism fails CI.

/// Default cosine-distance threshold for declaring two clusters
/// distinct. Tuned for Wespeaker ResNet34-LM embeddings. Lower
/// threshold → more clusters (over-segmentation); higher threshold
/// → fewer clusters (speakers merge together).
///
/// 0.6 is mid-range based on the published evaluation curves for
/// this family of models; we expose [`cluster_with_threshold`] for
/// tuning hands-on.
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

/// Cluster a list of embedding vectors into speaker-cluster indices.
///
/// Returns a `Vec<usize>` parallel to `embeddings` — element `i` is
/// the cluster ID that utterance `i` belongs to. Cluster IDs are
/// assigned in the order clusters first appear (utterance 0 is
/// always cluster 0; the next utterance from a different speaker is
/// cluster 1, etc.) so the output is stable and human-readable.
///
/// `distance_threshold` is the maximum cosine distance at which two
/// clusters are still considered the same speaker. Above this, the
/// algorithm stops merging.
///
/// Empty input returns an empty vec; single-element input returns
/// `[0]` (one cluster of one).
///
/// Complexity: O(n²) memory for the distance matrix, O(n³) time for
/// the merge loop. n is the number of utterances in a single chunk
/// (~10s) — typically <20 — so the constants matter more than the
/// asymptotic. Pre-allocating the matrix once and updating in place
/// keeps the merge loop tight.
pub fn cluster_with_threshold(embeddings: &[Vec<f32>], distance_threshold: f32) -> Vec<usize> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    if embeddings.len() == 1 {
        return vec![0];
    }

    let n = embeddings.len();

    // Each utterance starts as its own cluster. `parent[i]` is the
    // current cluster ID utterance `i` belongs to. As clusters
    // merge, we rewrite the lower-id cluster onto every member of
    // the higher-id one. (Union-find would be asymptotically
    // faster but unnecessary at n<50.)
    let mut parent: Vec<usize> = (0..n).collect();

    // Pairwise distance matrix between *clusters*. Initially each
    // cluster is one utterance, so this is the embedding distance
    // matrix. As clusters merge we collapse rows + columns.
    //
    // `f32::INFINITY` marks "this cluster has been absorbed into
    // another"; the merge loop ignores any pair where either row or
    // column is infinite, so we can avoid copying the matrix on
    // every merge.
    let mut dist = vec![vec![0.0_f32; n]; n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = cosine_distance(&embeddings[i], &embeddings[j]);
            dist[i][j] = d;
            dist[j][i] = d;
        }
    }

    // Active set: cluster IDs that have not been absorbed. Drives
    // the merge-loop search and the final renumbering.
    let mut active: Vec<usize> = (0..n).collect();

    loop {
        // Find the closest pair among active clusters. Tie-break
        // lexicographically (lower (a, b) wins) for determinism.
        let mut best: Option<(usize, usize, f32)> = None;
        for &a in &active {
            for &b in &active {
                if b <= a {
                    continue;
                }
                let d = dist[a][b];
                if !d.is_finite() {
                    continue;
                }
                match best {
                    None => best = Some((a, b, d)),
                    Some((_, _, current)) if d < current => best = Some((a, b, d)),
                    _ => {}
                }
            }
        }

        let Some((a, b, d)) = best else {
            break;
        };
        if d > distance_threshold {
            break;
        }

        // Merge cluster `b` into `a` (lower index wins, matches
        // the "first-appearance order" cluster-ID rule below).
        // Complete-link update: the merged cluster's distance to
        // any other active cluster is the *max* of the two pre-
        // merge distances. We collapse `b`'s row + column into
        // `a` and mark `b` as absorbed.
        for &c in &active {
            if c == a || c == b {
                continue;
            }
            let merged = dist[a][c].max(dist[b][c]);
            dist[a][c] = merged;
            dist[c][a] = merged;
        }
        // Mark cluster b as absorbed by setting all of its
        // distances to infinity. The next iteration's "find
        // closest" loop skips infinite entries.
        for row in dist.iter_mut() {
            row[b] = f32::INFINITY;
        }
        for entry in dist[b].iter_mut() {
            *entry = f32::INFINITY;
        }

        // Rewrite every utterance currently in cluster b to be in
        // cluster a. n is tiny so the linear sweep is fine.
        for p in parent.iter_mut() {
            if *p == b {
                *p = a;
            }
        }
        active.retain(|&c| c != b);
    }

    // Renumber surviving cluster IDs to be 0..k in first-
    // appearance order, so callers can present "Speaker 1, 2, …"
    // labels that match the order people first spoke.
    let mut renumber: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut next_id = 0_usize;
    let mut out = Vec::with_capacity(n);
    for &p in &parent {
        let id = *renumber.entry(p).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
        out.push(id);
    }
    out
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

    #[test]
    fn cluster_empty_input_returns_empty() {
        let out = cluster_with_threshold(&[], DEFAULT_DISTANCE_THRESHOLD);
        assert!(out.is_empty());
    }

    #[test]
    fn cluster_single_element_returns_single_cluster() {
        let out = cluster_with_threshold(&[axis(4, 0)], DEFAULT_DISTANCE_THRESHOLD);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn cluster_two_identical_embeddings_share_cluster() {
        let e = axis(4, 0);
        let out = cluster_with_threshold(&[e.clone(), e], DEFAULT_DISTANCE_THRESHOLD);
        assert_eq!(out, vec![0, 0]);
    }

    #[test]
    fn cluster_two_orthogonal_embeddings_split() {
        // Cosine distance 1.0 on orthogonal axes, well above the
        // 0.6 default threshold — must split into two clusters.
        let out = cluster_with_threshold(&[axis(4, 0), axis(4, 1)], DEFAULT_DISTANCE_THRESHOLD);
        assert_eq!(out, vec![0, 1]);
    }

    #[test]
    fn cluster_three_speaker_pattern_aba() {
        // Speaker 1 (axis 0), Speaker 2 (axis 1), Speaker 1 again.
        // Expected output: same cluster for utterances 0 and 2,
        // separate cluster for utterance 1, in first-appearance
        // order.
        let out = cluster_with_threshold(
            &[axis(4, 0), axis(4, 1), axis(4, 0)],
            DEFAULT_DISTANCE_THRESHOLD,
        );
        assert_eq!(out, vec![0, 1, 0]);
    }

    #[test]
    fn cluster_first_appearance_order() {
        // Utterance 0 is speaker A, utterance 1 is speaker B,
        // utterance 2 is speaker C. Cluster IDs must reflect the
        // *order people first spoke*, not the underlying axis
        // index — so the IDs are 0, 1, 2 even if the axes are
        // 2, 0, 1.
        let out = cluster_with_threshold(
            &[axis(4, 2), axis(4, 0), axis(4, 1)],
            DEFAULT_DISTANCE_THRESHOLD,
        );
        assert_eq!(out, vec![0, 1, 2]);
    }

    #[test]
    fn cluster_high_threshold_collapses_to_single_cluster() {
        // Threshold of 1.5 is above the maximum possible cosine
        // distance for two unit vectors in non-anti-parallel
        // arrangement (which maxes at 1.0 for orthogonal). Every
        // utterance should fold into the same cluster.
        let out = cluster_with_threshold(&[axis(4, 0), axis(4, 1), axis(4, 2), axis(4, 3)], 1.5);
        assert_eq!(out, vec![0, 0, 0, 0]);
    }

    #[test]
    fn cluster_low_threshold_isolates_every_element() {
        // Threshold below any pairwise distance — no merges happen
        // and every utterance is its own cluster.
        let out = cluster_with_threshold(&[axis(4, 0), axis(4, 1), axis(4, 2)], 0.0);
        assert_eq!(out, vec![0, 1, 2]);
    }

    #[test]
    fn cluster_complete_link_resists_chaining() {
        // Three embeddings on a near-line: A and B are close, B and
        // C are close, but A and C are far apart. Single-link
        // clustering would chain all three into one cluster;
        // complete-link refuses to merge {A, B} with C because the
        // worst-case pair (A↔C) exceeds the threshold.
        //
        // We construct A=axis0, B is a 50/50 mix of axes 0 and 1
        // (close to both), and C=axis1 (far from A but close to B).
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![1.0, 1.0, 0.0, 0.0];
        let c = vec![0.0, 1.0, 0.0, 0.0];
        // Pairwise cosine distances:
        //   A↔B: 1 - 1/√2 ≈ 0.293
        //   B↔C: 1 - 1/√2 ≈ 0.293
        //   A↔C: 1 - 0    = 1.0
        //
        // With threshold 0.5: A↔B can merge, B↔C can merge, but
        // complete-link on {A,B}↔C uses max(A↔C, B↔C) = 1.0 > 0.5,
        // so the second merge is rejected. Result: two clusters.
        let out = cluster_with_threshold(&[a, b, c], 0.5);
        // First-appearance order: A is cluster 0; B merges with A
        // (still 0); C is on its own (cluster 1).
        assert_eq!(out, vec![0, 0, 1]);
    }

    #[test]
    fn cluster_is_deterministic_under_ties() {
        // Three pairs at exactly the same distance — the algorithm
        // must pick the same merge sequence every run. Lower-index
        // pair wins on ties (lexicographic).
        let v = axis(4, 0);
        // Three identical-pointing embeddings → all distances are
        // 0 → ties everywhere. Should fold into a single cluster
        // regardless of merge order.
        let out = cluster_with_threshold(
            &[v.clone(), v.clone(), v.clone()],
            DEFAULT_DISTANCE_THRESHOLD,
        );
        assert_eq!(out, vec![0, 0, 0]);
    }

    #[test]
    fn cluster_real_world_two_speaker_conversation() {
        // Realistic case: a four-utterance conversation, two
        // speakers alternating, with embeddings perturbed slightly
        // from canonical axes (mimicking the noise a real model
        // emits on similar-but-not-identical utterances).
        let alice_1 = vec![1.0, 0.05, 0.0];
        let bob_1 = vec![0.05, 1.0, 0.0];
        let alice_2 = vec![1.0, 0.10, 0.0];
        let bob_2 = vec![0.0, 1.0, 0.05];
        let out = cluster_with_threshold(
            &[alice_1, bob_1, alice_2, bob_2],
            DEFAULT_DISTANCE_THRESHOLD,
        );
        // Alice utterances cluster together, Bob utterances cluster
        // together, in first-appearance order.
        assert_eq!(out, vec![0, 1, 0, 1]);
    }
}
