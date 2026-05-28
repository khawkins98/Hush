//! Voice Activity Detection — gates whisper inference behind a speech-presence
//! check so silent / non-speech windows don't trigger hallucinations
//! (".com", "Thanks for watching!", repeating-phrase loops). See
//! `docs/vad-hallucination-gate-proposal.md` for the design rationale.
//!
//! Production *will wire* [`onnx::SileroVad`] into the
//! [`crate::ipc::state::InferenceState`] `vad` slot at startup (Task 4 of the
//! implementation plan adds the slot — see `docs/vad-hallucination-gate-plan.md`).
//! Each streaming transcription session will mint a fresh [`VadSession`] at
//! start (`new_session`) and feed frames through it as audio arrives. A
//! `learnings.md` entry will be added when the feature lands.

pub mod onnx;

use anyhow::Result;

/// Sample rate the VAD operates at — matches the streaming inferer's mono-16kHz contract.
pub const SAMPLE_RATE_HZ: u32 = 16_000;

/// Silero VAD v5 expects 512-sample frames at 16kHz (~32ms). Exposing it as a
/// constant lets the caller chunk newly-fed audio correctly without hard-coding.
pub const FRAME_LEN_SAMPLES: usize = 512;

/// Heavy, immutable, shared across the app. Loads the ONNX model once.
/// Hands out per-stream [`VadSession`]s, each with its own recurrent state.
pub trait VadModel: Send + Sync {
    /// Mint a fresh per-stream session with zero-initialised recurrent state.
    fn new_session(&self) -> Box<dyn VadSession>;
}

/// Per-stream state for one ongoing audio source. Mutable because Silero's
/// LSTM hidden state evolves across calls. Calls MUST be in temporal order
/// on the same session — feeding frame N requires the prior call was for
/// frame N-1.
pub trait VadSession: Send {
    /// Speech probability ∈ [0,1] for one [`FRAME_LEN_SAMPLES`]-sample frame at
    /// [`SAMPLE_RATE_HZ`]. Updates internal state. Returns an error only if
    /// inference itself fails; never panics on slice length.
    fn score_frame(&mut self, frame: &[f32]) -> Result<f32>;
}

/// No-op fallback: always reports speech, so the gate never fires.
/// Used when the production Silero model fails to load (degrade gracefully —
/// transcription works as today, just without the gate) and by tests that
/// aren't exercising the gate.
pub struct NoopVad;

impl VadModel for NoopVad {
    fn new_session(&self) -> Box<dyn VadSession> {
        Box::new(NoopVadSession)
    }
}

/// Stateless companion to [`NoopVad`] — `score_frame` always returns `1.0`
/// (full speech), so there is no recurrent state to corrupt.
pub struct NoopVadSession;

impl VadSession for NoopVadSession {
    fn score_frame(&mut self, _frame: &[f32]) -> Result<f32> {
        Ok(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_vad_session_always_reports_full_speech() {
        let model = NoopVad;
        let mut session = model.new_session();
        let frame = vec![0.0f32; FRAME_LEN_SAMPLES];
        assert_eq!(session.score_frame(&frame).unwrap(), 1.0);
        // Repeated calls still return 1.0; no state to corrupt.
        assert_eq!(session.score_frame(&frame).unwrap(), 1.0);
    }

    #[test]
    fn constants_match_silero_v5_contract() {
        // Silero v5 mandates 512-sample frames at 16kHz; both are load-bearing
        // for downstream chunking and ONNX I/O. Pinned so a careless edit
        // doesn't silently regress.
        assert_eq!(FRAME_LEN_SAMPLES, 512);
        assert_eq!(SAMPLE_RATE_HZ, 16_000);
    }
}
