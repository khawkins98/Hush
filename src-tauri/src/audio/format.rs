//! Pure-logic PCM format helpers.
//!
//! This module is deliberately free of any OS or `cpal` dependency
//! so it can be unit-tested without an audio device. It exposes
//! channel downmixing; sample-rate conversion lives in
//! [`crate::transcription::resample`] (whisper.cpp expects 16 kHz
//! mono, the cpal capture format is host-native, the resample step
//! bridges them).

/// Apply a gain (in dB) to a slice of f32 PCM samples in place.
///
/// `gain_db` is the desired boost or attenuation: 0.0 = unity, +6 ≈ double
/// amplitude, –6 ≈ half. The linear multiplier is clamped so every output
/// sample stays in `[–1.0, 1.0]` — no silent clipping.
///
/// A no-op fast path skips the multiply when `gain_db` rounds to 0 dB
/// (avoids floating-point churn on the common default setting).
pub fn apply_mic_gain(samples: &mut [f32], gain_db: f32) {
    if gain_db == 0.0 || samples.is_empty() {
        return;
    }
    let linear = 10f32.powf(gain_db / 20.0);
    for s in samples.iter_mut() {
        *s = (*s * linear).clamp(-1.0, 1.0);
    }
}

/// Average channel-interleaved samples down to a single mono channel.
///
/// `samples` is a flat slice of frames where each frame contains `channels`
/// samples in channel order (the layout `cpal` hands us in input callbacks).
/// The output length is `samples.len() / channels`. Any trailing partial frame
/// is discarded — a partial frame from a healthy capture would mean the device
/// dropped data mid-frame, which is unrecoverable.
///
/// The mixdown is a straight arithmetic mean rather than a perceptual loudness
/// model. Whisper is robust to channel summing and the alternative (e.g.
/// ITU-R BS.775 downmix coefficients) would add complexity without measurably
/// improving transcription accuracy.
pub fn downmix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    // Fast paths: nothing to do for mono, and an empty buffer is a no-op.
    if channels <= 1 {
        return samples.to_vec();
    }
    if samples.is_empty() {
        return Vec::new();
    }

    let channels = channels as usize;
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(frames);
    let inv = 1.0 / channels as f32;
    for frame in samples.chunks_exact(channels) {
        let sum: f32 = frame.iter().sum();
        out.push(sum * inv);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_input_is_returned_unchanged() {
        let input = vec![0.1, -0.2, 0.3, -0.4];
        assert_eq!(downmix_to_mono(&input, 1), input);
    }

    #[test]
    fn zero_channels_is_treated_as_mono() {
        // Defensive: a 0-channel buffer should not panic. We treat it as mono
        // and return a copy so callers do not have to special-case the value
        // returned by a misbehaving driver.
        let input = vec![0.5_f32];
        assert_eq!(downmix_to_mono(&input, 0), input);
    }

    #[test]
    fn stereo_is_averaged_per_frame() {
        // Frame layout: [L0, R0, L1, R1, ...]
        let input = vec![1.0, -1.0, 0.5, 0.5, 0.2, 0.4];
        let out = downmix_to_mono(&input, 2);
        assert_eq!(out, vec![0.0, 0.5, 0.3]);
    }

    #[test]
    fn quad_is_averaged_per_frame() {
        let input = vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let out = downmix_to_mono(&input, 4);
        assert_eq!(out, vec![1.0, 0.0]);
    }

    #[test]
    fn trailing_partial_frame_is_dropped() {
        // 3 samples, 2 channels → 1 full frame (2 samples), 1 orphan sample dropped.
        let input = vec![0.4, 0.6, 0.9];
        let out = downmix_to_mono(&input, 2);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(downmix_to_mono(&[], 2).is_empty());
    }

    #[test]
    fn apply_mic_gain_zero_db_is_identity() {
        let original = vec![0.5, -0.5, 0.25];
        let mut samples = original.clone();
        apply_mic_gain(&mut samples, 0.0);
        assert_eq!(samples, original);
    }

    #[test]
    fn apply_mic_gain_positive_boosts_amplitude() {
        // +20 dB ≈ ×10 linear — a 0.05 sample becomes ~0.5.
        let mut samples = vec![0.05_f32];
        apply_mic_gain(&mut samples, 20.0);
        let expected = (0.05_f32 * 10.0).clamp(-1.0, 1.0);
        assert!((samples[0] - expected).abs() < 1e-5);
    }

    #[test]
    fn apply_mic_gain_clamps_at_unity() {
        // A large gain on a near-full-scale sample should not exceed ±1.0.
        let mut samples = vec![0.9_f32, -0.9_f32];
        apply_mic_gain(&mut samples, 20.0);
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[1], -1.0);
    }

    #[test]
    fn apply_mic_gain_negative_attenuates() {
        // –20 dB ≈ ×0.1 linear.
        let mut samples = vec![1.0_f32];
        apply_mic_gain(&mut samples, -20.0);
        let expected = (1.0_f32 * 0.1).clamp(-1.0, 1.0);
        assert!((samples[0] - expected).abs() < 1e-5);
    }

    #[test]
    fn apply_mic_gain_empty_slice_is_noop() {
        let mut samples: Vec<f32> = vec![];
        apply_mic_gain(&mut samples, 10.0); // must not panic
        assert!(samples.is_empty());
    }
}
