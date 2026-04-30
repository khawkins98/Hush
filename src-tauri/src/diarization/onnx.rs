//! ONNX-backed speaker-embedding diarizer (#111).
//!
//! Runs the wespeaker ResNet34-LM model over each utterance's audio
//! to produce a 256-dimensional speaker embedding, then hands the
//! per-utterance embeddings to [`super::cluster::cluster_with_threshold`]
//! to discover speaker turns. Final output: each utterance gets a
//! `"Speaker N"` label where utterances from the same speaker share
//! the same `N`, assigned in first-appearance order.
//!
//! ## Pipeline
//!
//! For each utterance, resample to 16 kHz mono (reuses
//! [`crate::transcription::resample::resample_to_mono`] +
//! `crate::audio::downmix_to_mono` from the Whisper preprocessing
//! path), compute 80-dim Mel-FB features via
//! [`super::features::MelExtractor`], feed `(1, num_frames, 80)` to
//! the ONNX session, and read the `(1, 256)` embedding back. Then
//! once across the whole batch: cluster via
//! [`super::cluster::cluster_with_threshold`] on the model's tuned
//! cosine threshold, and stamp each utterance `"Speaker {N+1}"` from
//! its 1-indexed cluster ID.
//!
//! ## Why a separate `Diarize` impl rather than swapping algorithms
//!
//! The trait's `label_utterances` signature already gives us audio
//! chunks parallel to utterances. The D1 [`super::EnergyDiarizer`]
//! ignores them; we use them. No interface change needed.
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

use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use ndarray::{Array2, Array3};
use ort::session::Session;
use ort::value::Value;

use crate::audio::CaptureFormat;
use crate::diarization::cluster::{cluster_with_threshold, DEFAULT_DISTANCE_THRESHOLD};
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

/// Production diarizer: ONNX speaker-embedding model + agglomerative
/// clustering. See module-level doc.
pub struct OnnxDiarizer {
    /// `ort` session loaded from the user's downloaded model file.
    /// Wrapped in a `Mutex` because `Session::run` takes `&mut self`
    /// — internal state (allocator, run options) is mutable across
    /// calls. The lock is held only for the duration of one
    /// inference call (~50–100 ms on CPU), and the meeting pump is
    /// the sole caller, so contention isn't a concern in practice.
    session: Mutex<Session>,
    /// Reusable Mel-FB extractor — holds the planned 512-pt FFT,
    /// Povey window, and 80-bin filterbank. Constructed once per
    /// `OnnxDiarizer`.
    mel: MelExtractor,
    /// Cosine-distance threshold passed to the clustering pass. The
    /// constructor seeds this with [`DEFAULT_DISTANCE_THRESHOLD`];
    /// kept on the struct so future tuning can flow through a
    /// settings row without changing the public API.
    distance_threshold: f32,
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
            distance_threshold: DEFAULT_DISTANCE_THRESHOLD,
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
        // so there's no real contention surface.
        let mut session = self
            .session
            .lock()
            .map_err(|e| anyhow!("ort: session mutex poisoned: {e}"))?;
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

        // Compute one embedding per utterance. Errors (audio too
        // short, ort failure) leave that utterance with `None` —
        // the dispatch path then falls back to the source-derived
        // "mic" / "system" label. We collect all embeddings even
        // if some are missing, with parallel indices, so the
        // clustering pass can ignore the gaps cleanly.
        let mut embeddings: Vec<Option<Vec<f32>>> = Vec::with_capacity(utterances.len());
        for chunk in audio_chunks.iter() {
            // Resample / downmix to 16 kHz mono if needed. The mel
            // extractor refuses anything else by construction
            // (SAMPLE_RATE_HZ is a const); keeps the boundary
            // checking honest.
            let resampled = prepare_audio_for_embedding(chunk, format);
            embeddings.push(match self.embed(&resampled) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::debug!(error = %e, "OnnxDiarizer: skip utterance");
                    None
                }
            });
        }

        // Clustering needs a packed `&[Vec<f32>]`. Build a packed
        // slice of just the successful embeddings + a parallel
        // index map so we can scatter the cluster IDs back to the
        // right utterance positions.
        let mut packed: Vec<Vec<f32>> = Vec::with_capacity(embeddings.len());
        let mut idx_map: Vec<usize> = Vec::with_capacity(embeddings.len());
        for (i, opt) in embeddings.iter().enumerate() {
            if let Some(v) = opt {
                idx_map.push(i);
                packed.push(v.clone());
            }
        }
        if packed.is_empty() {
            return;
        }
        let labels = cluster_with_threshold(&packed, self.distance_threshold);
        for (cluster_id, utt_idx) in labels.iter().zip(idx_map.iter()) {
            // 1-indexed for human display; "Speaker 1, 2, …".
            utterances[*utt_idx].speaker_label = Some(format!("Speaker {}", cluster_id + 1));
        }
    }
}

/// Mean-pool an `(N, EMBEDDING_DIM)` ndarray along axis 0 into a
/// flat `Vec<f32>` of length `EMBEDDING_DIM`. Exposed as a free
/// function so future variants of the diarizer (e.g. one that
/// chops long utterances into overlapping windows and averages
/// the per-window embeddings) can share the helper.
pub fn mean_pool_axis0(array: Array2<f32>) -> Vec<f32> {
    let n = array.shape()[0];
    if n == 0 {
        return vec![0.0; EMBEDDING_DIM];
    }
    let mean = array.mean_axis(ndarray::Axis(0)).unwrap_or_else(|| {
        // `mean_axis` returns None only on a zero-length axis,
        // which we already handled above. The fallback is a
        // belt-and-suspenders for an unreachable branch.
        ndarray::Array1::zeros(EMBEDDING_DIM)
    });
    mean.to_vec()
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

    #[test]
    fn mean_pool_empty_axis_returns_zero_vector() {
        // An empty (0, 256) array shouldn't panic; mean is 0.
        let a: Array2<f32> = Array2::zeros((0, EMBEDDING_DIM));
        let pooled = mean_pool_axis0(a);
        assert_eq!(pooled.len(), EMBEDDING_DIM);
        assert!(pooled.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn mean_pool_single_row_returns_that_row() {
        let mut a: Array2<f32> = Array2::zeros((1, EMBEDDING_DIM));
        a[[0, 0]] = 1.0;
        a[[0, 5]] = 0.5;
        let pooled = mean_pool_axis0(a);
        assert_eq!(pooled[0], 1.0);
        assert_eq!(pooled[5], 0.5);
        assert_eq!(pooled[1], 0.0);
    }

    #[test]
    fn mean_pool_two_rows_averages() {
        let mut a: Array2<f32> = Array2::zeros((2, EMBEDDING_DIM));
        a[[0, 0]] = 1.0;
        a[[1, 0]] = 3.0;
        let pooled = mean_pool_axis0(a);
        assert!((pooled[0] - 2.0).abs() < 1e-6);
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
