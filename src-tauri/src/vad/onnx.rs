//! Production [`crate::vad::VadModel`] backed by the bundled Silero VAD v5
//! ONNX model (16kHz-specialized; see `src-tauri/assets/README.md` and
//! `scripts/build-silero-vad-onnx.py` for the derivation). Loads once into
//! [`SileroVad`]; each [`crate::vad::VadSession`] minted from it owns its
//! own LSTM hidden state. Frame size and sample rate are pinned in
//! [`crate::vad`].

use anyhow::{anyhow, Context, Result};
use std::sync::Arc;
use tract_onnx::prelude::*;

use crate::vad::{VadModel, VadSession, FRAME_LEN_SAMPLES};

/// Bundled model bytes — 16kHz-specialized Silero v5.1.2 (see
/// `src-tauri/assets/README.md`). Avoids any first-run download dance.
const SILERO_VAD_ONNX: &[u8] = include_bytes!("../../assets/silero_vad.onnx");

/// SHA256 of the bundled file (verified at startup against the in-memory
/// bytes to catch a corrupted asset under git).
const SILERO_VAD_SHA256: &str = "38fafd5ad5a73fb8b75b555551e2db78d20c111b049ff031c22ccdf580f72bc7";

/// LSTM hidden-state shape: `[2, 1, 128]` (per Silero v5 ONNX signature).
const STATE_SHAPE: [usize; 3] = [2, 1, 128];

pub struct SileroVad {
    /// Compiled tract model. `TypedRunnableModel` is `Send + Sync`, so this
    /// `Arc` is shared across all sessions without locking.
    model: Arc<TypedRunnableModel<TypedModel>>,
}

impl SileroVad {
    /// Load the bundled model. Returns an error if tract can't parse the
    /// ONNX (means the bundled asset is broken — should fail loud at
    /// startup so we never silently degrade to NoopVad without telling
    /// the user).
    pub fn load() -> Result<Self> {
        // Self-check the bundled bytes haven't been corrupted under git.
        use sha2::{Digest, Sha256};
        let actual = format!("{:x}", Sha256::digest(SILERO_VAD_ONNX));
        if actual != SILERO_VAD_SHA256 {
            return Err(anyhow!(
                "bundled silero_vad.onnx SHA mismatch — expected {}, got {} \
                 (asset may be corrupted in checkout)",
                SILERO_VAD_SHA256,
                actual,
            ));
        }

        let mut cursor = std::io::Cursor::new(SILERO_VAD_ONNX);
        let model = tract_onnx::onnx()
            .model_for_read(&mut cursor)
            .context("parse silero_vad.onnx")?
            // Pin input shapes so tract can plan a fast typed graph. The
            // 16kHz-specialized model has only two inputs (audio + state);
            // `sr` was folded into a constant initializer.
            .with_input_fact(0, f32::fact([1, FRAME_LEN_SAMPLES]).into())
            .context("pin Silero `input` shape")?
            .with_input_fact(1, f32::fact(STATE_SHAPE).into())
            .context("pin Silero `state` shape")?
            .into_optimized()
            .context("optimise Silero graph")?
            .into_runnable()
            .context("compile Silero runnable")?;

        Ok(SileroVad {
            model: Arc::new(model),
        })
    }
}

impl VadModel for SileroVad {
    fn new_session(&self) -> Box<dyn VadSession> {
        Box::new(SileroVadSession {
            model: Arc::clone(&self.model),
            state: tract_ndarray::Array3::<f32>::zeros(STATE_SHAPE).into_dyn(),
        })
    }
}

pub struct SileroVadSession {
    model: Arc<TypedRunnableModel<TypedModel>>,
    /// LSTM hidden state, shape `[2, 1, 128]`. Re-written on every
    /// `score_frame` call from the model's `stateN` output. Zero-initialised
    /// at session start.
    state: tract_ndarray::ArrayD<f32>,
}

impl VadSession for SileroVadSession {
    fn score_frame(&mut self, frame: &[f32]) -> Result<f32> {
        if frame.len() != FRAME_LEN_SAMPLES {
            return Err(anyhow!(
                "Silero VAD expects frames of exactly {} samples; got {}",
                FRAME_LEN_SAMPLES,
                frame.len(),
            ));
        }

        // Inputs in declared order: audio f32 [1,512], state f32 [2,1,128].
        let audio_t: Tensor =
            tract_ndarray::Array2::from_shape_vec((1, FRAME_LEN_SAMPLES), frame.to_vec())
                .context("build Silero audio tensor")?
                .into();

        let state_t: Tensor = self.state.clone().into();

        let outputs = self
            .model
            .run(tvec!(audio_t.into(), state_t.into()))
            .context("Silero VAD inference")?;

        // Outputs in declared order: [prob f32 [1,1], stateN f32 [2,1,128]].
        let prob_view = outputs[0]
            .to_array_view::<f32>()
            .context("read Silero prob")?;
        if prob_view.len() != 1 {
            return Err(anyhow!(
                "Silero prob output has unexpected length {} (expected 1) — \
                 bundled model may have drifted from the [1,1] output contract",
                prob_view.len(),
            ));
        }
        let prob = prob_view
            .iter()
            .next()
            .copied()
            .ok_or_else(|| anyhow!("Silero prob output empty"))?;

        // Persist the new hidden state for the next call.
        self.state = outputs[1]
            .to_array_view::<f32>()
            .context("read Silero stateN")?
            .to_owned()
            .into_dyn();

        Ok(prob.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::SAMPLE_RATE_HZ;

    #[test]
    fn silero_loads_and_scores_silent_frame_low() {
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let silent = vec![0.0f32; FRAME_LEN_SAMPLES];
        // Warm the LSTM with silent frames — recurrent hidden state starts
        // at zero and converges to a steady-state under stationary input.
        // Without this, the very first frame's probability reflects the
        // zero-state transient rather than the silent-audio response.
        for _ in 0..4 {
            let _ = session.score_frame(&silent).unwrap();
        }
        let p = session.score_frame(&silent).expect("score");
        assert!(p < 0.3, "silent frame should score low; got {p}");
    }

    #[test]
    fn silero_scores_structured_noise_at_least_as_high_as_silence() {
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let silent = vec![0.0f32; FRAME_LEN_SAMPLES];

        // Warm the LSTM with silent frames — recurrent hidden state starts
        // at zero and converges to a steady-state under stationary input.
        // Without this, the very first frame's probability reflects the
        // zero-state transient rather than the silent-audio response.
        for _ in 0..4 {
            let _ = session.score_frame(&silent).unwrap();
        }
        let p_silent = session.score_frame(&silent).unwrap();

        let sr = SAMPLE_RATE_HZ as f32;
        let sine: Vec<f32> = (0..FRAME_LEN_SAMPLES)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * (i as f32) / sr).sin() * 0.5)
            .collect();
        for _ in 0..4 {
            let _ = session.score_frame(&sine).unwrap();
        }
        let p_noise = session.score_frame(&sine).unwrap();

        assert!(
            p_noise >= p_silent,
            "structured-noise prob ({p_noise}) should be >= silent prob ({p_silent})"
        );
    }

    #[test]
    fn silero_rejects_wrong_frame_size() {
        let model = SileroVad::load().expect("load Silero");
        let mut session = model.new_session();
        let wrong = vec![0.0f32; FRAME_LEN_SAMPLES + 1];
        assert!(session.score_frame(&wrong).is_err());
    }

    #[test]
    fn two_sessions_have_independent_lstm_state() {
        // Two sessions from one model. Feed different audio through each;
        // they should NOT see each other's state — concrete check: after
        // identical warm-up, identical input must produce identical output
        // regardless of what the other session is doing.
        let model = SileroVad::load().expect("load Silero");
        let mut s1 = model.new_session();
        let mut s2 = model.new_session();

        let silent = vec![0.0f32; FRAME_LEN_SAMPLES];
        let sr = SAMPLE_RATE_HZ as f32;
        let sine: Vec<f32> = (0..FRAME_LEN_SAMPLES)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * (i as f32) / sr).sin() * 0.5)
            .collect();

        // Warm both sessions identically.
        for _ in 0..4 {
            let _ = s1.score_frame(&silent).unwrap();
            let _ = s2.score_frame(&silent).unwrap();
        }

        // Diverge: s1 sees more silence, s2 sees sine. If state leaked across
        // sessions, the next silent frame on s1 would be polluted by s2's sine
        // exposure (and vice versa).
        let _ = s1.score_frame(&silent).unwrap();
        let _ = s2.score_frame(&sine).unwrap();

        // Now run an identical probe frame on a fresh third session, and on s1
        // (which has only seen silence). They must produce identical output —
        // identical state from identical history.
        let mut control = model.new_session();
        for _ in 0..5 {
            let _ = control.score_frame(&silent).unwrap();
        }
        let p_control = control.score_frame(&silent).unwrap();
        let p_s1 = s1.score_frame(&silent).unwrap();

        assert!(
            (p_control - p_s1).abs() < 1e-5,
            "s1 (silence-only history) should match a fresh control session; \
             got control={p_control}, s1={p_s1} — leak from s2's sine exposure?"
        );
    }
}
