//! Parakeet ASR engine (#521), via [`parakeet-rs`] over ONNX Runtime.
//!
//! ## Why this exists
//!
//! Whisper is an encoder–decoder model over a fixed 30-second window.
//! The meeting pump doesn't have 30-second windows — it has a 500 ms
//! tick and a continuously growing utterance — so the streaming path
//! re-runs inference over an ever-larger prefix and discards most of
//! the result. That is the structural cause behind #612 / #636.
//!
//! Parakeet TDT is a transducer: it walks the encoder frame sequence
//! left to right, emitting tokens and carrying LSTM state forward.
//! More audio is an append, not a re-decode.
//!
//! ## Why ORT, after #641 removed it
//!
//! This **reverses** #641 for this engine. It is not a loophole in it,
//! and the distinction matters if you are here to change something.
//!
//! #641 removed `ort` from the diarizer because ORT's Apple Silicon
//! prebuilts dispatch through Metal Performance Shaders even on the CPU
//! execution provider, growing IOAccelerator ~1.25 GB/min. It also
//! considered building ORT from source with Metal disabled and recorded
//! that as "Not done". We take the **prebuilts** — the same artifact it
//! named. [`crate::diarization::onnx`] stays on `tract-onnx` and should.
//!
//! Two things license the reversal, and neither is a reinterpretation
//! of #641:
//!
//! 1. **tract cannot load these graphs at all.** The encoder omits
//!    `Pad`'s optional `constant_value` by passing `''` (the ONNX
//!    convention for "skip this input"); tract discards empty-named
//!    inputs, then its `Pad` rule demands exactly 3, sees 2, and fails
//!    analysis across 48 nodes — fp32 and int8 alike, so not a
//!    quantization artefact. The preprocessor separately needs a rank-3
//!    `STFT` and the export supplies rank 2. So the choice here is ORT
//!    or no Parakeet.
//! 2. **New measurement** — see below. #641 measured `ort` rc.12 with
//!    wespeaker across many short sessions. This is rc.13 with Parakeet
//!    in one long-lived session. Different prebuilt, different
//!    workload, different result.
//!
//! Leaving `parakeet-rs`'s `coreml` feature off is *not* a mitigation
//! for #641's mechanism — that was MPS dispatch on the CPU EP, which
//! CoreML being off does nothing about. It is off because upstream
//! reports it unstable for these models. The memory result is the only
//! thing standing between this engine and #641's conclusion.
//!
//! ## Does ORT leak here? Measured: no
//!
//! [`tests::memory_soak_over_many_inferences`], 300 inferences of the
//! 11 s JFK fixture (≈55 minutes of audio), debug build, CPU EP:
//!
//! - Physical footprint **flat** — 1550 MB at iteration 10, 1548 MB at
//!   iteration 300 (int8).
//! - IOAccelerator region count **constant** (25 int8 / 38 fp32);
//!   resident bytes went *down* over the run.
//! - RSS went down too.
//!
//! IOAccelerator regions do exist — ORT touches the GPU path — but they
//! are a bounded cost paid once at model load, not #641's unbounded
//! ~1.25 GB/min growth. **Presence of IOAccelerator is not the bug;
//! unbounded growth is.**
//!
//! Throughput on the same fixture: int8 ≈13.7× realtime, fp32 ≈21×.
//! int8 is 639 MB on disk against fp32's ~2.5 GB and holds ~750 MB less
//! resident, so it is the better default; fp32 is the speed option.
//!
//! **Still on probation.** The soak is a tight inference loop, not a
//! real session with capture, diarization, and the HUD running, and
//! this engine is not yet wired into the pump. `npm run memwatch` on a
//! real meeting is the acceptance gate — see `docs/memory-debugging.md`
//! for why footprint rather than RSS is the number to read.
//!
//! ## Model files
//!
//! One directory holding either variant:
//!
//! - **int8** (recommended, 639 MB) — `encoder-model.int8.onnx`,
//!   `decoder_joint-model.int8.onnx`, `vocab.txt`
//! - **fp32** (~2.5 GB) — `encoder-model.onnx`,
//!   `encoder-model.onnx.data`, `decoder_joint-model.onnx`, `vocab.txt`
//!
//! Published at
//! [istupakov/parakeet-tdt-0.6b-v3-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
//! under CC BY 4.0.
//!
//! ## Note for #1004
//!
//! Parakeet already emits correct casing and punctuation — most of what
//! the LLM-refinement issue exists to fix. The upstream `Nemotron`
//! streaming variants go further and drop disfluencies. Worth evaluating
//! those before committing to a second inference stack and a 2 GB LLM.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use parakeet_rs::{ParakeetTDT, Transcriber};

#[cfg(test)]
mod tests;

/// Sample rate the model expects. Audio must be resampled to this
/// before it arrives; there is no resampling stage inside the model.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Accepted encoder filenames, fp32 first — mirroring `parakeet-rs`'s
/// own resolution order so our pre-flight check agrees with what the
/// loader will actually pick.
///
/// The int8 encoder is ~670 MB against fp32's ~2.5 GB (the latter also
/// needing an `encoder-model.onnx.data` sidecar, since it exceeds
/// protobuf's 2 GB message limit). Both work under ORT.
const ENCODER_CANDIDATES: &[&str] = &[
    "encoder-model.onnx",
    "encoder.onnx",
    "encoder-model.int8.onnx",
];

/// Accepted decoder+joint filenames, same ordering rationale.
const DECODER_CANDIDATES: &[&str] = &[
    "decoder_joint-model.onnx",
    "decoder_joint-model.int8.onnx",
    "decoder_joint.onnx",
    "decoder-model.onnx",
];

/// Vocabulary file. Not variant-dependent.
const VOCAB_FILE: &str = "vocab.txt";

/// The fp32 encoder exceeds protobuf's 2 GB message limit, so its
/// weights live in a sidecar that must sit beside the `.onnx`. Copying
/// only the `.onnx` is an easy mistake and the resulting ORT error is
/// opaque, so both [`ParakeetModel::load`] and
/// [`ParakeetModel::files_present`] check it here — they must agree.
fn fp32_sidecar_ok(dir: &Path) -> bool {
    !dir.join("encoder-model.onnx").is_file() || dir.join("encoder-model.onnx.data").is_file()
}

/// Compile-time proof that the model is safe to park in `AppState`
/// behind an `Arc`, the way every other inference seam is.
///
/// Asserted now rather than at integration time: nothing uses this
/// engine yet, so a `parakeet-rs` bump that introduced an `Rc` would
/// compile cleanly here and only fail once someone wired it into the
/// pump — a long way from the cause.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ParakeetModel>();
};

/// A loaded Parakeet TDT model.
///
/// `ParakeetTDT::transcribe_samples` takes `&mut self` (the transducer
/// carries decoder state), so the model sits behind a `Mutex`. That is
/// the same shape as the shared `Arc<Mutex<WhisperContext>>`, and the
/// same caveat would apply once this is wired into the pump: holding
/// the lock across an inference serialises callers. Concurrent meetings
/// are deferred today because of the Whisper context and the shared
/// diarizer (see `ARCHITECTURE.md`) — not because of this engine, which
/// nothing uses yet — but adopting Parakeet would not by itself lift
/// that restriction.
pub struct ParakeetModel {
    inner: Mutex<ParakeetTDT>,
    label: String,
}

impl ParakeetModel {
    /// Load the TDT model from a directory.
    ///
    /// Expensive — ORT parses and optimises a ~2.5 GB graph. Load once
    /// and hold it; never per utterance.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        for (what, candidates) in [
            ("encoder", ENCODER_CANDIDATES),
            ("decoder+joint", DECODER_CANDIDATES),
            ("vocabulary", &[VOCAB_FILE]),
        ] {
            if !candidates.iter().any(|f| dir.join(f).is_file()) {
                return Err(anyhow!(
                    "parakeet: no {what} model found in {} — expected one of: {}",
                    dir.display(),
                    candidates.join(", ")
                ));
            }
        }
        if !fp32_sidecar_ok(dir) {
            return Err(anyhow!(
                "parakeet: encoder-model.onnx is present but its weights sidecar \
                 encoder-model.onnx.data is missing from {}",
                dir.display()
            ));
        }
        // `None` config = default execution provider (CPU). Explicitly
        // not requesting CoreML/WebGPU: see the #641 note in the module
        // header before changing this.
        let inner = ParakeetTDT::from_pretrained(dir, None)
            .map_err(|e| anyhow!("{e}"))
            .with_context(|| format!("parakeet: load TDT model from {}", dir.display()))?;
        Ok(Self {
            inner: Mutex::new(inner),
            label: "parakeet-tdt-0.6b-v3".to_owned(),
        })
    }

    /// Whether a usable model (fp32 or int8) is present, without paying
    /// the load cost.
    ///
    /// Must agree with [`load`](Self::load)'s pre-flight checks — a
    /// model picker that reports "ready" here and then fails to load is
    /// worse than one that reports "not ready". Both go through
    /// [`fp32_sidecar_ok`] for that reason.
    pub fn files_present(dir: impl AsRef<Path>) -> bool {
        let dir = dir.as_ref();
        [ENCODER_CANDIDATES, DECODER_CANDIDATES, &[VOCAB_FILE]]
            .iter()
            .all(|candidates| candidates.iter().any(|f| dir.join(f).is_file()))
            && fp32_sidecar_ok(dir)
    }

    /// Catalog/display identifier for this backend.
    pub fn model_label(&self) -> &str {
        &self.label
    }

    /// Transcribe 16 kHz mono f32 samples.
    ///
    /// Returns an empty string for empty input rather than erroring —
    /// the pump can hand us a silent tick and that is not a failure.
    pub fn transcribe(&self, samples: &[f32]) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        // Recover from poisoning rather than failing forever. The guarded
        // value is a model, not a structure with invariants a panic could
        // have torn — and `parakeet-rs` has live panic paths (it indexes
        // ORT session outputs by name, which panics on a graph whose
        // output names differ from what it expects). Without
        // `into_inner`, one such panic would make every later call in the
        // process return "mutex poisoned", which is both unrecoverable
        // and useless to the user.
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let result = guard
            // Takes an owned Vec — the transducer consumes the buffer.
            // One copy per call; negligible beside the inference.
            .transcribe_samples(samples.to_vec(), SAMPLE_RATE_HZ, 1, None)
            .map_err(|e| anyhow!("{e}"))
            .context("parakeet: transcribe")?;
        Ok(result.text)
    }
}
