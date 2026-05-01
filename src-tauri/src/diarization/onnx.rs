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
use std::path::Path;
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
        let assigned_id = match best {
            Some((id, d)) if d <= self.distance_threshold => id,
            _ => {
                let id = self.next_id;
                self.next_id += 1;
                id
            }
        };
        self.history.push((embedding, assigned_id));
        assigned_id
    }
}

/// Production diarizer: ONNX speaker-embedding model + online
/// 1-NN-with-threshold clustering. See module-level doc.
pub struct OnnxDiarizer {
    /// `ort` session loaded from the user's downloaded model file.
    /// Wrapped in a `Mutex` because `Session::run` takes `&mut self`
    /// — internal state (allocator, run options) is mutable across
    /// calls. The lock is held only for the duration of one
    /// inference call (~50–100 ms on CPU), and the meeting pump is
    /// the sole caller, so contention isn't a concern in practice.
    /// We recover from poison by extracting the inner Session
    /// (`PoisonError::into_inner`) — a transient panic in one
    /// inference call shouldn't kill diarization for the rest of
    /// the meeting.
    session: Mutex<Session>,
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
    clusters: Mutex<SessionClusterState>,
    /// Name of the input tensor declared by the wespeaker model.
    /// Cached at construction so `run` doesn't re-allocate the
    /// string on every utterance.
    input_name: String,
}

impl OnnxDiarizer {
    /// Load the wespeaker model from `model_path` and ready it for
    /// inference. Fails if the file doesn't exist, isn't a valid
    /// ONNX model, or has an input shape we don't recognise.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        let model_path = model_path.as_ref();

        // Defence-in-depth: verify the file's SHA-256 against the
        // catalog before handing it to ort (#111 audit). The
        // download path verifies SHA at fetch time, but a sibling
        // app sharing the user's macOS account can write into the
        // models dir afterwards — re-checking at load means a
        // substituted ONNX (potentially crafted to exploit ort's
        // parser) is rejected before parsing. ~80 ms one-time cost
        // per app boot for the 26 MB wespeaker model.
        verify_model_sha256(model_path)
            .with_context(|| format!("verify SHA-256 of model at {}", model_path.display()))?;

        let mut builder = Session::builder().context("ort: build session")?;
        let session = builder
            .commit_from_file(model_path)
            .with_context(|| format!("ort: load model from {}", model_path.display()))?;

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

        Ok(Self {
            session: Mutex::new(session),
            mel: MelExtractor::new(),
            clusters: Mutex::new(SessionClusterState::new(DEFAULT_DISTANCE_THRESHOLD)),
            input_name,
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

        // The lock is held for the duration of one inference call
        // (~50–100 ms on CPU). The meeting pump is the sole caller
        // so there's no real contention surface. Recover from
        // poison via `into_inner` so a transient panic in one call
        // doesn't kill diarization for the rest of the session;
        // ort's run path is C++ behind a Result-returning shim, so
        // poisoning is unlikely in practice but cheap to defend
        // against.
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(view.to_vec())
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
}
