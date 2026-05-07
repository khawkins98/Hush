//! ONNX-backed speaker-embedding diarizer (#111).
//!
//! Runs the wespeaker ResNet34-LM model over each utterance's audio
//! to produce a 256-dimensional speaker embedding, then assigns
//! each new embedding to a session-stable cluster ID via
//! [`SessionClusterState`]'s online 1-NN-with-threshold matcher.
//! Final output: each utterance gets a `"Speaker N"` label where
//! utterances from the same speaker share the same `N`, assigned
//! in first-appearance order. Cluster IDs are stable across pump
//! ticks for the lifetime of the diarizer — the speaker that gets
//! "Speaker 1" early in a meeting keeps it for the whole meeting.
//!
//! Pre-#303 the diarizer ran per-tick agglomerative clustering
//! (`super::cluster::cluster_with_threshold`, removed in #310);
//! cluster IDs reset on every pump tick. The streaming
//! session-state matcher fixes that — a speaker who gets "Speaker
//! 1" early in the meeting keeps it for the whole meeting.
//!
//! ## Pipeline
//!
//! For each utterance, resample to 16 kHz mono (reuses
//! [`crate::transcription::resample::resample_to_mono`] +
//! `crate::audio::downmix_to_mono` from the Whisper preprocessing
//! path), compute 80-dim Mel-FB features via
//! [`super::features::MelExtractor`], feed `(1, num_frames, 80)` to
//! the ONNX session, and read the `(1, 256)` embedding back. Hand
//! the embedding to `SessionClusterState::assign` to get a stable
//! cluster ID, and stamp the utterance `"Speaker {N+1}"`. Cluster
//! state persists for the lifetime of the diarizer, so cluster
//! IDs are stable across pump ticks.
//!
//! ## Threading model
//!
//! `Session` (the ort handle) is `Send + Sync` and supports
//! concurrent `run` calls. We hold one inside the diarizer for the
//! whole app lifetime; the meeting pump's `tokio::spawn_blocking`
//! task calls into us per chunk. No locks required.
//!
//! ## Cost
//!
//! Per-utterance inference on the wespeaker ResNet34-LM model is
//! ~50–100 ms on CPU for a 1–10 s audio clip (the embedding is
//! extracted once per utterance, regardless of length, because the
//! model mean-pools internally over time frames). On Apple Silicon
//! with the CoreML execution provider it's roughly 3× faster — but
//! we currently use the default CPU provider; CoreML wiring is
//! tracked separately.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use ndarray::Array3;
use ort::session::Session;
use ort::value::Value;
use sha2::{Digest, Sha256};

use crate::audio::CaptureFormat;
use crate::diarization::catalog::default_diarizer_model;
use crate::diarization::cluster::{cosine_distance, DEFAULT_DISTANCE_THRESHOLD};
use crate::diarization::features::{MelExtractor, NUM_MEL_BINS, SAMPLE_RATE_HZ};
use crate::diarization::Diarize;
use crate::transcription::Utterance;

/// Embedding dimensionality — wespeaker ResNet34-LM emits 256-dim
/// vectors. Used for shape-checking the model output.
pub const EMBEDDING_DIM: usize = 256;

/// Number of successful `embed` calls before the ORT `Session` is
/// dropped and lazily recreated (#641).
///
/// **Why recreate the session at all:** on Apple Silicon, ORT's
/// prebuilt `download-binaries` binaries link against Metal
/// Performance Shaders and dispatch matmul / layernorm / softmax
/// kernels through MPS even when only the CPU execution provider is
/// registered. Each `session.run` allocates Metal command buffers and
/// texture-backed `IOAccelerator` regions that are pinned to the
/// `Session` object's lifetime — they are NOT freed at the end of
/// `run`. Over a 5-min meeting post-#639 `vmmap` showed 96 such
/// regions totalling 9 GB virtual / 7.8 GB in swap. The growth rate
/// was ~1.25 GB/min with diarization enabled and flatlined
/// immediately when diarization was toggled off mid-meeting
/// (confirmed by Ken, #612 diagnostic).
///
/// **Why the Session only, not SessionClusterState:** the cluster
/// state (`SessionClusterState`) is purely Rust — `Vec<(Vec<f32>,
/// usize)>`. It has no ORT or Metal associations. Dropping only the
/// `Session` forces Metal command buffers to retire and IOAccelerator
/// regions to be released while keeping speaker-label continuity
/// across recreations. "Speaker 1" at minute 3 stays "Speaker 1" at
/// minute 8.
///
/// **Why 25:** at a rough ~10 utterances/min cadence this means one
/// recreation every ~2.5 min. Each `IOAccelerator` batch at 1.25
/// GB/min × 2.5 min ≈ 3 GB peak before each flush — acceptable and
/// far below the OOM threshold on any supported Mac. Empirical tuning
/// may revise this (lower = more flushing, less GPU pressure; higher
/// = fewer pauses but more peak). Zero means "never recreate" (A/B
/// knob for regression testing).
///
/// Tunable via `HUSH_DIARIZER_SESSION_RECREATE_INTERVAL` env var at
/// app launch without rebuilding.
pub const DEFAULT_SESSION_RECREATE_INTERVAL: u64 = 25;

/// Resolves [`DEFAULT_SESSION_RECREATE_INTERVAL`] against an
/// optional `HUSH_DIARIZER_SESSION_RECREATE_INTERVAL` env-var
/// override read once at process start. Returns 0 to mean "never
/// recreate" (A/B knob; keeps the old unbounded behaviour).
fn session_recreate_interval() -> u64 {
    std::env::var("HUSH_DIARIZER_SESSION_RECREATE_INTERVAL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SESSION_RECREATE_INTERVAL)
}

/// Minimum number of Mel-FB frames an utterance needs to produce a
/// useful embedding. Below this, the model can technically still
/// run, but the resulting vector is dominated by noise — better to
/// skip the inference and let the source-derived label stand.
///
/// 25 frames @ 10 ms hop = 250 ms of audio — ~one syllable. Shorter
/// than that and we'd be embedding silence + breath.
const MIN_FRAMES_FOR_EMBEDDING: usize = 25;

/// Per-utterance memory keeping cluster IDs stable across meeting
/// pump ticks. Pre-PR-G the diarizer ran agglomerative clustering
/// over each tick's batch in isolation, so "Speaker 1" in tick N
/// could be a different person from "Speaker 1" in tick N+1 — a
/// correctness bug that surfaces immediately on real meetings.
///
/// Algorithm: 1-NN with threshold. For each new embedding, find the
/// closest previously-seen embedding (cosine distance). If within
/// `DEFAULT_DISTANCE_THRESHOLD`, reuse that embedding's cluster
/// ID; otherwise allocate the next cluster ID. Single-link in
/// spirit, but bounded by an absolute distance threshold so it
/// doesn't chain like classical single-link agglomerative.
///
/// Memory: every embedding (256 f32 = 1 KB) lives until the
/// session ends. A 100-utterance meeting holds ~100 KB of state —
/// negligible.
struct SessionClusterState {
    /// Per-utterance `(embedding, cluster_id)`. Append-only over
    /// the session.
    history: Vec<(Vec<f32>, usize)>,
    /// Next cluster ID to allocate when no existing centroid is
    /// within threshold. Equal to `unique cluster count` — IDs are
    /// dense and assigned in first-appearance order so labels read
    /// "Speaker 1, 2, …" the way the user expects.
    next_id: usize,
    /// Maximum cosine distance at which two embeddings are still
    /// considered the same speaker.
    distance_threshold: f32,
}

impl SessionClusterState {
    fn new(distance_threshold: f32) -> Self {
        Self {
            history: Vec::new(),
            next_id: 0,
            distance_threshold,
        }
    }

    /// Assign a cluster ID to `embedding`. Appends to history;
    /// returns the assigned ID (0-indexed).
    ///
    /// Known limitation (1-NN single-link chaining): nearest-
    /// neighbour matching against the full session history can
    /// chain a slowly-drifting voice (microphone position change,
    /// vocal fatigue) into an adjacent speaker's cluster — each
    /// new utterance latches onto the most-recent neighbour, and
    /// after enough small drifts the chain crosses the threshold
    /// into a different cluster while still being labeled the
    /// original one. Acceptable for v1: the pre-PR-G alternative
    /// (per-tick agglomerative re-clustering) was demonstrably
    /// worse because cluster IDs themselves were unstable across
    /// ticks. A future iteration could match against per-cluster
    /// centroids (medoids) instead of all past embeddings, or
    /// re-run a global agglomerative pass periodically. Leaving
    /// the chain risk documented here so a future contributor
    /// re-deriving the design choice doesn't have to from scratch.
    fn assign(&mut self, embedding: Vec<f32>) -> usize {
        let mut best: Option<(usize, f32)> = None;
        for (e, id) in &self.history {
            let d = cosine_distance(embedding.as_slice(), e.as_slice());
            match best {
                None => best = Some((*id, d)),
                Some((_, current)) if d < current => best = Some((*id, d)),
                _ => {}
            }
        }
        let (assigned_id, was_new, best_distance) = match best {
            Some((id, d)) if d <= self.distance_threshold => (id, false, Some(d)),
            Some((_, d)) => {
                // A nearest neighbour exists but it's beyond the
                // threshold — record the distance so a debug session
                // can see how close we came (helps tune the
                // threshold via HUSH_DIARIZER_THRESHOLD).
                let id = self.next_id;
                self.next_id += 1;
                (id, true, Some(d))
            }
            None => {
                let id = self.next_id;
                self.next_id += 1;
                (id, true, None)
            }
        };
        // INFO-level so it lands in the on-disk log file by default
        // (#316 diagnostic). Cheap — fires at most once per utterance,
        // which is the same cadence the diarizer already runs at.
        // Reads "Speaker N (new cluster)" or "Speaker N (matched, distance=0.42)".
        match best_distance {
            Some(d) if was_new => tracing::info!(
                speaker = assigned_id + 1,
                best_distance = d,
                threshold = self.distance_threshold,
                history_len = self.history.len(),
                "diarizer: NEW cluster (best match was beyond threshold)"
            ),
            Some(d) => tracing::info!(
                speaker = assigned_id + 1,
                distance = d,
                threshold = self.distance_threshold,
                "diarizer: matched existing cluster"
            ),
            None => tracing::info!(
                speaker = assigned_id + 1,
                "diarizer: NEW cluster (first utterance)"
            ),
        }
        self.history.push((embedding, assigned_id));
        assigned_id
    }
}

/// Resolve the diarizer's cosine-distance threshold, honouring an
/// optional `HUSH_DIARIZER_THRESHOLD` env-var override (#316). Falls
/// back to [`DEFAULT_DISTANCE_THRESHOLD`] when the var is unset or
/// can't be parsed as an `f32` in the valid range `[0.0, 2.0]`.
///
/// Read once at `OnnxDiarizer::new` so a mid-session env-var toggle
/// can't change behaviour partway through. Exposed as a tuning knob
/// for users hitting the multi-speaker chaining issue documented in
/// #316; lower values create more clusters (e.g. `0.3` for very
/// aggressive splitting on short-utterance calls), higher values
/// merge speakers (e.g. `0.6` to revert to the pre-#316 default).
fn resolve_distance_threshold() -> f32 {
    match std::env::var("HUSH_DIARIZER_THRESHOLD") {
        Ok(raw) => match raw.parse::<f32>() {
            Ok(v) if (0.0..=2.0).contains(&v) => v,
            Ok(v) => {
                tracing::warn!(
                    raw = %raw,
                    value = v,
                    default = DEFAULT_DISTANCE_THRESHOLD,
                    "HUSH_DIARIZER_THRESHOLD out of [0.0, 2.0]; falling back to default"
                );
                DEFAULT_DISTANCE_THRESHOLD
            }
            Err(_) => {
                tracing::warn!(
                    raw = %raw,
                    default = DEFAULT_DISTANCE_THRESHOLD,
                    "HUSH_DIARIZER_THRESHOLD not parseable as f32; falling back to default"
                );
                DEFAULT_DISTANCE_THRESHOLD
            }
        },
        Err(_) => DEFAULT_DISTANCE_THRESHOLD,
    }
}

/// Production diarizer: ONNX speaker-embedding model + online
/// 1-NN-with-threshold clustering. See module-level doc.
pub struct OnnxDiarizer {
    /// Path to the loaded model file. Retained so the session can be
    /// recreated periodically to flush Metal/MPS IOAccelerator
    /// regions (#641).
    model_path: PathBuf,
    /// ORT session. `None` signals "needs lazy recreation" — same
    /// pattern as `WhisperState` in the streaming transcriber (#612).
    /// Holding an `Option` lets us `take()` the session at the
    /// recreation boundary without touching the cluster state.
    session: Mutex<Option<Session>>,
    /// Reusable Mel-FB extractor — holds the planned 512-pt FFT,
    /// Povey window, and 80-bin filterbank. Constructed once per
    /// `OnnxDiarizer`.
    mel: MelExtractor,
    /// Persistent cluster state across pump ticks. See
    /// [`SessionClusterState`] for the algorithm. `Mutex` because
    /// `Diarize::label_utterances` takes `&self` but the cluster
    /// state mutates on each call. Lock is held for the duration
    /// of a single batch's labelling — sub-millisecond at typical
    /// batch sizes.
    ///
    /// Deliberately separate from `session`: this state is preserved
    /// across periodic ORT session recreations so speaker-label
    /// continuity is maintained for the full meeting (#641).
    clusters: Mutex<SessionClusterState>,
    /// Name of the input tensor declared by the wespeaker model.
    /// Cached at construction so `run` doesn't re-allocate the
    /// string on every utterance.
    input_name: String,
    /// Successful `embed` calls on the current ORT session. When
    /// this reaches `session_recreate_interval`, the session is
    /// dropped (set to `None`) so the next `embed` lazily creates a
    /// fresh one, flushing accumulated Metal command buffers (#641).
    embeds_on_current_session: AtomicU64,
    /// Cached value from [`session_recreate_interval`]. Zero means
    /// "never recreate" (A/B disable knob).
    session_recreate_interval: u64,
}

impl OnnxDiarizer {
    /// Load the wespeaker model from `model_path` and ready it for
    /// inference. Fails if the file doesn't exist, isn't a valid
    /// ONNX model, or has an input shape we don't recognise.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref().to_path_buf();
        let session = build_ort_session(&model_path)?;

        // The wespeaker model exposes a single named input. Cache
        // the name rather than hard-coding "feats" — different
        // ONNX exports of the same architecture sometimes use
        // different input names ("feats" vs "input" vs "fbank"),
        // and reading it from the model removes that footgun.
        let input_name = session
            .inputs()
            .first()
            .ok_or_else(|| anyhow!("ort: model has no inputs (expected exactly one)"))?
            .name()
            .to_owned();

        let threshold = resolve_distance_threshold();
        if (threshold - DEFAULT_DISTANCE_THRESHOLD).abs() > f32::EPSILON {
            tracing::info!(
                threshold,
                default_threshold = DEFAULT_DISTANCE_THRESHOLD,
                "OnnxDiarizer: using HUSH_DIARIZER_THRESHOLD override"
            );
        }
        Ok(Self {
            model_path,
            session: Mutex::new(Some(session)),
            mel: MelExtractor::new(),
            clusters: Mutex::new(SessionClusterState::new(threshold)),
            input_name,
            embeds_on_current_session: AtomicU64::new(0),
            session_recreate_interval: session_recreate_interval(),
        })
    }

    /// Run the embedding model on a single chunk of 16 kHz mono PCM.
    /// Returns the 256-d embedding as a `Vec<f32>`.
    ///
    /// Fails if the audio is shorter than [`MIN_FRAMES_FOR_EMBEDDING`]
    /// frames after Mel-FB extraction (caller falls back to the
    /// source-derived label), or if the ONNX session reports an
    /// inference error.
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>> {
        let mel = self.mel.extract(samples);
        let num_frames = mel.len() / NUM_MEL_BINS;
        if num_frames < MIN_FRAMES_FOR_EMBEDDING {
            return Err(anyhow!(
                "audio too short for embedding ({num_frames} frames < {MIN_FRAMES_FOR_EMBEDDING})"
            ));
        }

        // Reshape the flat row-major (num_frames, 80) buffer into a
        // 3-D ndarray with a unit batch dimension. The model wants
        // (batch, num_frames, num_mels). `from_shape_vec` is O(0) —
        // no copy, just a view over the existing allocation.
        let input: Array3<f32> = Array3::from_shape_vec((1, num_frames, NUM_MEL_BINS), mel)
            .context("ndarray: reshape Mel features into (1, frames, 80)")?;

        let input_value = Value::from_array(input).context("ort: wrap ndarray as a Value")?;

        // Acquire the session lock. `None` here means the previous
        // embed dropped the session for recreation (see below) — lazy-
        // init a fresh one now. The session mutex is held for the full
        // inference call (~50–100 ms on CPU); the meeting pump is the
        // sole caller so contention isn't a concern in practice. Recover
        // from poison via `into_inner` so a transient panic in one call
        // doesn't kill diarization for the rest of the meeting.
        let mut session_guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if session_guard.is_none() {
            let rebuilt = build_ort_session(&self.model_path)
                .context("OnnxDiarizer: rebuild ORT session (#641)")?;
            *session_guard = Some(rebuilt);
            tracing::info!(
                "OnnxDiarizer: ORT session recreated to flush Metal/MPS IOAccelerator regions (#641)"
            );
        }

        // Scoped block so the `session` borrow of `session_guard` ends
        // before we potentially set `session_guard = None` below. The
        // `outputs` extraction (including the `.to_vec()` copy) must
        // complete while the session is still alive.
        let embedding: Vec<f32> = {
            let session = session_guard.as_mut().unwrap();
            // Reverted from #631's `run_with_options` + per-run arena
            // shrinkage. The shrinkage released the output tensor's
            // backing memory before `try_extract_tensor` could read
            // it, causing `embed()` to silently fail on every
            // utterance — which made the leak APPEAR fixed (no
            // successful inferences = no allocations) while actually
            // breaking diarization (every utterance fell back to the
            // source-derived "mic"/"system" label). #630's build-time
            // `with_arena_allocator(false)` + `with_memory_pattern(false)`
            // are the fix we ship; the per-run shrinkage is too
            // dangerous given how ORT's allocator interacts with
            // output tensor lifetime.
            let outputs = session
                .run(ort::inputs![self.input_name.as_str() => input_value])
                .context("ort: session.run")?;

            // Single output (the embedding). `try_extract_array` gives a
            // typed view we can copy out of without unsafe.
            let (_, view): (_, &[f32]) = outputs[0]
                .try_extract_tensor::<f32>()
                .context("ort: extract f32 output tensor")?;

            if view.len() != EMBEDDING_DIM {
                return Err(anyhow!(
                    "ort: unexpected embedding length {} (expected {EMBEDDING_DIM})",
                    view.len()
                ));
            }
            view.to_vec()
        }; // outputs drops here; borrow on session_guard ends

        // Periodic session recreation (#641): after every
        // `session_recreate_interval` successful embeds, drop the ORT
        // session so the next call lazy-recreates a fresh one. This
        // forces Metal/MPS command buffers to retire and IOAccelerator
        // regions to be released. The cluster state is separate and
        // unaffected — speaker-label continuity is preserved.
        //
        // Counter uses Relaxed ordering: we only need approximate
        // threshold detection; no other state is synchronised on this
        // counter. Off-by-one across threads is impossible here (sole
        // caller is the meeting-pump blocking task) and inconsequential
        // even if it weren't.
        let count = self
            .embeds_on_current_session
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        if self.session_recreate_interval > 0 && count >= self.session_recreate_interval {
            *session_guard = None; // drop Session; cluster state untouched
            self.embeds_on_current_session.store(0, Ordering::Relaxed);
            tracing::info!(
                embeds = count,
                interval = self.session_recreate_interval,
                "OnnxDiarizer: ORT session dropped; next embed will recreate (#641)"
            );
        }

        Ok(embedding)
    }
}

impl Diarize for OnnxDiarizer {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        format: CaptureFormat,
    ) {
        if utterances.is_empty() {
            return;
        }
        // Without per-utterance audio there's no signal to embed;
        // fall through to leave the source-derived stamp in place.
        if audio_chunks.len() != utterances.len() {
            tracing::warn!(
                utterances = utterances.len(),
                chunks = audio_chunks.len(),
                "OnnxDiarizer: audio_chunks slice length mismatches utterances; skipping"
            );
            return;
        }

        // Embed each utterance and assign a session-stable cluster
        // ID. Stable across pump ticks: a speaker that gets ID 0
        // in tick 0 keeps ID 0 throughout the meeting because
        // `SessionClusterState::assign` matches against the full
        // session history, not just the current batch.
        //
        // Errors on `embed` (audio too short, ort failure) leave
        // the utterance unlabelled — the dispatch path then falls
        // through to the source-derived "mic" / "system" stamp.
        let mut session_clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        for (i, chunk) in audio_chunks.iter().enumerate() {
            let resampled = prepare_audio_for_embedding(chunk, format);
            match self.embed(&resampled) {
                Ok(emb) => {
                    let cluster_id = session_clusters.assign(emb);
                    // 1-indexed for human display; "Speaker 1, 2, …".
                    utterances[i].speaker_label = Some(format!("Speaker {}", cluster_id + 1));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "OnnxDiarizer: skip utterance");
                }
            }
        }
    }
}

/// Build an ORT [`Session`] from `model_path` with the standard
/// options used by every `OnnxDiarizer` instance (arena disabled,
/// memory-pattern cache disabled). Shared by [`OnnxDiarizer::new`]
/// and the periodic session-recreation path (#641) so the options
/// stay in sync.
///
/// Verifies the file's SHA-256 against the catalog before handing
/// it to ort on every call — both at initial load and on recreation
/// (#111 defence-in-depth: a sibling app could replace the model
/// file between constructions).
///
/// `with_execution_providers` and `with_memory_pattern` return
/// `Result<SessionBuilder, ort::Error<SessionBuilder>>` where the
/// error variant carries the partially-built session back to the
/// caller. anyhow's `.context()` doesn't apply because that error
/// type isn't `StdError` the way anyhow expects, so we convert via
/// `.map_err(|e| e.into_inner())` — `into_inner` returns the
/// underlying `ort::Error` which IS `StdError`.
fn build_ort_session(model_path: &Path) -> Result<Session> {
    // Defence-in-depth: verify SHA-256 before each (re)load so a
    // substituted model file is caught even if it was written after
    // the first boot-time check. ~80 ms cost per call at 26 MB.
    verify_model_sha256(model_path)
        .with_context(|| format!("verify SHA-256 of model at {}", model_path.display()))?;

    // Disable the CPU EP's arena allocator and ORT's memory-pattern
    // cache (#612). The wespeaker model takes variable-length log-Mel
    // features per utterance — the textbook trigger for ORT's
    // dynamic-shape arena growth (microsoft/onnxruntime#11627, #22271).
    // Arena disabled: allocations route through plain malloc/free and
    // return to the OS at end of `run`. Memory-pattern disabled: no
    // per-shape plan cache accumulating. 2–10 % latency hit per `run`,
    // invisible at our once-per-utterance cadence.
    let mut builder = Session::builder()
        .context("ort: build session")?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|e| anyhow!("ort: register CPU EP with arena disabled (#612): {}", e))?
        .with_memory_pattern(false)
        .map_err(|e| anyhow!("ort: disable memory pattern cache (#612): {}", e))?;
    builder
        .commit_from_file(model_path)
        .with_context(|| format!("ort: load model from {}", model_path.display()))
}

/// Hash the file at `path` and reject if its SHA-256 doesn't match
/// the catalog's expected value. Streams the file in 64 KB chunks
/// so we don't allocate a 26 MB scratch buffer.
fn verify_model_sha256(path: &Path) -> Result<()> {
    use std::io::Read;
    let expected = default_diarizer_model().sha256;
    let mut file =
        fs::File::open(path).with_context(|| format!("open model file at {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).context("read model file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = format!("{:x}", hasher.finalize());
    if got != expected {
        return Err(anyhow!(
            "model SHA-256 mismatch (got {got}, expected {expected}) — refusing to load"
        ));
    }
    Ok(())
}

/// Convert a captured chunk into the `f32 16 kHz mono` shape the
/// embedding model expects. Re-uses the existing transcription-
/// pipeline helpers so a fix to the resampler benefits both paths.
fn prepare_audio_for_embedding(chunk: &[f32], format: CaptureFormat) -> Vec<f32> {
    let mono = if format.channels > 1 {
        crate::audio::downmix_to_mono(chunk, format.channels)
    } else {
        chunk.to_vec()
    };
    if format.sample_rate == SAMPLE_RATE_HZ {
        mono
    } else {
        crate::transcription::resample::resample_to_mono(&mono, format.sample_rate, SAMPLE_RATE_HZ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an embedding pointing along a single axis. Shared
    /// helper across the SessionClusterState tests; keeps the
    /// test data trivial and the assertions focused on the
    /// cluster-assignment logic, not on numerical noise.
    fn axis(idx: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; EMBEDDING_DIM];
        v[idx] = 1.0;
        v
    }

    #[test]
    fn session_cluster_state_assigns_first_embedding_id_zero() {
        let mut s = SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD);
        assert_eq!(s.assign(axis(0)), 0);
    }

    #[test]
    fn session_cluster_state_groups_similar_embeddings() {
        let mut s = SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD);
        // Two identical embeddings → same cluster.
        let id_a = s.assign(axis(0));
        let id_b = s.assign(axis(0));
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn session_cluster_state_separates_distinct_embeddings() {
        let mut s = SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD);
        // Orthogonal embeddings (cosine distance 1.0) exceed the
        // 0.6 default threshold → distinct clusters.
        let id_a = s.assign(axis(0));
        let id_b = s.assign(axis(1));
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn session_cluster_state_cluster_ids_are_stable_across_ticks() {
        // The whole point of moving from per-tick agglomerative to
        // session-state matching: the speaker who got ID 0 on the
        // first call must still get ID 0 on the third, even after
        // a different speaker has appeared in between.
        //
        // Pre-PR-G this test would have failed because each tick's
        // `cluster_with_threshold` call started fresh.
        let mut s = SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD);
        let alice = axis(0);
        let bob = axis(1);
        // Tick 1: Alice speaks → ID 0
        assert_eq!(s.assign(alice.clone()), 0);
        // Tick 2: Bob speaks → ID 1
        assert_eq!(s.assign(bob.clone()), 1);
        // Tick 3: Alice speaks again → must still be ID 0
        assert_eq!(s.assign(alice.clone()), 0);
        // Tick 4: Bob again → still ID 1
        assert_eq!(s.assign(bob), 1);
    }

    #[test]
    fn session_cluster_state_first_appearance_order() {
        // ID 0 goes to the first speaker, 1 to the second, etc. —
        // matches how end-users will read "Speaker 1, 2, 3" in
        // transcripts.
        let mut s = SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD);
        assert_eq!(s.assign(axis(2)), 0); // first speaker is ID 0 regardless of axis
        assert_eq!(s.assign(axis(0)), 1);
        assert_eq!(s.assign(axis(1)), 2);
        // Returning to first speaker reuses ID 0.
        assert_eq!(s.assign(axis(2)), 0);
    }

    #[test]
    fn prepare_audio_passthrough_when_already_16k_mono() {
        let chunk = vec![0.1_f32, 0.2, 0.3];
        let out = prepare_audio_for_embedding(
            &chunk,
            CaptureFormat {
                sample_rate: SAMPLE_RATE_HZ,
                channels: 1,
            },
        );
        assert_eq!(out, chunk);
    }

    #[test]
    fn prepare_audio_downmixes_stereo_to_mono() {
        // Interleaved L/R: [L0, R0, L1, R1] → mean pairs.
        let stereo = vec![1.0_f32, -1.0, 0.5, 0.5];
        let out = prepare_audio_for_embedding(
            &stereo,
            CaptureFormat {
                sample_rate: SAMPLE_RATE_HZ,
                channels: 2,
            },
        );
        // After downmix to mono: [(1.0 + -1.0)/2, (0.5 + 0.5)/2]
        assert_eq!(out.len(), 2);
        assert!((out[0] - 0.0).abs() < 1e-6, "got {}", out[0]);
        assert!((out[1] - 0.5).abs() < 1e-6, "got {}", out[1]);
    }

    #[test]
    fn prepare_audio_resamples_when_rate_mismatched() {
        // 48 kHz mono of length 96 → 16 kHz mono of length ~32.
        let in_chunk: Vec<f32> = (0..96).map(|i| i as f32 / 96.0).collect();
        let out = prepare_audio_for_embedding(
            &in_chunk,
            CaptureFormat {
                sample_rate: 48_000,
                channels: 1,
            },
        );
        // Linear resampler at 3:1 ratio drops to a third of the
        // input length, give or take an edge sample.
        assert!(
            (out.len() as i64 - 32).abs() <= 1,
            "expected ~32 samples, got {}",
            out.len()
        );
    }

    #[test]
    fn verify_model_sha256_rejects_unknown_file() {
        // Write a tiny scratch file whose SHA-256 cannot match
        // the catalog's expected wespeaker hash, then confirm
        // `OnnxDiarizer::new` refuses to load it before ort sees
        // it. This is the "sibling app substituted the model"
        // defence-in-depth path from the audit.
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-wespeaker.onnx");
        let mut f = fs::File::create(&path).expect("create temp file");
        f.write_all(b"definitely not a wespeaker model")
            .expect("write");
        drop(f);

        let err = match OnnxDiarizer::new(&path) {
            Ok(_) => panic!("OnnxDiarizer::new accepted a wrong-SHA file"),
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("SHA-256 mismatch"),
            "expected SHA-256 mismatch error, got: {msg}"
        );
    }

    /// End-to-end test against a real wespeaker model file. Marked
    /// `#[ignore]` because CI doesn't have the model on disk —
    /// developers run it manually after `huggingface-cli download
    /// Wespeaker/wespeaker-voxceleb-resnet34-LM voxceleb_resnet34_LM.onnx`
    /// and exporting `HUSH_DIARIZATION_MODEL_PATH` to that file's
    /// path. PR-E's hands-on validation against a real meeting
    /// recording covers the broader correctness check; this test
    /// just verifies the inference path connects end-to-end.
    #[test]
    #[ignore]
    fn embed_runs_against_real_model() {
        let path = match std::env::var("HUSH_DIARIZATION_MODEL_PATH") {
            Ok(p) => p,
            Err(_) => {
                eprintln!("skipping: set HUSH_DIARIZATION_MODEL_PATH to a wespeaker .onnx file");
                return;
            }
        };
        let diarizer = OnnxDiarizer::new(&path).expect("load wespeaker model");

        // 1 s of 16 kHz silence → 98 frames → above the 25-frame
        // floor → embedding is computed (silence-input
        // embeddings are nonsense but the inference path runs).
        let samples = vec![0.0_f32; 16_000];
        let emb = diarizer.embed(&samples).expect("embed silence");
        assert_eq!(emb.len(), EMBEDDING_DIM);
    }

    #[test]
    fn session_recreate_interval_env_var_and_default() {
        // Override: set → assert custom value, then unset → assert default.
        // Single test avoids the env-var race that would occur if the
        // override and defaults cases ran concurrently in separate tests.
        std::env::set_var("HUSH_DIARIZER_SESSION_RECREATE_INTERVAL", "10");
        assert_eq!(session_recreate_interval(), 10);
        std::env::remove_var("HUSH_DIARIZER_SESSION_RECREATE_INTERVAL");
        assert_eq!(
            session_recreate_interval(),
            DEFAULT_SESSION_RECREATE_INTERVAL
        );
    }
}
