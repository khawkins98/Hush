//! Parakeet TDT 0.6B v3 ASR engine (#521) — **feasibility spike, not a
//! working engine.**
//!
//! # STATUS: BLOCKED. Read this before building on the module.
//!
//! `tract-onnx` cannot load the published Parakeet export. This was
//! measured, not assumed — [`tests::each_graph_loads_in_tract`]
//! reproduces it against a real download. Per graph:
//!
//! | Graph | int8 export | fp32 export |
//! |---|---|---|
//! | `nemo128.onnx` (preprocessor) | ✗ `STFT` | ✗ `STFT` (same file) |
//! | `encoder-model*.onnx` | ✗ `Pad` | ✗ `Pad` |
//! | `decoder_joint-model*.onnx` | ✗ `DynamicQuantizeLSTM` | ✓ **loads** |
//!
//! The three failures are unrelated to each other:
//!
//! 1. **`Pad` (encoder) — the blocker with no workaround.** The export
//!    omits `Pad`'s optional `constant_value` by passing `''`, the ONNX
//!    convention for "skip this input" (opset 17). tract drops
//!    empty-named inputs and its `Pad` rule then demands exactly 3,
//!    sees 2, and fails analysis. Affects int8 *and* fp32 identically,
//!    so it is not a quantization artefact — it is tract not handling
//!    omitted optional inputs. 48 `Pad` nodes are affected.
//! 2. **`STFT` (preprocessor).** tract's `STFT` requires a rank-3
//!    signal; the export feeds rank 2. Workaroundable — we would hand-
//!    roll the 128-bin NeMo log-mel in Rust (`realfft` is already
//!    in-tree for [`crate::diarization::features`], though at 80 bins
//!    for wespeaker, so it is a new filterbank rather than a reuse).
//! 3. **`DynamicQuantizeLSTM` (int8 decoder).** A `com.microsoft`
//!    contrib op — ONNX Runtime-specific and never coming to tract.
//!    Moot: the fp32 decoder is standard-domain and works.
//!
//! Note the consequence for download size: only the fp32 encoder is a
//! candidate, so the "~670 MB, like Detto" figure does **not** apply to
//! us. tract-viable weights are ~2.5 GB.
//!
//! ## Getting unblocked
//!
//! Roughly in order of cost:
//!
//! - **Re-export from NeMo** with tract-friendly ops (materialise the
//!   `Pad` constant instead of omitting it; drop `STFT` in favour of
//!   feeding precomputed mel). Best outcome — no fork, no upstream wait.
//! - **Patch tract** to tolerate omitted optional inputs on `Pad` and
//!   rank-2 `STFT`. Genuinely upstreamable; both are spec-conformant.
//! - **Graph surgery at build time.** Rewrite offending nodes before
//!   tract sees them. Fragile and re-breaks on every model revision.
//!
//! Reintroducing `ort` is **not** on the list. It would load all of
//! this happily, and that is exactly the trap #641 removed: ORT's Apple
//! Silicon prebuilts dispatch through Metal Performance Shaders even on
//! the CPU execution provider, causing unbounded IOAccelerator growth.
//!
//! ## What is nonetheless real here
//!
//! The parts that do not depend on tract are implemented and tested:
//! the [`vocab`] parser + SentencePiece join, and the greedy TDT loop
//! in [`decode`] with its anti-hang guards. Those carry forward
//! unchanged under any of the unblocking routes above, which is why
//! this is committed rather than discarded.
//!
//! ## Why a second engine at all
//!
//! Whisper is an encoder–decoder model over a fixed 30-second window.
//! The meeting pump doesn't have 30-second windows — it has a 500 ms
//! tick and a continuously growing utterance — so the streaming path
//! re-runs inference over an ever-larger prefix and throws most of the
//! result away. That is the structural reason behind the memory and
//! latency work in #612 / #636.
//!
//! Parakeet is a Token-and-Duration Transducer (TDT). It consumes the
//! encoder frame sequence left-to-right and emits tokens as it goes,
//! carrying its own LSTM state forward. Feeding it more audio is an
//! append, not a re-decode. That is a much better fit for a pump, and
//! it is why this engine exists alongside Whisper rather than
//! replacing it outright.
//!
//! ## The three graphs
//!
//! The upstream export ships the pipeline as three ONNX files. On
//! paper that removes the two pieces #516 flagged as the hard parts —
//! a NeMo-compatible mel preprocessor and a SentencePiece tokenizer —
//! since neither would be ours to write. In practice the preprocessor
//! is one of the graphs tract rejects, so the mel work comes back; see
//! the STATUS section above.
//!
//! | File | Signature |
//! |---|---|
//! | `nemo128.onnx` | `waveforms [B, N] f32`, `waveforms_lens [B] i64` → `features [B, 128, T] f32`, `features_lens [B] i64` |
//! | `encoder-model.int8.onnx` | `audio_signal [B, 128, T] f32`, `length [B] i64` → `outputs [B, 1024, T']`, `encoded_lengths [B] i64` |
//! | `decoder_joint-model.int8.onnx` | encoder frame + previous token + LSTM state → `[.., 8198]` logits + next state |
//!
//! The preprocessor is the NeMo mel filterbank exported to ONNX, so it
//! matches training exactly rather than approximately. The vocabulary
//! is a plain `vocab.txt`, already de-tokenized into SentencePiece
//! pieces — a `▁`-prefix join is the whole "tokenizer".
//!
//! ## Reading the joint output
//!
//! The joint head is 8198 wide over a 8193-entry vocabulary. The split
//! is `[0..8193)` token logits (index 8192 is `<blk>`) and `[8193..8198)`
//! duration logits. That trailing block is what makes this TDT rather
//! than plain RNN-T: instead of advancing one encoder frame per step
//! and emitting blanks to pass time, the model *predicts how many
//! frames to skip*. Decoding therefore jumps forward by the argmax
//! duration, which is where the speed advantage over RNN-T comes from.
//!
//! See [`decode`] for the greedy loop and the guards that keep it
//! terminating.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

mod decode;
mod vocab;

#[cfg(test)]
mod tests;

pub use decode::decode;
pub use vocab::Vocabulary;

/// Sample rate the preprocessor graph was exported for. Audio must be
/// resampled to this before it reaches [`ParakeetModel::transcribe`];
/// the graph has no resampling stage and will silently produce
/// garbage features for anything else.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Mel bins produced by `nemo128.onnx` (`features_size` in the
/// upstream `config.json`). Note this is 128, not Whisper's 80 — the
/// two engines do NOT share a preprocessor.
pub const NUM_MEL_BINS: usize = 128;

/// Encoder hidden size — the middle dimension of the encoder output
/// and the width of each frame handed to the joint network.
pub const ENCODER_DIM: usize = 1024;

/// Number of duration logits at the tail of the joint output. The
/// upstream TDT config uses durations `[0, 1, 2, 3, 4]`.
pub const NUM_DURATIONS: usize = 5;

/// Durations, in encoder frames, that each duration logit selects.
pub const DURATIONS: [usize; NUM_DURATIONS] = [0, 1, 2, 3, 4];

/// Predictor LSTM state width (`input_states_*` third dimension).
pub const PRED_STATE_DIM: usize = 640;

/// Predictor LSTM layer count (`input_states_*` leading dimension).
pub const PRED_STATE_LAYERS: usize = 2;

/// Filenames the loader expects inside the model directory. These are
/// the upstream names as published, so a user can drop a manual export
/// in the same directory and have it work.
///
/// These name the **fp32** variants deliberately. The int8 export is
/// four times smaller and would otherwise be the obvious choice, but
/// its decoder uses `DynamicQuantizeLSTM` — an ONNX Runtime contrib op
/// tract cannot load. fp32 is the only variant with any path forward
/// (see the STATUS section in the module header).
pub const PREPROCESSOR_FILE: &str = "nemo128.onnx";
pub const ENCODER_FILE: &str = "encoder-model.onnx";
pub const DECODER_JOINT_FILE: &str = "decoder_joint-model.onnx";
pub const VOCAB_FILE: &str = "vocab.txt";

/// External weights for [`ENCODER_FILE`]. The fp32 encoder exceeds
/// protobuf's 2 GB message limit, so its tensors live in a sidecar that
/// must sit beside the `.onnx` for tract to resolve them. Easy to miss
/// when copying files by hand, hence the explicit presence check.
pub const ENCODER_DATA_FILE: &str = "encoder-model.onnx.data";

type Runnable = TypedRunnableModel<TypedModel>;

/// A loaded Parakeet pipeline: three compiled tract plans plus the
/// vocabulary.
///
/// All three plans are `Send + Sync` (tract's `SimplePlan` is), so —
/// exactly as with [`crate::diarization::onnx::OnnxDiarizer`] — this
/// needs no mutex around the models themselves. That matters for the
/// meeting pump, where the shared-`WhisperContext` mutex is the reason
/// concurrent meetings are still deferred (see `ARCHITECTURE.md`).
pub struct ParakeetModel {
    preprocessor: Runnable,
    encoder: Runnable,
    decoder_joint: Runnable,
    vocab: Vocabulary,
}

impl ParakeetModel {
    /// Load all three graphs plus the vocabulary from `dir`.
    ///
    /// Fails if any file is missing or tract cannot parse/optimize a
    /// graph. Loading is expensive (the int8 encoder is ~650 MB and
    /// tract optimizes on load), so callers should do this once and
    /// hold the result — never per utterance.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let vocab = Vocabulary::load(dir.join(VOCAB_FILE))?;
        Ok(Self {
            preprocessor: build_plan(&dir.join(PREPROCESSOR_FILE))?,
            encoder: build_plan(&dir.join(ENCODER_FILE))?,
            decoder_joint: build_plan_with_facts(
                &dir.join(DECODER_JOINT_FILE),
                &decoder_joint_facts(),
            )?,
            vocab,
        })
    }

    /// Report whether every file [`load`](Self::load) needs is present
    /// in `dir`, without paying the load cost.
    ///
    /// Used by the catalog / model-picker path to decide whether the
    /// engine is selectable, and by `available()` on the trait impl.
    pub fn files_present(dir: impl AsRef<Path>) -> bool {
        let dir = dir.as_ref();
        [
            PREPROCESSOR_FILE,
            ENCODER_FILE,
            ENCODER_DATA_FILE,
            DECODER_JOINT_FILE,
            VOCAB_FILE,
        ]
        .iter()
        .all(|f| dir.join(f).is_file())
    }

    /// Borrow the vocabulary — decoding lives in [`decode`], which
    /// needs it to turn token ids into text.
    pub fn vocab(&self) -> &Vocabulary {
        &self.vocab
    }

    /// Run the preprocessor: raw 16 kHz mono f32 → `(128, T)` log-mel
    /// features, plus the valid frame count.
    ///
    /// Returned as a flat row-major `(NUM_MEL_BINS, frames)` buffer so
    /// the caller can hand it straight back to the encoder without a
    /// transpose.
    pub fn preprocess(&self, samples: &[f32]) -> Result<(Vec<f32>, usize)> {
        if samples.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let waveforms =
            tract_ndarray::Array2::<f32>::from_shape_vec((1, samples.len()), samples.to_vec())
                .context("parakeet: reshape waveform into (1, N)")?;
        let lens = tract_ndarray::Array1::<i64>::from_vec(vec![samples.len() as i64]);

        let out = self
            .preprocessor
            .run(tvec!(
                waveforms.into_tensor().into(),
                lens.into_tensor().into()
            ))
            .context("parakeet: preprocessor run")?;

        let features = out[0]
            .to_array_view::<f32>()
            .context("parakeet: read preprocessor features")?;
        let frames = out[1]
            .to_array_view::<i64>()
            .context("parakeet: read preprocessor feature lengths")?[0]
            as usize;

        let shape = features.shape();
        if shape.len() != 3 || shape[1] != NUM_MEL_BINS {
            return Err(anyhow!(
                "parakeet: unexpected preprocessor output shape {shape:?} \
                 (expected [1, {NUM_MEL_BINS}, T])"
            ));
        }
        // Clamp: `features_lens` is the count of *valid* frames, which
        // can be shorter than the padded time axis.
        let frames = frames.min(shape[2]);
        Ok((features.iter().copied().collect(), frames))
    }

    /// Run the encoder over preprocessed features.
    ///
    /// `features` is the flat `(NUM_MEL_BINS, padded_frames)` buffer
    /// from [`preprocess`](Self::preprocess). Returns the flat
    /// `(ENCODER_DIM, T')` encoder output and its valid length `T'`,
    /// where `T'` is roughly `frames / 8` (the config's
    /// `subsampling_factor`).
    pub fn encode(&self, features: &[f32], frames: usize) -> Result<(Vec<f32>, usize)> {
        if frames == 0 {
            return Ok((Vec::new(), 0));
        }
        let padded = features.len() / NUM_MEL_BINS;
        let signal = tract_ndarray::Array3::<f32>::from_shape_vec(
            (1, NUM_MEL_BINS, padded),
            features.to_vec(),
        )
        .context("parakeet: reshape features into (1, 128, T)")?;
        let length = tract_ndarray::Array1::<i64>::from_vec(vec![frames as i64]);

        let out = self
            .encoder
            .run(tvec!(
                signal.into_tensor().into(),
                length.into_tensor().into()
            ))
            .context("parakeet: encoder run")?;

        let encoded = out[0]
            .to_array_view::<f32>()
            .context("parakeet: read encoder output")?;
        let encoded_len = out[1]
            .to_array_view::<i64>()
            .context("parakeet: read encoder output length")?[0] as usize;

        let shape = encoded.shape();
        if shape.len() != 3 || shape[1] != ENCODER_DIM {
            return Err(anyhow!(
                "parakeet: unexpected encoder output shape {shape:?} \
                 (expected [1, {ENCODER_DIM}, T'])"
            ));
        }
        let encoded_len = encoded_len.min(shape[2]);
        Ok((encoded.iter().copied().collect(), encoded_len))
    }

    /// One step of the fused decoder+joint network.
    ///
    /// `frame` is a single `ENCODER_DIM`-wide encoder frame, `token` the
    /// previously emitted token id (the decoder's own input), and
    /// `state` the predictor LSTM state carried across steps. Returns
    /// the `8198`-wide joint logits and the updated state.
    ///
    /// Kept as a thin, allocation-honest wrapper rather than folded
    /// into [`decode`] so the decoder loop can be unit-tested against a
    /// scripted step function without any ONNX files present.
    pub fn step(
        &self,
        frame: &[f32],
        token: i32,
        state: &PredictorState,
    ) -> Result<(Vec<f32>, PredictorState)> {
        let enc = tract_ndarray::Array3::<f32>::from_shape_vec((1, ENCODER_DIM, 1), frame.to_vec())
            .context("parakeet: reshape encoder frame into (1, 1024, 1)")?;
        let targets = tract_ndarray::Array2::<i32>::from_shape_vec((1, 1), vec![token])
            .context("parakeet: build targets tensor")?;
        let target_length = tract_ndarray::Array1::<i32>::from_vec(vec![1]);

        let out = self
            .decoder_joint
            .run(tvec!(
                enc.into_tensor().into(),
                targets.into_tensor().into(),
                target_length.into_tensor().into(),
                state.h.clone().into_tensor().into(),
                state.c.clone().into_tensor().into()
            ))
            .context("parakeet: decoder_joint run")?;

        let logits: Vec<f32> = out[0]
            .to_array_view::<f32>()
            .context("parakeet: read joint logits")?
            .iter()
            .copied()
            .collect();
        let h = out[2]
            .to_array_view::<f32>()
            .context("parakeet: read output_states_1")?
            .to_owned()
            .into_dimensionality::<tract_ndarray::Ix3>()
            .context("parakeet: output_states_1 dimensionality")?;
        let c = out[3]
            .to_array_view::<f32>()
            .context("parakeet: read output_states_2")?
            .to_owned()
            .into_dimensionality::<tract_ndarray::Ix3>()
            .context("parakeet: output_states_2 dimensionality")?;

        Ok((logits, PredictorState { h, c }))
    }

    /// Full pipeline: 16 kHz mono f32 samples → text.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        let (features, frames) = self.preprocess(samples)?;
        if frames == 0 {
            return Ok(String::new());
        }
        let (encoded, encoded_len) = self.encode(&features, frames)?;
        if encoded_len == 0 {
            return Ok(String::new());
        }
        let tokens = decode(encoded_len, |frame_idx, token, state| {
            let start = frame_idx;
            // Encoder output is (ENCODER_DIM, T') row-major, so a frame
            // is a strided gather down the time axis, not a slice.
            let frame: Vec<f32> = (0..ENCODER_DIM)
                .map(|d| encoded[d * encoded_len + start])
                .collect();
            self.step(&frame, token, state)
        })?;
        Ok(self.vocab.decode_tokens(&tokens))
    }
}

/// Predictor LSTM state carried between [`ParakeetModel::step`] calls.
///
/// Both tensors are `[PRED_STATE_LAYERS, batch, PRED_STATE_DIM]`.
#[derive(Clone)]
pub struct PredictorState {
    pub h: tract_ndarray::Array3<f32>,
    pub c: tract_ndarray::Array3<f32>,
}

impl PredictorState {
    /// Zero state — what a fresh utterance starts from.
    pub fn zeros() -> Self {
        Self {
            h: tract_ndarray::Array3::zeros((PRED_STATE_LAYERS, 1, PRED_STATE_DIM)),
            c: tract_ndarray::Array3::zeros((PRED_STATE_LAYERS, 1, PRED_STATE_DIM)),
        }
    }
}

impl Default for PredictorState {
    fn default() -> Self {
        Self::zeros()
    }
}

/// Parse + optimize one ONNX graph into a runnable tract plan.
///
/// Deliberately does NOT verify a SHA like the diarizer's loader does:
/// Parakeet is a multi-file model whose integrity is checked at
/// download time across the whole file set, rather than per-graph at
/// load time. See the downloader for where that check lives.
fn build_plan(path: &PathBuf) -> Result<Runnable> {
    build_plan_with_facts(path, &[])
}

/// As [`build_plan`], but pins concrete input shapes before analysis.
///
/// Needed for the decoder+joint graph. Its export gives each input its
/// own dynamic batch symbol (`targets_dynamic_axes_1`,
/// `input_states_1_dynamic_axes_1`, …). tract's LSTM shape rule
/// requires the state batch dim and the sequence batch dim to unify,
/// and it cannot prove two *distinct* symbols are equal — so analysis
/// fails even though both are always 1 in practice. Pinning every
/// input to a concrete shape replaces the symbols with literals and
/// the rule discharges trivially.
///
/// We only ever run this graph one frame and one batch at a time (the
/// greedy loop is inherently sequential), so fixing the shapes costs
/// nothing.
fn build_plan_with_facts(path: &PathBuf, facts: &[(usize, InferenceFact)]) -> Result<Runnable> {
    if !path.is_file() {
        return Err(anyhow!(
            "parakeet: missing model file {} — the engine needs all of \
             {PREPROCESSOR_FILE}, {ENCODER_FILE}, {DECODER_JOINT_FILE}, {VOCAB_FILE}",
            path.display()
        ));
    }
    let mut model = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("parakeet: load ONNX from {}", path.display()))?;
    for (idx, fact) in facts {
        model = model
            .with_input_fact(*idx, fact.clone())
            .with_context(|| format!("parakeet: pin input {idx} of {}", path.display()))?;
    }
    model
        .into_optimized()
        .with_context(|| format!("parakeet: optimize {}", path.display()))?
        .into_runnable()
        .with_context(|| format!("parakeet: make runnable {}", path.display()))
}

/// Input facts that make the decoder+joint graph analysable — see
/// [`build_plan_with_facts`] for why they're required.
fn decoder_joint_facts() -> Vec<(usize, InferenceFact)> {
    vec![
        // encoder_outputs [batch, ENCODER_DIM, frames]
        (0, f32::fact([1, ENCODER_DIM, 1]).into()),
        // targets [batch, U]
        (1, i32::fact([1, 1]).into()),
        // target_length [batch]
        (2, i32::fact([1]).into()),
        // input_states_1 / _2 [layers, batch, PRED_STATE_DIM]
        (3, f32::fact([PRED_STATE_LAYERS, 1, PRED_STATE_DIM]).into()),
        (4, f32::fact([PRED_STATE_LAYERS, 1, PRED_STATE_DIM]).into()),
    ]
}
