//! Greedy TDT (Token-and-Duration Transducer) decoding.
//!
//! ## The loop
//!
//! Classic RNN-T greedy decoding walks encoder frames one at a time and
//! emits a blank to mean "nothing more here, advance". TDT adds a
//! second head that predicts *how many frames to skip*, so the loop
//! advances by a learned duration instead of always by one. Fewer
//! joint-network evaluations per second of audio; same transcript.
//!
//! Per step:
//!
//! 1. Evaluate the joint on `(encoder[t], last_token, state)`.
//! 2. Split the output: `[0..VOCAB_SIZE)` token logits, then
//!    [`NUM_DURATIONS`] duration logits.
//! 3. `argmax` each half independently.
//! 4. If the token is not blank, emit it and adopt the new predictor
//!    state. (On blank, both the emitted token and the state are
//!    discarded — the predictor only advances on real symbols.)
//! 5. Advance `t` by the chosen duration.
//!
//! ## Why the guards exist
//!
//! The duration head can legitimately predict 0 — that is how the model
//! emits several tokens against the same encoder frame. Left alone, a
//! zero-duration non-blank step is an infinite loop the moment the
//! model gets stuck in a repeat, which is a real failure mode on
//! degenerate audio (silence, tones, music). Two bounds keep it honest:
//!
//! - [`MAX_SYMBOLS_PER_FRAME`] caps emissions at a single `t`, then
//!   forces a one-frame advance.
//! - [`MAX_TOTAL_STEPS_PER_FRAME`] bounds total joint evaluations
//!   across the whole utterance, so even a pathological interleaving of
//!   zero-duration blanks terminates.
//!
//! Both are deliberately generous: they are anti-hang backstops, not
//! quality knobs, and hitting either means something is wrong. Both log
//! at `warn` when they fire.

use anyhow::{anyhow, Result};

use super::vocab::{BLANK_ID, VOCAB_SIZE};
use super::{PredictorState, DURATIONS, NUM_DURATIONS};

/// Maximum tokens emitted against one encoder frame before the decoder
/// forces `t` forward. NeMo's reference greedy decoder uses 10.
pub const MAX_SYMBOLS_PER_FRAME: usize = 10;

/// Ceiling on joint evaluations, expressed per encoder frame. With
/// durations averaging ~2 frames a healthy utterance runs well under
/// 1 step per frame, so 4× is a wide margin.
pub const MAX_TOTAL_STEPS_PER_FRAME: usize = 4;

/// Run greedy TDT decoding over `num_frames` encoder frames.
///
/// `step` is invoked as `step(frame_index, previous_token, state)` and
/// must return `(joint_logits, next_state)` where `joint_logits` is
/// `VOCAB_SIZE + NUM_DURATIONS` wide. Taking the step as a callback
/// keeps this loop testable against a scripted model — the guards above
/// are exactly the logic that needs adversarial tests, and standing up
/// a 650 MB ONNX graph to exercise them would be absurd.
///
/// Returns the emitted token ids, blanks already filtered.
pub fn decode<F>(num_frames: usize, mut step: F) -> Result<Vec<i32>>
where
    F: FnMut(usize, i32, &PredictorState) -> Result<(Vec<f32>, PredictorState)>,
{
    let mut tokens: Vec<i32> = Vec::new();
    if num_frames == 0 {
        return Ok(tokens);
    }

    let mut t = 0usize;
    // The predictor has no dedicated SOS symbol in this export; blank
    // is what NeMo feeds as the initial label.
    let mut last_token = BLANK_ID;
    let mut state = PredictorState::zeros();

    let mut symbols_at_t = 0usize;
    let mut total_steps = 0usize;
    let max_total_steps = num_frames.saturating_mul(MAX_TOTAL_STEPS_PER_FRAME);

    while t < num_frames {
        if total_steps >= max_total_steps {
            tracing::warn!(
                num_frames,
                total_steps,
                emitted = tokens.len(),
                "parakeet: greedy decode hit the total-step ceiling; \
                 returning the transcript decoded so far"
            );
            break;
        }
        total_steps += 1;

        let (logits, next_state) = step(t, last_token, &state)?;
        let expected = VOCAB_SIZE + NUM_DURATIONS;
        if logits.len() < expected {
            return Err(anyhow!(
                "parakeet: joint produced {} logits, expected at least {expected}",
                logits.len()
            ));
        }

        let token = argmax(&logits[..VOCAB_SIZE]);
        let duration_idx = argmax(&logits[VOCAB_SIZE..VOCAB_SIZE + NUM_DURATIONS]);
        let duration = DURATIONS[duration_idx];

        if token as i32 == BLANK_ID {
            // Blank: no emission, predictor state unchanged. Advance by
            // at least one frame — a zero-duration blank would other-
            // wise re-evaluate the identical (frame, token, state)
            // triple forever, since nothing about the input changed.
            t += duration.max(1);
            symbols_at_t = 0;
            continue;
        }

        tokens.push(token as i32);
        last_token = token as i32;
        state = next_state;
        symbols_at_t += 1;

        if duration == 0 {
            // Staying on this frame to emit another symbol is expected
            // TDT behaviour, but only up to a point.
            if symbols_at_t >= MAX_SYMBOLS_PER_FRAME {
                tracing::warn!(
                    frame = t,
                    emitted = tokens.len(),
                    "parakeet: greedy decode emitted {MAX_SYMBOLS_PER_FRAME} symbols \
                     against one frame; forcing advance"
                );
                t += 1;
                symbols_at_t = 0;
            }
        } else {
            t += duration;
            symbols_at_t = 0;
        }
    }

    Ok(tokens)
}

/// Index of the largest value. Ties resolve to the lowest index, which
/// matches `argmax` in NumPy/PyTorch and therefore the reference
/// decoder this was written against.
///
/// NaN is treated as "not greater than", so an all-NaN slice yields 0
/// rather than panicking — a degenerate joint output should produce a
/// junk token the caller can filter, not bring down the pump.
fn argmax(values: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_val = f32::NEG_INFINITY;
    for (i, &v) in values.iter().enumerate() {
        if v > best_val {
            best_val = v;
            best = i;
        }
    }
    best
}
