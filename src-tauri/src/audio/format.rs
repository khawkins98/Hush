//! Pure-logic PCM format helpers.
//!
//! This module is deliberately free of any OS or `cpal` dependency so it can
//! be unit-tested without an audio device. It currently exposes channel
//! downmixing; sample-rate conversion will land alongside the transcription
//! integration (TODO(#2)) once we know whether `whisper-rs` will accept a
//! native-rate buffer or whether we need to resample first.

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
}
