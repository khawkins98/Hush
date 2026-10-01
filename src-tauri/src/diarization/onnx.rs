//! tract-backed speaker-embedding diarizer (#111, ORT replaced in #641).
//!
//! Runs the wespeaker ResNet34-LM model over each utterance's audio
//! to produce a 256-dimensional speaker embedding, then assigns
//! each new embedding to a session-stable cluster ID via
//! [`SessionClusterState`]'s online matcher (`session.rs`), which
//! also runs the session-end re-cluster (#1013).
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
//! ## Why tract, not ORT
//!
//! The original implementation used `ort` (ONNX Runtime) with the
//! `download-binaries` feature. On Apple Silicon, ORT's prebuilt
//! binaries route matmul / layernorm / softmax through Metal
//! Performance Shaders even with `CPU::default()` EP. Each
//! `session.run` allocates `IOAccelerator` regions pinned to the
//! `Session` lifetime; they do NOT flush at end-of-run. Over a
//! 5-min meeting `vmmap` showed 96 such regions totalling 9 GB
//! virtual / 7.8 GB in swap (~1.25 GB/min growth). Periodic
//! session recreation (#642) bounded but did not eliminate the
//! growth. `tract-onnx` is pure Rust — zero Metal dispatch —
//! fixing the leak at the root (#641).
//!
//! ## Pipeline
//!
//! For each utterance, resample to 16 kHz mono (reuses
//! [`crate::transcription::resample::resample_to_mono`] +
//! `crate::audio::downmix_to_mono` from the Whisper preprocessing
//! path), compute 80-dim Mel-FB features via
//! [`super::features::MelExtractor`] (kaldi-compatible, matching
//! wespeaker's front end), apply per-utterance cepstral mean
//! normalisation ([`super::features::apply_cmn`] — the model was
//! trained on mean-normalised features; #1013), feed
//! `(1, num_frames, 80)` to the tract inference plan, and read the
//! `(1, 256)` embedding back. Hand the embedding plus the chunk's
//! duration and the pump's [`ChunkHint`] to
//! `SessionClusterState::assign` to get a stable cluster ID, and stamp
//! the utterance with its label (`"Speaker {N+1}"`).
//! Cluster state persists for the lifetime of the diarizer, so
//! cluster IDs are stable across pump ticks.
//!
//! ## Threading model
//!
//! `TypedRunnableModel<TypedModel>` (tract's `SimplePlan`) is
//! `Send + Sync`: `TypedOp` requires `Send + Sync` on all
//! implementations and `SimplePlan` holds only `Arc<TypedModel>`.
//! No additional lock is required for `self.model`. The
//! `self.clusters` mutex is held only for the cheap cluster-
//! assignment loop (sub-millisecond), NOT during inference.
//!
//! ## Cost
//!
//! Per-utterance inference on the wespeaker ResNet34-LM model is
//! ~50–100 ms on CPU for a 1–10 s audio clip (the embedding is
//! extracted once per utterance, regardless of length, because the
//! model mean-pools internally over time frames). tract is CPU-only;
//! without CoreML acceleration the latency matches ORT's CPU EP —
//! acceptable given inference runs at most once per utterance on the
//! meeting pump's blocking thread.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use tract_onnx::prelude::*;

use crate::audio::CaptureFormat;
use crate::diarization::catalog::default_diarizer_model;
use crate::diarization::cluster::DEFAULT_DISTANCE_THRESHOLD;
use crate::diarization::features::{apply_cmn, MelExtractor, NUM_MEL_BINS, SAMPLE_RATE_HZ};
use crate::diarization::session::{ChunkMeta, SessionClusterState};
use crate::diarization::{ChunkHint, Diarize, Relabel};
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

/// Resolve the diarizer's cosine-distance threshold, honouring an optional
/// `HUSH_DIARIZER_THRESHOLD` env-var override (#316). Falls back to
/// [`DEFAULT_DISTANCE_THRESHOLD`] when the var is unset or can't be parsed
/// as an `f32` in the valid range `[0.0, 2.0]`.
fn resolve_distance_threshold_from_env() -> f32 {
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

/// Production diarizer: tract-backed speaker-embedding model + online
/// clustering ([`SessionClusterState`]). See module-level doc.
pub struct OnnxDiarizer {
    /// Compiled and optimised tract inference plan for the wespeaker
    /// ResNet34-LM model. `SimplePlan` (the concrete type behind
    /// `TypedRunnableModel`) is `Send + Sync` because `TypedOp`
    /// requires `Send + Sync` on all implementations and the plan
    /// holds only `Arc<TypedModel>` — no lock needed.
    model: TypedRunnableModel<TypedModel>,
    /// Reusable Mel-FB extractor — holds the planned 512-pt FFT,
    /// Hamming window, and 80-bin filterbank. Constructed once per
    /// `OnnxDiarizer`.
    mel: MelExtractor,
    /// Persistent cluster state across pump ticks. `Mutex` because
    /// `Diarize::label_utterances` takes `&self` but the cluster
    /// state mutates on each call. Lock is held only for the cheap
    /// cluster-assignment loop (sub-millisecond), NOT during
    /// inference.
    clusters: Mutex<SessionClusterState>,
}

impl OnnxDiarizer {
    /// Load the wespeaker model from `model_path` and ready it for
    /// inference. Fails if the file doesn't exist, its SHA-256
    /// doesn't match the catalog, or tract cannot parse/optimize it.
    pub fn new(model_path: impl AsRef<Path>) -> Result<Self> {
        Self::new_with_threshold(model_path, None)
    }

    /// Same as [`Self::new`], but allows an explicit threshold override.
    /// `None` falls back to `HUSH_DIARIZER_THRESHOLD` / default.
    pub fn new_with_threshold(
        model_path: impl AsRef<Path>,
        threshold_override: Option<f32>,
    ) -> Result<Self> {
        let model = build_tract_model(model_path.as_ref())?;

        let threshold = threshold_override.unwrap_or_else(resolve_distance_threshold_from_env);
        if (threshold - DEFAULT_DISTANCE_THRESHOLD).abs() > f32::EPSILON {
            tracing::info!(
                threshold,
                default_threshold = DEFAULT_DISTANCE_THRESHOLD,
                "OnnxDiarizer: using non-default distance threshold"
            );
        }
        Ok(Self {
            model,
            mel: MelExtractor::new(),
            clusters: Mutex::new(SessionClusterState::new(threshold)),
        })
    }

    /// Run the embedding model on a single chunk of 16 kHz mono PCM.
    /// Returns the 256-d embedding as a `Vec<f32>`.
    ///
    /// Fails if the audio is shorter than [`MIN_FRAMES_FOR_EMBEDDING`]
    /// frames after Mel-FB extraction (caller falls back to the
    /// source-derived label), or if the tract inference returns an error.
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>> {
        embed_with(&self.model, &self.mel, samples)
    }
}

/// Fbank → CMN → model. Free function so the evaluation harness can
/// run alternative front ends through the same model.
fn embed_with(
    model: &TypedRunnableModel<TypedModel>,
    mel: &MelExtractor,
    samples: &[f32],
) -> Result<Vec<f32>> {
    embed_features(model, mel, samples, true)
}

fn embed_features(
    model: &TypedRunnableModel<TypedModel>,
    mel: &MelExtractor,
    samples: &[f32],
    cmn: bool,
) -> Result<Vec<f32>> {
    let mut feats = mel.extract(samples);
    let num_frames = feats.len() / NUM_MEL_BINS;
    if num_frames < MIN_FRAMES_FOR_EMBEDDING {
        return Err(anyhow!(
            "audio too short for embedding ({num_frames} frames < {MIN_FRAMES_FOR_EMBEDDING})"
        ));
    }
    // Per-utterance CMN, as wespeaker trains and infers (#1013). Without
    // it the model sees an out-of-distribution per-bin offset.
    if cmn {
        apply_cmn(&mut feats);
    }

    // Reshape the flat row-major (num_frames, 80) buffer into a
    // 3-D tensor with a unit batch dimension: (1, num_frames, 80).
    let input: Tensor =
        tract_ndarray::Array3::<f32>::from_shape_vec((1, num_frames, NUM_MEL_BINS), feats)
            .context("tract: reshape Mel features into (1, frames, 80)")?
            .into();

    let result = model
        .run(tvec!(input.into()))
        .context("tract: session run")?;

    let view = result[0]
        .to_array_view::<f32>()
        .context("tract: extract f32 output tensor")?;

    if view.len() != EMBEDDING_DIM {
        return Err(anyhow!(
            "tract: unexpected embedding length {} (expected {EMBEDDING_DIM})",
            view.len()
        ));
    }

    Ok(view.iter().copied().collect())
}

impl Diarize for OnnxDiarizer {
    fn label_utterances(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        format: CaptureFormat,
    ) {
        let hints = vec![ChunkHint::default(); utterances.len()];
        self.label_utterances_with_hints(utterances, audio_chunks, &hints, format);
    }

    fn label_utterances_with_hints(
        &self,
        utterances: &mut [Utterance],
        audio_chunks: &[Vec<f32>],
        hints: &[ChunkHint],
        format: CaptureFormat,
    ) {
        if utterances.is_empty() {
            return;
        }
        // Without per-utterance audio there's no signal to embed;
        // fall through to leave the source-derived stamp in place.
        if audio_chunks.len() != utterances.len() || hints.len() != utterances.len() {
            tracing::warn!(
                utterances = utterances.len(),
                chunks = audio_chunks.len(),
                hints = hints.len(),
                "OnnxDiarizer: audio_chunks / hints length mismatches utterances; skipping"
            );
            return;
        }

        // Compute all embeddings before acquiring the clusters lock.
        // Inference (~50–100 ms per utterance) must not hold the lock:
        // a future concurrent caller would stall for the full batch.
        let embeddings: Vec<Option<(Vec<f32>, f32)>> = audio_chunks
            .iter()
            .map(|chunk| {
                let resampled = prepare_audio_for_embedding(chunk, format);
                let duration_secs = resampled.len() as f32 / SAMPLE_RATE_HZ as f32;
                match self.embed(&resampled) {
                    Ok(emb) => Some((emb, duration_secs)),
                    Err(e) => {
                        tracing::debug!(error = %e, "OnnxDiarizer: skip utterance");
                        None
                    }
                }
            })
            .collect();

        // Cluster assignment is pure Rust (~microseconds per utterance);
        // hold the lock only for this tight loop.
        let mut session_clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        for (i, emb_opt) in embeddings.into_iter().enumerate() {
            let Some((emb, duration_secs)) = emb_opt else {
                continue;
            };
            let meta = ChunkMeta {
                duration_secs,
                hint: hints[i],
                started_at_ms: utterances[i].started_at_ms,
                ended_at_ms: utterances[i].ended_at_ms,
            };
            if let Some(cluster_id) = session_clusters.assign(&emb, meta) {
                utterances[i].speaker_label = Some(session_clusters.label(cluster_id));
            }
            // `emb` is a voiceprint; don't leave it in freed heap.
            let mut emb = emb;
            zeroize::Zeroize::zeroize(&mut emb);
        }
    }

    /// Reset speaker cluster state for a new meeting session. Preserves the
    /// distance threshold so the user's tuning carries over, but clears all
    /// per-session speaker history so IDs from a previous meeting do not
    /// bleed into the next one.
    fn reset(&self) {
        let mut clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        let threshold = clusters.distance_threshold;
        *clusters = SessionClusterState::new(threshold);
        tracing::debug!("OnnxDiarizer: cluster state reset for new session");
    }

    fn session_centroids(&self) -> Vec<(usize, Vec<f32>, usize)> {
        let clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        clusters.centroids()
    }

    fn set_distance_threshold(&self, threshold: f32) {
        let mut clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        clusters.distance_threshold = threshold.clamp(0.0, 2.0);
        tracing::info!(
            threshold = clusters.distance_threshold,
            "OnnxDiarizer: updated distance threshold"
        );
    }

    fn finalize_session(&self) -> Vec<Relabel> {
        let mut clusters = self.clusters.lock().unwrap_or_else(|e| e.into_inner());
        // The average-linkage cut uses the live threshold: on the #1013
        // sweep the re-cluster was best at the same 0.6 as the online
        // matcher, and tying them keeps the Settings slider meaningful
        // for both passes.
        let threshold = clusters.distance_threshold;
        clusters.recluster(threshold)
    }
}

/// Load, optimise, and compile the wespeaker ONNX model into a
/// tract inference plan. Called once at [`OnnxDiarizer::new`].
///
/// Verifies the file's SHA-256 against the catalog before handing
/// it to tract — defence-in-depth against a sibling app substituting
/// the model file (#111 audit).
///
/// tract is pure Rust — zero Metal/CoreML dispatch — so IOAccelerator
/// regions never accumulate, fixing the ~1.25 GB/min growth rate
/// that ORT's `download-binaries` prebuilts exhibited (#641).
fn build_tract_model(model_path: &Path) -> Result<TypedRunnableModel<TypedModel>> {
    verify_model_sha256(model_path)
        .with_context(|| format!("verify SHA-256 of model at {}", model_path.display()))?;

    tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: load ONNX from {}", model_path.display()))?
        .into_optimized()
        .context("tract: optimize model")?
        .into_runnable()
        .context("tract: make model runnable")
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
    let got = crate::transcription::download::hex_encode(hasher.finalize());
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
#[path = "eval_tests.rs"]
mod eval_tests;

#[cfg(test)]
mod tests {
    use super::*;

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
    /// path.
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
