//! Online speaker-cluster state and the session-end re-cluster (#1013).
//!
//! ## Online assignment
//!
//! Each new embedding is matched against one centroid per speaker
//! (cosine distance, threshold [`super::cluster::DEFAULT_DISTANCE_THRESHOLD`]
//! or the user's slider value). Three refinements over the pre-#1013
//! running-mean matcher:
//!
//! - **Duration gate** (idea from diart's `rho_update` / `delta_new`,
//!   MIT). Embeddings from short chunks are noisy — speaker-verification
//!   EER roughly doubles below 2 s — so a chunk shorter than
//!   [`UPDATE_MIN_SECS`] may only be *assigned*; it never moves a
//!   centroid. Founding a new speaker needs [`CREATE_MIN_SECS`]. A chunk
//!   too short to found a speaker but beyond threshold of every existing
//!   one is assigned to the nearest (a guess the re-cluster can revise)
//!   rather than spawning a one-off "Speaker 4". The constants come from
//!   the #1013 duration study (wespeaker with CMN, LibriSpeech,
//!   leave-one-out speaker centroids): at 0.8 s the 90th-percentile
//!   distance to the speaker's own centroid is about 0.57–0.60, level
//!   with the 0.6 threshold, so sub-second chunks routinely look like a
//!   new speaker, and 1.4–2.0 % land nearest a wrong centroid; at 1.2 s
//!   that drops to p90 0.45–0.49 and 0.2–0.3 %; from 2 s, 0 %.
//! - **Centroid = sum of unit vectors**, normalised on read (diart /
//!   sherpa-onnx style). wespeaker embedding norms vary with loudness and
//!   duration; a running mean of raw vectors lets one loud utterance
//!   drag the centroid. Summing unit vectors weights every accepted
//!   utterance equally. (On the level-normalised LibriSpeech evaluation
//!   this measured as neutral; it is kept for the real-call case where
//!   levels swing.)
//! - **Hints from the pump** ([`ChunkHint`]): a chunk overlapping local
//!   mic speech may not update or create (overlap guard), and clusters
//!   live in per-channel namespaces that never match each other
//!   (cannot-link).
//!
//! ## Session-end re-cluster (#316)
//!
//! Online 1-NN matching commits to a label the moment an utterance
//! arrives and can chain a drifting voice into the wrong centroid. Chen
//! et al. 2022 report naive online centroids at 36.8 % DER versus
//! 19.6 % once a global re-clustering pass plus label matching is added.
//! So every embedding is retained for the session (unit vectors,
//! zeroized on drop like the centroids) and [`SessionClusterState::recluster`]
//! runs once in background finalization:
//!
//! 1. Per namespace, average-linkage agglomerative clustering
//!    (duration-weighted UPGMA, NN-chain algorithm so it is O(n²)) over
//!    chunks long enough to trust, stopping at the live threshold.
//! 2. Clusters with less than [`TINY_CLUSTER_SECS`] of speech are folded
//!    into the nearest real cluster (one stray cough must not be a
//!    speaker); short / overlapped chunks are then attached to the
//!    nearest cluster.
//! 3. New clusters are greedily matched to the live IDs by shared speech
//!    duration, so "Speaker 1" keeps its name whenever the re-cluster
//!    agrees with it.
//!
//! The algorithms are implemented from their published descriptions;
//! no code was copied from diart, sherpa-onnx or the paper's artefacts.

use super::cluster::cosine_distance;
use super::{ChunkHint, Relabel, SpeakerNamespace};
/// Unit-normalise (zero vector stays zero; cosine distance then reports
/// 1.0, "unrelated"). Shared with the speaker store.
pub use crate::speakers::unit_vector as normalised;

/// Chunks shorter than this may be assigned but never update a centroid.
pub const UPDATE_MIN_SECS: f32 = 1.0;

/// Chunks shorter than this can never found a new speaker.
pub const CREATE_MIN_SECS: f32 = 1.5;

/// Chunks shorter than this sit out the re-cluster's AHC pass and are
/// attached to the nearest resulting cluster afterwards.
pub const RECLUSTER_MIN_SECS: f32 = 1.0;

/// A re-clustered speaker with less total speech than this is folded
/// into its nearest neighbour (when a larger cluster exists).
pub const TINY_CLUSTER_SECS: f32 = 4.0;

/// Cap on per-chunk weight in the duration-weighted linkage, so one
/// 30 s monologue chunk doesn't outweigh twenty normal turns.
const MAX_LINKAGE_WEIGHT_SECS: f32 = 10.0;

/// Above this many retained chunks the O(n²) distance matrix (4 bytes
/// per pair) stops being a rounding error, so the re-cluster is skipped
/// and the live labels stand. 3 000 chunks ≈ 36 MB, a multi-hour call.
const MAX_RECLUSTER_ITEMS: usize = 3_000;

/// Overlap-guard escape hatch: after this many guarded-namespace chunks…
const OVERLAP_ESCAPE_MIN_CHUNKS: usize = 12;
/// …if more than this fraction were flagged as overlapping, the mic is
/// almost certainly hearing the remote audio (speakerphone, no
/// headset), so "mic active" says nothing about crosstalk. The guard is
/// switched off for the rest of the session instead of starving the
/// diarizer of updates.
const OVERLAP_ESCAPE_FRACTION: f32 = 0.6;

/// Matching policy. Production is [`ClusterPolicy::default`]; the
/// pre-#1013 behaviour is kept for the evaluation harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterPolicy {
    /// Apply [`UPDATE_MIN_SECS`] / [`CREATE_MIN_SECS`].
    pub duration_gate: bool,
    /// Accumulate unit vectors (true) or raw vectors (pre-#1013 running
    /// mean; the mean's direction equals the raw sum's).
    pub unit_sum_centroid: bool,
}

impl Default for ClusterPolicy {
    fn default() -> Self {
        Self {
            duration_gate: true,
            unit_sum_centroid: true,
        }
    }
}

impl ClusterPolicy {
    /// The matcher as it shipped before #1013.
    #[cfg(test)]
    pub const LEGACY: Self = Self {
        duration_gate: false,
        unit_sum_centroid: false,
    };
}

/// What the caller knows about a chunk besides its embedding.
#[derive(Debug, Clone, Copy)]
pub struct ChunkMeta {
    pub duration_secs: f32,
    pub hint: ChunkHint,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
}

struct Cluster {
    id: usize,
    namespace: SpeakerNamespace,
    /// Sum of accepted embeddings (unit vectors under the default
    /// policy). Direction is the centroid.
    sum: Vec<f32>,
    count: usize,
    duration_secs: f32,
}

impl Cluster {
    fn centroid(&self) -> Vec<f32> {
        normalised(&self.sum)
    }
}

/// One retained chunk, for the re-cluster.
struct Record {
    unit: Vec<f32>,
    duration_secs: f32,
    hint: ChunkHint,
    started_at_ms: u64,
    ended_at_ms: u64,
    cluster_id: Option<usize>,
    /// The label the pump persisted for this chunk (a speaker label, or
    /// the fallback source tag when the diarizer abstained).
    label: String,
}

/// Online speaker-cluster state for one diarisation session.
///
/// Privacy invariant: centroids and retained embeddings are speaker
/// biometrics and are zeroized in `Drop`, the same as the raw PCM.
pub struct SessionClusterState {
    clusters: Vec<Cluster>,
    records: Vec<Record>,
    next_id: usize,
    /// Maximum cosine distance at which an embedding still matches a
    /// centroid.
    pub distance_threshold: f32,
    policy: ClusterPolicy,
    /// The [`SpeakerNamespace::LocalRoom`] cluster labelled `"mic"`.
    local_primary: Option<usize>,
    guard_seen: usize,
    guard_flagged: usize,
    guard_escaped: bool,
}

impl SessionClusterState {
    pub fn new(distance_threshold: f32) -> Self {
        Self::with_policy(distance_threshold, ClusterPolicy::default())
    }

    pub fn with_policy(distance_threshold: f32, policy: ClusterPolicy) -> Self {
        Self {
            clusters: Vec::new(),
            records: Vec::new(),
            next_id: 0,
            distance_threshold,
            policy,
            local_primary: None,
            guard_seen: 0,
            guard_flagged: 0,
            guard_escaped: false,
        }
    }

    /// The label for a cluster ID: `"mic"` for the dominant in-room
    /// cluster, `"Speaker N"` (1-indexed) otherwise.
    pub fn label(&self, id: usize) -> String {
        if self.local_primary == Some(id) {
            crate::audio::LOCAL_SPEAKER_TAG.to_owned()
        } else {
            format!("Speaker {}", id + 1)
        }
    }

    /// Number of live clusters (all namespaces).
    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    /// Whether the overlap guard has been switched off for this session.
    pub fn overlap_guard_escaped(&self) -> bool {
        self.guard_escaped
    }

    /// Track the overlap-guard flag rate and decide whether this chunk's
    /// `may_update = false` is honoured.
    fn effective_may_update(&mut self, hint: &ChunkHint) -> bool {
        // Only the remote stream is ever flagged; counting mic chunks
        // would dilute the rate the escape hatch keys on.
        if hint.namespace != SpeakerNamespace::Shared {
            return hint.may_update;
        }
        self.guard_seen += 1;
        if !hint.may_update {
            self.guard_flagged += 1;
        }
        if !self.guard_escaped
            && self.guard_seen >= OVERLAP_ESCAPE_MIN_CHUNKS
            && (self.guard_flagged as f32) > OVERLAP_ESCAPE_FRACTION * self.guard_seen as f32
        {
            self.guard_escaped = true;
            tracing::info!(
                flagged = self.guard_flagged,
                seen = self.guard_seen,
                "diarizer: most remote chunks overlap mic activity (speakerphone?); \
                 overlap guard disabled for the rest of the session"
            );
        }
        hint.may_update || self.guard_escaped
    }

    /// Assign a cluster to `embedding`. Returns the cluster ID, or `None`
    /// when the diarizer abstains (no cluster in this namespace yet and
    /// the chunk may not found one) — the caller then keeps the source
    /// label.
    pub fn assign(&mut self, embedding: &[f32], meta: ChunkMeta) -> Option<usize> {
        let unit = normalised(embedding);
        let may_update = self.effective_may_update(&meta.hint);
        let (can_update, can_create) = if self.policy.duration_gate {
            (
                may_update && meta.duration_secs >= UPDATE_MIN_SECS,
                may_update && meta.duration_secs >= CREATE_MIN_SECS,
            )
        } else {
            (may_update, may_update)
        };

        let mut best: Option<(usize, f32)> = None;
        for (idx, c) in self.clusters.iter().enumerate() {
            if c.namespace != meta.hint.namespace {
                continue; // cannot-link across channels
            }
            let d = cosine_distance(&unit, &c.centroid());
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((idx, d));
            }
        }

        let accumulate = if self.policy.unit_sum_centroid {
            unit.as_slice()
        } else {
            embedding
        };

        // INFO so it lands in the on-disk log by default (#316
        // diagnostic): one line per utterance, the diarizer's cadence.
        let assigned = match best {
            Some((idx, d)) if d <= self.distance_threshold => {
                if can_update {
                    let c = &mut self.clusters[idx];
                    for (s, e) in c.sum.iter_mut().zip(accumulate) {
                        *s += e;
                    }
                    c.count += 1;
                    c.duration_secs += meta.duration_secs;
                }
                let id = self.clusters[idx].id;
                tracing::info!(
                    speaker = id + 1,
                    distance = d,
                    threshold = self.distance_threshold,
                    duration_secs = meta.duration_secs,
                    updated = can_update,
                    "diarizer: matched existing cluster"
                );
                Some(id)
            }
            _ if can_create => {
                let id = self.next_id;
                self.next_id += 1;
                self.clusters.push(Cluster {
                    id,
                    namespace: meta.hint.namespace,
                    sum: accumulate.to_vec(),
                    count: 1,
                    duration_secs: meta.duration_secs,
                });
                if meta.hint.namespace == SpeakerNamespace::LocalRoom
                    && self.local_primary.is_none()
                {
                    self.local_primary = Some(id);
                }
                tracing::info!(
                    speaker = id + 1,
                    best_distance = best.map(|(_, d)| d),
                    threshold = self.distance_threshold,
                    duration_secs = meta.duration_secs,
                    cluster_count = self.clusters.len(),
                    "diarizer: NEW cluster"
                );
                Some(id)
            }
            Some((idx, d)) => {
                let id = self.clusters[idx].id;
                tracing::info!(
                    speaker = id + 1,
                    distance = d,
                    threshold = self.distance_threshold,
                    duration_secs = meta.duration_secs,
                    may_update,
                    "diarizer: beyond threshold but chunk may not found a speaker; \
                     assigned to nearest"
                );
                Some(id)
            }
            None => {
                tracing::info!(
                    duration_secs = meta.duration_secs,
                    may_update,
                    "diarizer: abstained (no speaker yet and chunk may not found one)"
                );
                None
            }
        };

        let label = match assigned {
            Some(id) => self.label(id),
            None => meta.hint.fallback_label.to_owned(),
        };
        self.records.push(Record {
            unit,
            duration_secs: meta.duration_secs,
            hint: meta.hint,
            started_at_ms: meta.started_at_ms,
            ended_at_ms: meta.ended_at_ms,
            cluster_id: assigned,
            label,
        });
        assigned
    }

    /// `(cluster_id, unit centroid, utterance_count)` per speaker cluster,
    /// for cross-session identity resolution (#667). The in-room `"mic"`
    /// cluster is excluded: its rows carry the `"mic"` tag rather than a
    /// `"Speaker N"` label, so it could never be linked anyway, and
    /// enrolling the local user is a separate product decision (#1006).
    ///
    /// `utterance_count` counts every chunk attributed to the cluster
    /// (including assign-only ones), which is what the identity store's
    /// "enough evidence" floor wants.
    pub fn centroids(&self) -> Vec<(usize, Vec<f32>, usize)> {
        let mut out: Vec<(usize, Vec<f32>, usize)> = self
            .clusters
            .iter()
            .filter(|c| Some(c.id) != self.local_primary)
            .map(|c| {
                let attributed = self
                    .records
                    .iter()
                    .filter(|r| r.cluster_id == Some(c.id))
                    .count()
                    .max(c.count);
                (c.id, c.centroid(), attributed)
            })
            .collect();
        out.sort_by_key(|(id, _, _)| *id);
        out
    }

    /// Session-end re-cluster. See the module docs for the algorithm.
    /// `threshold` is the average-linkage cosine-distance stop criterion.
    /// Rebuilds the cluster state from the result and returns the
    /// persisted-label changes.
    pub fn recluster(&mut self, threshold: f32) -> Vec<Relabel> {
        if self.records.len() > MAX_RECLUSTER_ITEMS {
            tracing::warn!(
                records = self.records.len(),
                cap = MAX_RECLUSTER_ITEMS,
                "diarizer re-cluster: too many chunks; keeping live labels"
            );
            return Vec::new();
        }

        let mut new_clusters: Vec<Cluster> = Vec::new();
        let mut new_ids: Vec<Option<usize>> = self.records.iter().map(|r| r.cluster_id).collect();
        let mut new_local_primary = None;
        let mut next_id = self.next_id;

        for ns in [SpeakerNamespace::Shared, SpeakerNamespace::LocalRoom] {
            let members: Vec<usize> = (0..self.records.len())
                .filter(|&i| self.records[i].hint.namespace == ns)
                .collect();
            let trusted: Vec<usize> = members
                .iter()
                .copied()
                .filter(|&i| {
                    let r = &self.records[i];
                    r.duration_secs >= RECLUSTER_MIN_SECS
                        && (r.hint.may_update || self.guard_escaped)
                })
                .collect();
            if trusted.len() < 2 {
                // Too little to re-cluster: keep this namespace's live
                // clusters and labels as they are.
                for c in self.clusters.iter().filter(|c| c.namespace == ns) {
                    new_clusters.push(Cluster {
                        id: c.id,
                        namespace: c.namespace,
                        sum: c.sum.clone(),
                        count: c.count,
                        duration_secs: c.duration_secs,
                    });
                }
                if ns == SpeakerNamespace::LocalRoom {
                    new_local_primary = self.local_primary;
                }
                continue;
            }

            // 1. AHC over trusted chunks.
            let units: Vec<&[f32]> = trusted
                .iter()
                .map(|&i| self.records[i].unit.as_slice())
                .collect();
            let weights: Vec<f32> = trusted
                .iter()
                .map(|&i| self.records[i].duration_secs.min(MAX_LINKAGE_WEIGHT_SECS))
                .collect();
            let groups = average_linkage(&units, &weights, threshold);

            // Group → member record indices.
            let n_groups = groups.iter().copied().max().map_or(0, |m| m + 1);
            let mut group_members: Vec<Vec<usize>> = vec![Vec::new(); n_groups];
            for (k, &g) in groups.iter().enumerate() {
                group_members[g].push(trusted[k]);
            }

            // 2. Fold tiny groups into the nearest big one.
            let durations: Vec<f32> = group_members
                .iter()
                .map(|m| m.iter().map(|&i| self.records[i].duration_secs).sum())
                .collect();
            let big: Vec<usize> = (0..n_groups)
                .filter(|&g| durations[g] >= TINY_CLUSTER_SECS)
                .collect();
            if !big.is_empty() {
                let big_centroids: Vec<(usize, Vec<f32>)> = big
                    .iter()
                    .map(|&g| (g, self.sum_centroid(&group_members[g])))
                    .collect();
                for g in 0..n_groups {
                    if durations[g] >= TINY_CLUSTER_SECS {
                        continue;
                    }
                    let moved = std::mem::take(&mut group_members[g]);
                    for i in moved {
                        let target = nearest(&self.records[i].unit, &big_centroids);
                        group_members[target].push(i);
                    }
                }
            }
            let groups_final: Vec<Vec<usize>> = group_members
                .into_iter()
                .filter(|m| !m.is_empty())
                .collect();

            // 3. Attach untrusted chunks (short / overlapped) to the
            //    nearest group, judged against the trusted centroids.
            let centroids: Vec<(usize, Vec<f32>)> = groups_final
                .iter()
                .enumerate()
                .map(|(g, m)| (g, self.sum_centroid(m)))
                .collect();
            let mut assignment: Vec<(usize, usize)> = Vec::new(); // (record, group)
            for (g, m) in groups_final.iter().enumerate() {
                for &i in m {
                    assignment.push((i, g));
                }
            }
            for &i in &members {
                if !trusted.contains(&i) {
                    assignment.push((i, nearest(&self.records[i].unit, &centroids)));
                }
            }

            // 4. Match groups to live IDs by shared speech duration
            //    (greedy on descending overlap; K is a handful, so this
            //    agrees with Hungarian in practice and stays readable).
            let mut overlap: Vec<(f32, usize, usize)> = Vec::new(); // (secs, group, live id)
            {
                use std::collections::HashMap;
                let mut acc: HashMap<(usize, usize), f32> = HashMap::new();
                for &(i, g) in &assignment {
                    if let Some(live) = self.records[i].cluster_id {
                        *acc.entry((g, live)).or_default() += self.records[i].duration_secs;
                    }
                }
                for ((g, live), secs) in acc {
                    overlap.push((secs, g, live));
                }
            }
            overlap.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
                    .then(a.2.cmp(&b.2))
            });
            let mut group_id: Vec<Option<usize>> = vec![None; groups_final.len()];
            let mut used_live: Vec<usize> = Vec::new();
            for (_, g, live) in overlap {
                if group_id[g].is_none() && !used_live.contains(&live) {
                    group_id[g] = Some(live);
                    used_live.push(live);
                }
            }
            // Unmatched groups get fresh IDs, in first-appearance order.
            let mut order: Vec<usize> = (0..groups_final.len()).collect();
            order.sort_by_key(|&g| {
                groups_final[g]
                    .iter()
                    .map(|&i| self.records[i].started_at_ms)
                    .min()
                    .unwrap_or(u64::MAX)
            });
            for g in order {
                if group_id[g].is_none() {
                    group_id[g] = Some(next_id);
                    next_id += 1;
                }
            }

            // Rebuild clusters (from trusted members only, so the
            // centroids the identity store sees carry no short-chunk
            // noise) and record assignments.
            let mut ns_clusters: Vec<Cluster> = Vec::new();
            for (g, m) in groups_final.iter().enumerate() {
                let id = group_id[g].expect("every group was given an id");
                let mut sum = vec![0.0_f32; self.records[m[0]].unit.len()];
                for &i in m {
                    for (s, v) in sum.iter_mut().zip(&self.records[i].unit) {
                        *s += v;
                    }
                }
                ns_clusters.push(Cluster {
                    id,
                    namespace: ns,
                    sum,
                    count: m.len(),
                    duration_secs: m.iter().map(|&i| self.records[i].duration_secs).sum(),
                });
            }
            for &(i, g) in &assignment {
                new_ids[i] = group_id[g];
            }
            if ns == SpeakerNamespace::LocalRoom {
                // The dominant in-room voice is the local user.
                new_local_primary = ns_clusters
                    .iter()
                    .max_by(|a, b| {
                        a.duration_secs
                            .partial_cmp(&b.duration_secs)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|c| c.id);
            }
            new_clusters.extend(ns_clusters);
        }

        let before = self.clusters.len();
        let mut old = std::mem::replace(&mut self.clusters, new_clusters);
        zeroize_clusters(&mut old);
        self.local_primary = new_local_primary;
        self.next_id = next_id;

        let mut relabels = Vec::new();
        for (i, new_id) in new_ids.into_iter().enumerate() {
            let new_label = match new_id {
                Some(id) => self.label(id),
                None => self.records[i].hint.fallback_label.to_owned(),
            };
            let r = &mut self.records[i];
            if new_label != r.label {
                relabels.push(Relabel {
                    started_at_ms: r.started_at_ms,
                    ended_at_ms: r.ended_at_ms,
                    old_label: std::mem::replace(&mut r.label, new_label.clone()),
                    new_label,
                });
            }
            r.cluster_id = new_id;
        }
        tracing::info!(
            clusters_before = before,
            clusters_after = self.clusters.len(),
            chunks = self.records.len(),
            relabelled = relabels.len(),
            threshold,
            "diarizer re-cluster: done"
        );
        relabels
    }

    /// Unit centroid of a set of records.
    fn sum_centroid(&self, members: &[usize]) -> Vec<f32> {
        let dim = members.first().map_or(0, |&i| self.records[i].unit.len());
        let mut sum = vec![0.0_f32; dim];
        for &i in members {
            for (s, v) in sum.iter_mut().zip(&self.records[i].unit) {
                *s += v;
            }
        }
        normalised(&sum)
    }
}

impl Drop for SessionClusterState {
    fn drop(&mut self) {
        // Speaker embeddings are biometric voiceprints: zeroize before
        // the allocations go back to the allocator, the same privacy
        // claim as the raw PCM buffers.
        use zeroize::Zeroize;
        zeroize_clusters(&mut self.clusters);
        for r in &mut self.records {
            r.unit.zeroize();
        }
    }
}

fn zeroize_clusters(clusters: &mut [Cluster]) {
    use zeroize::Zeroize;
    for c in clusters {
        c.sum.zeroize();
    }
}

/// Index (into `centroids`' first field) of the nearest centroid.
fn nearest(unit: &[f32], centroids: &[(usize, Vec<f32>)]) -> usize {
    centroids
        .iter()
        .map(|(g, c)| (*g, cosine_distance(unit, c)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(g, _)| g)
        .expect("nearest() needs at least one centroid")
}

/// Weighted average-linkage (UPGMA) agglomerative clustering over unit
/// vectors with cosine distance, cut at `threshold`. Returns a dense
/// group index per item, numbered in order of each group's first item.
///
/// Uses the nearest-neighbour-chain algorithm (Murtagh 1983): average
/// linkage is *reducible* — merging two clusters never brings the merged
/// cluster closer to a third than the nearer of its parts — so reciprocal
/// nearest neighbours can be merged in any order and the result equals
/// the classic greedy closest-pair dendrogram. Reducibility also lets a
/// reciprocal pair further apart than `threshold` be retired outright:
/// nothing can ever come within threshold of either.
pub fn average_linkage(units: &[&[f32]], weights: &[f32], threshold: f32) -> Vec<usize> {
    let n = units.len();
    debug_assert_eq!(n, weights.len());
    if n == 0 {
        return Vec::new();
    }
    let mut dist = vec![0.0_f32; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            let d = cosine_distance(units[i], units[j]);
            dist[i * n + j] = d;
            dist[j * n + i] = d;
        }
    }
    let mut weight: Vec<f32> = weights.iter().map(|w| w.max(1e-3)).collect();
    let mut active = vec![true; n]; // still a cluster representative
    let mut retired = vec![false; n]; // can no longer merge under threshold
    let mut parent: Vec<usize> = (0..n).collect();
    let mut chain: Vec<usize> = Vec::new();

    loop {
        if chain.is_empty() {
            match (0..n).find(|&i| active[i] && !retired[i]) {
                Some(start) => chain.push(start),
                None => break,
            }
        }
        let a = *chain.last().expect("chain is non-empty");
        let prev = if chain.len() >= 2 {
            Some(chain[chain.len() - 2])
        } else {
            None
        };
        let mut best: Option<(usize, f32)> = None;
        for b in 0..n {
            if b == a || !active[b] || retired[b] {
                continue;
            }
            let d = dist[a * n + b];
            // Prefer `prev` on ties so reciprocal pairs are recognised.
            let better = match best {
                None => true,
                Some((bb, bd)) => d < bd || (d == bd && Some(b) == prev && Some(bb) != prev),
            };
            if better {
                best = Some((b, d));
            }
        }
        let Some((b, d)) = best else {
            // `a` is the last mergeable cluster standing.
            retired[a] = true;
            chain.pop();
            continue;
        };
        if Some(b) != prev {
            chain.push(b);
            continue;
        }
        // Reciprocal nearest neighbours: a ↔ b.
        chain.pop();
        chain.pop();
        if d > threshold {
            retired[a] = true;
            retired[b] = true;
            continue;
        }
        // Merge b into a (Lance–Williams update for weighted average).
        let (wa, wb) = (weight[a], weight[b]);
        for k in 0..n {
            if !active[k] || k == a || k == b {
                continue;
            }
            let nd = (wa * dist[a * n + k] + wb * dist[b * n + k]) / (wa + wb);
            dist[a * n + k] = nd;
            dist[k * n + a] = nd;
        }
        weight[a] = wa + wb;
        active[b] = false;
        parent[b] = a;
    }

    fn root(parent: &[usize], mut i: usize) -> usize {
        while parent[i] != i {
            i = parent[i];
        }
        i
    }
    let mut dense: Vec<Option<usize>> = vec![None; n];
    let mut next = 0;
    (0..n)
        .map(|i| {
            let r = root(&parent, i);
            *dense[r].get_or_insert_with(|| {
                next += 1;
                next - 1
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: usize = 16;

    fn axis(idx: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        v[idx] = 1.0;
        v
    }

    /// Unit vector mostly along `idx` with a little of `other` mixed in.
    fn near(idx: usize, other: usize, amount: f32) -> Vec<f32> {
        let mut v = axis(idx);
        v[other] += amount;
        normalised(&v)
    }

    fn meta(duration_secs: f32) -> ChunkMeta {
        meta_at(duration_secs, 0)
    }

    fn meta_at(duration_secs: f32, started_at_ms: u64) -> ChunkMeta {
        ChunkMeta {
            duration_secs,
            hint: ChunkHint::default(),
            started_at_ms,
            ended_at_ms: started_at_ms + (duration_secs * 1000.0) as u64,
        }
    }

    fn with_hint(m: ChunkMeta, hint: ChunkHint) -> ChunkMeta {
        ChunkMeta { hint, ..m }
    }

    // ---- online assignment (carried over from the pre-#1013 tests) ----

    #[test]
    fn first_embedding_gets_id_zero_and_label_speaker_1() {
        let mut s = SessionClusterState::new(0.4);
        assert_eq!(s.assign(&axis(0), meta(3.0)), Some(0));
        assert_eq!(s.label(0), "Speaker 1");
    }

    #[test]
    fn ids_are_stable_and_in_first_appearance_order() {
        let mut s = SessionClusterState::new(0.4);
        assert_eq!(s.assign(&axis(2), meta(3.0)), Some(0));
        assert_eq!(s.assign(&axis(0), meta(3.0)), Some(1));
        assert_eq!(s.assign(&axis(2), meta(3.0)), Some(0));
        assert_eq!(s.assign(&axis(1), meta(3.0)), Some(2));
        assert_eq!(s.assign(&axis(0), meta(3.0)), Some(1));
    }

    // ---- duration gate (#1013 item 2) ----

    #[test]
    fn short_chunk_cannot_found_a_speaker() {
        let mut s = SessionClusterState::new(0.4);
        // Nothing exists yet and the chunk is too short: abstain.
        assert_eq!(s.assign(&axis(0), meta(1.2)), None);
        assert_eq!(s.cluster_count(), 0);
        assert_eq!(s.assign(&axis(0), meta(2.0)), Some(0));
        // Orthogonal but short: assigned to nearest, no new cluster.
        assert_eq!(s.assign(&axis(1), meta(1.2)), Some(0));
        assert_eq!(s.cluster_count(), 1);
        // Long enough: founds Speaker 2.
        assert_eq!(s.assign(&axis(1), meta(1.5)), Some(1));
    }

    #[test]
    fn short_chunk_does_not_move_the_centroid() {
        let mut s = SessionClusterState::new(0.4);
        s.assign(&axis(0), meta(3.0));
        // A within-threshold but short chunk leaning towards axis 1.
        s.assign(&near(0, 1, 0.6), meta(0.5));
        let c = &s.centroids()[0].1;
        assert!((c[0] - 1.0).abs() < 1e-6 && c[1].abs() < 1e-6, "{c:?}");
        // The same chunk at 1 s does move it.
        s.assign(&near(0, 1, 0.6), meta(1.0));
        let c = &s.centroids()[0].1;
        assert!(c[1] > 0.1, "{c:?}");
    }

    #[test]
    fn legacy_policy_has_no_duration_gate() {
        let mut s = SessionClusterState::with_policy(0.4, ClusterPolicy::LEGACY);
        assert_eq!(s.assign(&axis(0), meta(0.3)), Some(0));
        assert_eq!(s.assign(&axis(1), meta(0.3)), Some(1));
    }

    // ---- centroid math (#1013 item 3) ----

    #[test]
    fn centroid_is_normalised_sum_of_unit_vectors() {
        let mut s = SessionClusterState::new(0.6);
        // Same direction family, wildly different norms.
        let mut loud = near(0, 1, 0.3);
        for v in &mut loud {
            *v *= 50.0;
        }
        let quiet = near(0, 2, 0.3);
        s.assign(&loud, meta(3.0));
        s.assign(&quiet, meta(3.0));
        let c = &s.centroids()[0].1;
        let norm: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "centroid not unit: {norm}");
        // Equal weight: axis 1 and axis 2 contributions match.
        assert!((c[1] - c[2]).abs() < 1e-5, "{c:?}");

        // Legacy running mean lets the loud vector dominate.
        let mut l = SessionClusterState::with_policy(0.6, ClusterPolicy::LEGACY);
        l.assign(&loud, meta(3.0));
        l.assign(&quiet, meta(3.0));
        let c = &l.centroids()[0].1;
        assert!(c[1] > 10.0 * c[2], "{c:?}");
    }

    // ---- overlap guard (#1013 item 6) ----

    fn overlapped() -> ChunkHint {
        ChunkHint {
            may_update: false,
            ..ChunkHint::default()
        }
    }

    #[test]
    fn overlapped_chunk_may_assign_but_not_create_or_update() {
        let mut s = SessionClusterState::new(0.4);
        assert_eq!(s.assign(&axis(0), with_hint(meta(3.0), overlapped())), None);
        assert_eq!(s.assign(&axis(0), meta(3.0)), Some(0));
        assert_eq!(
            s.assign(&axis(1), with_hint(meta(3.0), overlapped())),
            Some(0)
        );
        assert_eq!(s.cluster_count(), 1);
        s.assign(&near(0, 1, 0.6), with_hint(meta(3.0), overlapped()));
        let c = &s.centroids()[0].1;
        assert!(
            c[1].abs() < 1e-6,
            "overlapped chunk moved the centroid: {c:?}"
        );
    }

    #[test]
    fn overlap_guard_escapes_when_nearly_everything_overlaps() {
        let mut s = SessionClusterState::new(0.4);
        for _ in 0..OVERLAP_ESCAPE_MIN_CHUNKS {
            s.assign(&axis(0), with_hint(meta(3.0), overlapped()));
        }
        assert!(s.overlap_guard_escaped());
        // Now an overlapped chunk can found a speaker.
        assert!(s
            .assign(&axis(1), with_hint(meta(3.0), overlapped()))
            .is_some());
        assert!(s.cluster_count() >= 1);
    }

    #[test]
    fn overlap_guard_does_not_escape_on_occasional_overlap() {
        let mut s = SessionClusterState::new(0.4);
        for i in 0..40 {
            let hint = if i % 4 == 0 {
                overlapped()
            } else {
                ChunkHint::default()
            };
            s.assign(&axis(0), with_hint(meta(3.0), hint));
        }
        assert!(!s.overlap_guard_escaped());
    }

    // ---- namespaces (#1013 item 7) ----

    fn room() -> ChunkHint {
        ChunkHint {
            namespace: SpeakerNamespace::LocalRoom,
            may_update: true,
            fallback_label: crate::audio::LOCAL_SPEAKER_TAG,
        }
    }

    #[test]
    fn namespaces_never_match_each_other() {
        let mut s = SessionClusterState::new(0.4);
        let remote = s.assign(&axis(0), meta(3.0)).unwrap();
        // Identical embedding on the room channel: must NOT join the
        // remote cluster.
        let local = s.assign(&axis(0), with_hint(meta(3.0), room())).unwrap();
        assert_ne!(remote, local);
        assert_eq!(
            s.label(local),
            "mic",
            "first room cluster is the local user"
        );
        // A second in-room voice gets a Speaker label.
        let guest = s.assign(&axis(3), with_hint(meta(3.0), room())).unwrap();
        assert_eq!(s.label(guest), format!("Speaker {}", guest + 1));
        // Remote matching still works and ignores room clusters.
        assert_eq!(s.assign(&axis(0), meta(3.0)), Some(remote));
    }

    #[test]
    fn local_primary_is_excluded_from_identity_centroids() {
        let mut s = SessionClusterState::new(0.4);
        s.assign(&axis(0), meta(3.0));
        s.assign(&axis(1), with_hint(meta(3.0), room()));
        let ids: Vec<usize> = s.centroids().iter().map(|c| c.0).collect();
        assert_eq!(ids, vec![0]);
    }

    // ---- AHC ----

    #[test]
    fn average_linkage_separates_two_tight_groups() {
        let a1 = near(0, 5, 0.1);
        let a2 = near(0, 6, 0.1);
        let a3 = near(0, 7, 0.1);
        let b1 = near(1, 5, 0.1);
        let b2 = near(1, 6, 0.1);
        let units: Vec<&[f32]> = vec![&a1, &b1, &a2, &b2, &a3];
        let groups = average_linkage(&units, &[1.0; 5], 0.5);
        assert_eq!(groups, vec![0, 1, 0, 1, 0]);
    }

    #[test]
    fn average_linkage_threshold_extremes() {
        let vs: Vec<Vec<f32>> = (0..4).map(axis).collect();
        let units: Vec<&[f32]> = vs.iter().map(|v| v.as_slice()).collect();
        assert_eq!(average_linkage(&units, &[1.0; 4], 0.5), vec![0, 1, 2, 3]);
        assert_eq!(average_linkage(&units, &[1.0; 4], 1.5), vec![0, 0, 0, 0]);
        assert!(average_linkage(&[], &[], 0.5).is_empty());
    }

    #[test]
    fn average_linkage_matches_naive_greedy_on_random_data() {
        // Cross-check the NN-chain implementation against the textbook
        // "merge the closest pair until it exceeds threshold" loop.
        let mut seed = 0x1234_5678_u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f32 / 10_000.0 - 0.5
        };
        for trial in 0..20 {
            let n = 12 + trial;
            let vs: Vec<Vec<f32>> = (0..n)
                .map(|i| {
                    let mut v: Vec<f32> = (0..DIM).map(|_| rnd() * 0.6).collect();
                    v[i % 3] += 1.0; // three loose families
                    normalised(&v)
                })
                .collect();
            let ws: Vec<f32> = (0..n).map(|i| 1.0 + (i % 4) as f32).collect();
            let units: Vec<&[f32]> = vs.iter().map(|v| v.as_slice()).collect();
            for thr in [0.3_f32, 0.6, 0.9] {
                let fast = average_linkage(&units, &ws, thr);
                let slow = naive_average_linkage(&units, &ws, thr);
                assert_eq!(
                    canonical(&fast),
                    canonical(&slow),
                    "trial {trial} thr {thr}"
                );
            }
        }
    }

    fn canonical(groups: &[usize]) -> Vec<usize> {
        let mut map = std::collections::HashMap::new();
        groups
            .iter()
            .map(|g| {
                let next = map.len();
                *map.entry(*g).or_insert(next)
            })
            .collect()
    }

    fn naive_average_linkage(units: &[&[f32]], w: &[f32], thr: f32) -> Vec<usize> {
        let mut clusters: Vec<Vec<usize>> = (0..units.len()).map(|i| vec![i]).collect();
        loop {
            let mut best: Option<(usize, usize, f32)> = None;
            for i in 0..clusters.len() {
                for j in (i + 1)..clusters.len() {
                    let mut num = 0.0;
                    let mut den = 0.0;
                    for &a in &clusters[i] {
                        for &b in &clusters[j] {
                            num += w[a] * w[b] * cosine_distance(units[a], units[b]);
                            den += w[a] * w[b];
                        }
                    }
                    let d = num / den;
                    if best.map_or(true, |(_, _, bd)| d < bd) {
                        best = Some((i, j, d));
                    }
                }
            }
            match best {
                Some((i, j, d)) if d <= thr => {
                    let moved = clusters.remove(j);
                    clusters[i].extend(moved);
                }
                _ => break,
            }
        }
        let mut out = vec![0; units.len()];
        for (g, c) in clusters.iter().enumerate() {
            for &i in c {
                out[i] = g;
            }
        }
        out
    }

    // ---- re-cluster (#1013 item 5) ----

    #[test]
    fn recluster_splits_a_chained_cluster_and_keeps_live_names() {
        // Live threshold so loose that two speakers chain into one.
        let mut s = SessionClusterState::new(1.2);
        let mut t = 0;
        for i in 0..6 {
            let v = if i % 2 == 0 {
                near(0, 5 + i, 0.15)
            } else {
                near(1, 5 + i, 0.15)
            };
            s.assign(&v, meta_at(3.0, t));
            t += 4_000;
        }
        assert_eq!(s.cluster_count(), 1, "precondition: live matcher chained");
        let relabels = s.recluster(0.5);
        assert_eq!(s.cluster_count(), 2);
        // The bigger/first group keeps "Speaker 1"; the split-off group
        // is a new speaker; only its rows are relabelled.
        assert_eq!(relabels.len(), 3, "{relabels:?}");
        for r in &relabels {
            assert_eq!(r.old_label, "Speaker 1");
            assert_eq!(r.new_label, "Speaker 2");
        }
        let starts: Vec<u64> = relabels.iter().map(|r| r.started_at_ms).collect();
        assert_eq!(starts, vec![4_000, 12_000, 20_000]);
    }

    #[test]
    fn recluster_merges_an_over_split_speaker() {
        // Tight live threshold over-segments one voice.
        let mut s = SessionClusterState::new(0.05);
        for i in 0..4 {
            s.assign(&near(0, 5 + i, 0.3), meta_at(3.0, i as u64 * 4_000));
        }
        assert!(s.cluster_count() > 1);
        let relabels = s.recluster(0.5);
        assert_eq!(s.cluster_count(), 1);
        assert!(relabels.iter().all(|r| r.new_label == "Speaker 1"));
        assert_eq!(s.centroids().len(), 1);
    }

    #[test]
    fn recluster_folds_tiny_clusters_and_labels_abstained_chunks() {
        let mut s = SessionClusterState::new(0.4);
        // A 1.2 s first chunk abstains (can't found a speaker).
        assert_eq!(s.assign(&near(0, 9, 0.1), meta_at(1.2, 0)), None);
        for i in 0..4 {
            s.assign(&near(0, 5 + i, 0.1), meta_at(3.0, 2_000 + i as u64 * 4_000));
        }
        // One stray 1.6 s chunk founds a bogus speaker.
        let stray = s.assign(&axis(3), meta_at(1.6, 30_000)).unwrap();
        assert_eq!(stray, 1);
        let relabels = s.recluster(0.5);
        assert_eq!(s.cluster_count(), 1, "tiny cluster folded away");
        let by_start: std::collections::HashMap<u64, &Relabel> =
            relabels.iter().map(|r| (r.started_at_ms, r)).collect();
        assert_eq!(by_start[&0].old_label, "system");
        assert_eq!(by_start[&0].new_label, "Speaker 1");
        assert_eq!(by_start[&30_000].old_label, "Speaker 2");
        assert_eq!(by_start[&30_000].new_label, "Speaker 1");
    }

    #[test]
    fn recluster_is_idempotent() {
        let mut s = SessionClusterState::new(0.4);
        for i in 0..8 {
            let v = if i % 2 == 0 { axis(0) } else { axis(1) };
            s.assign(&v, meta_at(3.0, i * 4_000));
        }
        let _ = s.recluster(0.5);
        assert!(s.recluster(0.5).is_empty());
    }

    #[test]
    fn recluster_respects_namespaces_and_keeps_you() {
        let mut s = SessionClusterState::new(0.4);
        for i in 0..4 {
            s.assign(&axis(0), meta_at(3.0, i * 8_000));
            s.assign(&axis(0), with_hint(meta_at(3.0, i * 8_000 + 4_000), room()));
        }
        let relabels = s.recluster(0.5);
        assert!(relabels.is_empty(), "{relabels:?}");
        assert_eq!(s.cluster_count(), 2, "cannot-link survives the re-cluster");
        let labels: Vec<String> = s.clusters.iter().map(|c| s.label(c.id)).collect();
        assert!(labels.contains(&"mic".to_owned()));
        assert!(labels.contains(&"Speaker 1".to_owned()));
    }

    #[test]
    fn recluster_with_too_little_data_changes_nothing() {
        let mut s = SessionClusterState::new(0.4);
        s.assign(&axis(0), meta(3.0));
        s.assign(&axis(1), meta(0.5));
        assert!(s.recluster(0.5).is_empty());
        assert_eq!(s.cluster_count(), 1);
    }
}
